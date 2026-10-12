//! Vacuum TMz Yee solver. E is at integer time, H at the preceding half-step.
//! After a failed step this instance is stopped permanently; callers must keep
//! their last successfully published snapshot rather than publish partial data.
mod cpml;
mod geometry;
mod rms;

pub(crate) use geometry::PreparedGeometry;
use geometry::{add_source_cost, build_samples};

use crate::model::{EPS0, GridSpec, MU0, Scene};
use crate::sources::CompiledSource;
use crate::{Error, Result};
use cpml::Axis;
use rms::Rms;

const MAX_SOURCE_STEP_COST: usize = 1_000_000;

type Sample = (usize, f64, f64);

struct Excitation {
    id: u64,
    compiled: CompiledSource,
    samples: Vec<Sample>,
}

pub struct Solver {
    grid: GridSpec,
    ez: Vec<f64>,
    hx: Vec<f64>,
    hy: Vec<f64>,
    mask: Vec<bool>,
    x: Axis,
    y: Axis,
    psi_hx_y: Vec<f64>,
    psi_hy_x: Vec<f64>,
    psi_e_x: Vec<f64>,
    psi_e_y: Vec<f64>,
    sources: Vec<Excitation>,
    rms: Rms,
    steps: u64,
    failed: bool,
}

impl Solver {
    /// Compile a validated static scene, with zero fields and zero CPML memory.
    pub fn new(scene: &Scene) -> Result<Self> {
        let grid = scene.validate()?;
        let mask = scene.pec_mask(grid);
        let mut sources = Vec::new();
        let mut step_cost = 0usize;
        for source in &scene.sources {
            // Muting disables excitation, not validation. Keep Apply consistent
            // with scene persistence, which validates every source profile.
            let compiled = CompiledSource::new(source)?;
            if !source.enabled {
                continue;
            }
            let samples = build_samples(source, grid, &mask)?;
            step_cost = add_source_cost(step_cost, compiled.cost(), samples.len())?;
            sources.push(Excitation {
                id: source.id,
                compiled,
                samples,
            });
        }
        let len = grid.len();
        Ok(Self {
            grid,
            ez: vec![0.0; len],
            hx: vec![0.0; len],
            hy: vec![0.0; len],
            mask,
            x: Axis::new(grid.nx, grid)?,
            y: Axis::new(grid.ny, grid)?,
            psi_hx_y: vec![0.0; len],
            psi_hy_x: vec![0.0; len],
            psi_e_x: vec![0.0; len],
            psi_e_y: vec![0.0; len],
            sources,
            rms: Rms::new(len, scene.rms_window_s, grid.dt)?,
            steps: 0,
            failed: false,
        })
    }

    pub fn grid(&self) -> GridSpec {
        self.grid
    }
    pub fn ez(&self) -> &[f64] {
        &self.ez
    }
    pub fn mask(&self) -> &[bool] {
        &self.mask
    }
    /// Last complete nonoverlapping window; None until the first completion.
    pub fn rms(&self) -> Option<&[f64]> {
        self.rms.last()
    }
    pub fn rms_progress(&self) -> f64 {
        self.rms.progress()
    }
    pub fn time(&self) -> f64 {
        self.steps as f64 * self.grid.dt
    }
    pub fn steps(&self) -> u64 {
        self.steps
    }

    pub fn step(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::Numerical(
                "решатель остановлен после предыдущей ошибки; требуется Reset".into(),
            ));
        }
        let result = self.step_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn next_step_clock(&self) -> Result<(u64, f64)> {
        let g = self.grid;
        let next = self
            .steps
            .checked_add(1)
            .ok_or_else(|| Error::Numerical("переполнение счётчика физических шагов".into()))?;
        let now = self.time();
        let next_time = next as f64 * g.dt;
        let midpoint = now + 0.5 * g.dt;
        if !g.dt.is_finite()
            || g.dt <= 0.0
            || !next_time.is_finite()
            || !midpoint.is_finite()
            || next_time <= now
            || midpoint <= now
            || midpoint >= next_time
        {
            return Err(Error::Numerical(
                "шаг времени перестал быть конечным/различимым в f64".into(),
            ));
        }
        Ok((next, midpoint))
    }

    fn step_inner(&mut self) -> Result<()> {
        let g = self.grid;
        let (next, midpoint) = self.next_step_clock()?;
        let h_factor = g.dt / MU0;
        let e_factor = g.dt / EPS0;
        // Hx(i,j+1/2): no magnetic clamping at a PEC/vacuum interface.
        for j in 0..g.ny - 1 {
            for i in 0..g.nx {
                let k = g.index(i, j);
                let derivative = (self.ez[k + g.nx] - self.ez[k]) / g.dx;
                let stretched = self.y.magnetic[j].apply(derivative, &mut self.psi_hx_y[k]);
                self.hx[k] = finite(self.hx[k] - h_factor * stretched, "Hx", k)?;
            }
        }
        // Hy(i+1/2,j). Trailing elements of both arrays remain unused zero.
        for j in 0..g.ny {
            for i in 0..g.nx - 1 {
                let k = g.index(i, j);
                let derivative = (self.ez[k + 1] - self.ez[k]) / g.dx;
                let stretched = self.x.magnetic[i].apply(derivative, &mut self.psi_hy_x[k]);
                self.hy[k] = finite(self.hy[k] + h_factor * stretched, "Hy", k)?;
            }
        }
        for j in 1..g.ny - 1 {
            for i in 1..g.nx - 1 {
                let k = g.index(i, j);
                if self.mask[k] {
                    self.ez[k] = 0.0;
                    continue;
                }
                let dx_hy = (self.hy[k] - self.hy[k - 1]) / g.dx;
                let dy_hx = (self.hx[k] - self.hx[k - g.nx]) / g.dx;
                let sx = self.x.electric[i].apply(dx_hy, &mut self.psi_e_x[k]);
                let sy = self.y.electric[j].apply(dy_hx, &mut self.psi_e_y[k]);
                self.ez[k] = finite(self.ez[k] + e_factor * (sx - sy), "Ez", k)?;
            }
        }
        // Physical soft current Jz [A/m²], quadrature at t_(n+1/2).
        // A source contributes additively; it never overwrites the field.
        for source in &self.sources {
            for &(index, s, weight) in &source.samples {
                let jz = weight * source.compiled.value(midpoint, s)?;
                self.ez[index] = finite(self.ez[index] - e_factor * jz, "Ez/source", index)?;
            }
        }
        self.rms.push(&self.ez)?;
        self.steps = next;
        Ok(())
    }
}

#[inline]
fn finite(value: f64, component: &str, index: usize) -> Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Numerical(format!(
            "неконечное {component} в ячейке {index}"
        )))
    }
}

#[cfg(test)]
mod absorption_tests;
#[cfg(test)]
mod geometry_tests;
#[cfg(test)]
mod tests;
