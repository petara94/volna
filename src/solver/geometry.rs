//! Transactional pose-only updates. This edits a static PEC constraint without
//! transporting fields and is not a model of moving metal or Doppler physics.
use super::{MAX_SOURCE_STEP_COST, Sample, Solver};
use crate::model::{GridSpec, Scene, Source};
use crate::{Error, Result};

/// Opaque prepared work, to be committed by the owning worker to the SAME solver
/// before another geometry transaction/cold replacement. Preparation is immutable;
/// commit has no fallible computation or allocation.
pub(crate) struct PreparedGeometry {
    mask: Option<Vec<bool>>,
    sample_updates: Vec<(usize, Vec<Sample>)>,
}

pub(super) fn build_samples(source: &Source, grid: GridSpec, mask: &[bool]) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for j in grid.pml..grid.ny - grid.pml {
        for i in grid.pml..grid.nx - grid.pml {
            if let Some((s, weight)) = source.excitation_weight(grid.point(i, j)) {
                let index = grid.index(i, j);
                if mask[index] {
                    return Err(Error::Invalid(format!(
                        "источник {} пересекает PEC в ({i}, {j})",
                        source.id
                    )));
                }
                samples.push((index, s, weight));
            }
        }
    }
    if samples.is_empty() {
        return Err(Error::Invalid(format!(
            "полоса источника {} не содержит узлов Ez",
            source.id
        )));
    }
    Ok(samples)
}

pub(super) fn add_source_cost(total: usize, cost: usize, sample_count: usize) -> Result<usize> {
    let total = cost
        .max(1)
        .checked_mul(sample_count)
        .and_then(|c| total.checked_add(c))
        .ok_or_else(|| Error::Invalid("переполнение бюджета источников".into()))?;
    if total > MAX_SOURCE_STEP_COST {
        return Err(Error::Invalid(format!(
            "источники требуют {total} операций/шаг; предел {MAX_SOURCE_STEP_COST}"
        )));
    }
    Ok(total)
}

fn same_grid(a: GridSpec, b: GridSpec) -> bool {
    a.nx == b.nx
        && a.ny == b.ny
        && a.pml == b.pml
        && a.dx.to_bits() == b.dx.to_bits()
        && a.dt.to_bits() == b.dt.to_bits()
}

fn require_pose_only(previous: &Scene, candidate: &Scene) -> Result<()> {
    let invalid = || {
        Error::Invalid("live-изменение допускает только координаты/угол существующих объектов; остальные настройки требуют Apply/Reset".into())
    };
    if previous.sources.len() != candidate.sources.len()
        || previous.reflectors.len() != candidate.reflectors.len()
        || previous.probes.len() != candidate.probes.len()
    {
        return Err(invalid());
    }
    // Remove only the permitted differences, then compare every remaining
    // field. New model fields therefore cannot silently bypass this contract.
    let mut unposed = candidate.clone();
    for (old, new) in previous.sources.iter().zip(&mut unposed.sources) {
        if old.id != new.id {
            return Err(invalid());
        }
        new.center = old.center;
        new.angle_deg = old.angle_deg;
    }
    for (old, new) in previous.reflectors.iter().zip(&mut unposed.reflectors) {
        if old.id != new.id {
            return Err(invalid());
        }
        new.center = old.center;
        new.angle_deg = old.angle_deg;
    }
    for (old, new) in previous.probes.iter().zip(&mut unposed.probes) {
        if old.id != new.id {
            return Err(invalid());
        }
        new.position = old.position;
    }
    if &unposed != previous {
        return Err(invalid());
    }
    Ok(())
}

impl Solver {
    pub(crate) fn prepare_geometry(
        &self,
        previous: &Scene,
        candidate: &Scene,
    ) -> Result<PreparedGeometry> {
        if self.failed {
            return Err(Error::Numerical(
                "после ошибки расчёта изменение позиции не разрешено; требуется Reset".into(),
            ));
        }
        let previous_grid = previous.validate()?;
        let candidate_grid = candidate.validate()?;
        if !same_grid(self.grid, previous_grid) || !same_grid(self.grid, candidate_grid) {
            return Err(Error::Invalid(
                "live-изменение не может менять сетку, CFL или CPML".into(),
            ));
        }
        require_pose_only(previous, candidate)?;
        if self.sources.iter().map(|s| s.id).ne(previous
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id))
        {
            return Err(Error::Invalid(
                "предыдущая сцена не соответствует ID активных источников решателя".into(),
            ));
        }
        // Runtime bypasses the solver for probes/no-ops; keep this defensive
        // guarantee here as well, including preservation of the RMS window.
        if previous.sources == candidate.sources && previous.reflectors == candidate.reflectors {
            return Ok(PreparedGeometry {
                mask: None,
                sample_updates: Vec::new(),
            });
        }
        let midpoint = self.next_step_clock()?.1;
        let mask = candidate.pec_mask(self.grid);
        let mut sample_updates = Vec::new();
        let mut total_cost = 0;
        let old_sources = previous.sources.iter().filter(|s| s.enabled);
        let new_sources = candidate.sources.iter().filter(|s| s.enabled);
        for (index, ((cached, old), new)) in self
            .sources
            .iter()
            .zip(old_sources)
            .zip(new_sources)
            .enumerate()
        {
            if old.center != new.center || old.angle_deg != new.angle_deg {
                let samples = build_samples(new, self.grid, &mask)?;
                total_cost = add_source_cost(total_cost, cached.compiled.cost(), samples.len())?;
                // Same compiled expression and global clock, but new aperture
                // coordinates. This catches the NEXT midpoint's domain errors,
                // not every possible error at an arbitrary future time.
                for &(_, s, _) in &samples {
                    cached.compiled.value(midpoint, s)?;
                }
                sample_updates.push((index, samples));
            } else {
                for &(k, _, _) in &cached.samples {
                    if mask[k] {
                        return Err(Error::Invalid(format!(
                            "новая геометрия пересекает полосу источника {}",
                            cached.id
                        )));
                    }
                }
                total_cost =
                    add_source_cost(total_cost, cached.compiled.cost(), cached.samples.len())?;
            }
        }
        Ok(PreparedGeometry {
            mask: Some(mask),
            sample_updates,
        })
    }

    pub(crate) fn commit_geometry(&mut self, prepared: PreparedGeometry) {
        let Some(mask) = prepared.mask else {
            return;
        };
        for (index, samples) in prepared.sample_updates {
            self.sources[index].samples = samples;
        }
        for (value, &pec) in self.ez.iter_mut().zip(&mask) {
            if pec {
                *value = 0.0;
            }
        }
        self.mask = mask;
        self.rms.reset();
        // Hx/Hy, all four psi arrays, CPML coefficients, compiled sources,
        // grid, steps, failed flag and every non-PEC Ez bit are untouched.
    }
}
