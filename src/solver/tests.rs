use super::*;
use crate::model::{C0, Domain};
use std::f64::consts::PI;

fn empty_scene(width: f64, height: f64, dx: f64, pml: usize) -> Scene {
    Scene {
        domain: Domain {
            width_m: width,
            height_m: height,
            cell_m: dx,
            cfl: 0.9,
            pml_cells: pml,
        },
        sources: vec![],
        reflectors: vec![],
        probes: vec![],
        rms_window_s: 1e-10,
        ..Scene::default()
    }
}

#[test]
#[ignore = "bounded release baseline, not a GUI FPS benchmark"]
fn preset_step_baseline() {
    let started = std::time::Instant::now();
    let mut s = Solver::new(&Scene::default()).unwrap();
    let initialization = started.elapsed();
    // Warm code/data once; time stepping includes sources, finite checks and RMS.
    s.step().unwrap();
    let started = std::time::Instant::now();
    let mut count = 0;
    while count < 500 && started.elapsed().as_secs_f64() < 5.0 {
        s.step().unwrap();
        count += 1;
    }
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "baseline: {}x{}, steps={count}, seconds={elapsed:.6}, steps/s={:.3}, physical ns/s={:.3}, initialization={initialization:?}, threads=1, arch={}, os={}",
        s.grid.nx,
        s.grid.ny,
        count as f64 / elapsed,
        count as f64 * s.grid.dt / elapsed * 1e9,
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    std::hint::black_box(s.ez());
    assert!(count > 0);
}

// E^0 is a standing-wave maximum. H^{-1/2} is NOT zero: this
// discrete half-step initialization yields exactly cos(Omega*n*dt).
fn cavity(dx: f64) -> Solver {
    let mut s = Solver::new(&empty_scene(0.06, 0.04, dx, 0)).unwrap();
    let g = s.grid;
    for j in 1..g.ny - 1 {
        for i in 1..g.nx - 1 {
            s.ez[g.index(i, j)] = (PI * i as f64 / (g.nx - 1) as f64).sin()
                * (PI * j as f64 / (g.ny - 1) as f64).sin();
        }
    }
    for j in 0..g.ny - 1 {
        for i in 0..g.nx {
            let k = g.index(i, j);
            s.hx[k] = g.dt / (2.0 * MU0 * g.dx) * (s.ez[k + g.nx] - s.ez[k]);
        }
    }
    for j in 0..g.ny {
        for i in 0..g.nx - 1 {
            let k = g.index(i, j);
            s.hy[k] = -g.dt / (2.0 * MU0 * g.dx) * (s.ez[k + 1] - s.ez[k]);
        }
    }
    s
}

#[test]
fn zero_is_exact_with_and_without_cpml() {
    for pml in [0, 20] {
        let mut s = Solver::new(&empty_scene(0.04, 0.04, 0.001, pml)).unwrap();
        for _ in 0..120 {
            s.step().unwrap();
        }
        assert!(s.ez().iter().chain(&s.hx).chain(&s.hy).all(|&v| v == 0.0));
        assert!(s.rms().unwrap().iter().all(|&v| v == 0.0));
        assert_eq!(s.steps(), 120);
    }
}

#[test]
fn cavity_frequency_field_and_discrete_dispersion() {
    let mut s = cavity(0.002);
    let g = s.grid;
    let kx = PI / 0.06;
    let ky = PI / 0.04;
    let omega = C0 * kx.hypot(ky);
    let discrete = 2.0 / g.dt
        * (C0 * g.dt / g.dx
            * ((kx * g.dx / 2.0).sin().powi(2) + (ky * g.dx / 2.0).sin().powi(2)).sqrt())
        .asin();
    let initial = s.ez.clone();
    let center = g.index((g.nx - 1) / 2, (g.ny - 1) / 2);
    s.step().unwrap();
    let measured = (s.ez[center] / initial[center]).acos() / g.dt;
    eprintln!(
        "cavity: f_exact={:.9} GHz, f_measured={:.9} GHz, relative_error={:.9e}",
        omega / (2.0 * PI * 1e9),
        measured / (2.0 * PI * 1e9),
        (measured / omega - 1.0).abs()
    );
    assert!((measured / omega - 1.0).abs() < 0.01);
    assert!((measured / discrete - 1.0).abs() < 1e-11);
    for _ in 1..150 {
        s.step().unwrap();
    }
    let factor = (discrete * s.time()).cos();
    for (&actual, &shape) in s.ez.iter().zip(&initial) {
        assert!((actual - factor * shape).abs() < 2e-12);
    }
}

#[test]
fn cavity_refinement_is_second_order_against_continuum() {
    let omega = C0 * (PI / 0.06).hypot(PI / 0.04);
    let mut errors = vec![];
    for refinement in [1, 2, 4] {
        let mut s = cavity(0.002 / refinement as f64);
        for _ in 0..30 * refinement {
            s.step().unwrap();
        }
        let center = s.grid.index((s.grid.nx - 1) / 2, (s.grid.ny - 1) / 2);
        errors.push((s.ez[center] - (omega * s.time()).cos()).abs());
    }
    for pair in errors.windows(2) {
        let order = (pair[0] / pair[1]).log2();
        eprintln!("cavity refinement: errors={errors:?}, order={order:.9}");
        assert!(
            (1.8..=2.2).contains(&order),
            "errors={errors:?}, order={order}"
        );
    }
}

#[test]
fn staggered_energy_is_conserved_and_wall_h_is_not_clamped() {
    let mut s = cavity(0.002);
    let mut first = None;
    for _ in 0..200 {
        let previous = s.ez.clone();
        s.step().unwrap();
        // Exact lossless leapfrog invariant, evaluated at n+1/2.
        let energy = 0.5
            * s.grid.dx.powi(2)
            * (EPS0 * previous.iter().zip(&s.ez).map(|(a, b)| a * b).sum::<f64>()
                + MU0 * s.hx.iter().chain(&s.hy).map(|h| h * h).sum::<f64>());
        let reference = *first.get_or_insert(energy);
        assert!((energy / reference - 1.0).abs() < 1e-11);
        assert!(s.ez.iter().zip(&s.mask).all(|(&e, &pec)| !pec || e == 0.0));
    }
    assert!(s.hx.iter().any(|h| h.abs() > 1e-5));
    assert!(s.hx[s.grid.nx / 2].abs() > 1e-6);
}

#[test]
fn rms_is_scaled_nonoverlapping_and_keeps_last_complete_window() {
    let mut r = Rms::new(2, 3.0, 1.0).unwrap();
    r.push(&[3.0, 1e300]).unwrap();
    assert!(r.last().is_none());
    assert!((r.progress() - 1.0 / 3.0).abs() < 1e-15);
    r.push(&[4.0, -1e300]).unwrap();
    r.push(&[0.0, 1e300]).unwrap();
    let last = r.last().unwrap();
    assert!((last[0] - (25.0_f64 / 3.0).sqrt()).abs() < 1e-14);
    assert!((last[1] / 1e300 - 1.0).abs() < 1e-14);
    r.push(&[0.0, 0.0]).unwrap();
    assert_eq!(r.last().unwrap()[1], 1e300);
}

#[test]
fn nonfinite_update_and_exhausted_time_are_errors() {
    let mut s = cavity(0.002);
    let k = s.grid.index(2, 2);
    s.ez[k] = f64::MAX;
    assert!(s.step().is_err());
    assert!(s.step().is_err(), "failed solver must remain stopped");
    let mut s = cavity(0.002);
    s.steps = u64::MAX;
    assert!(s.step().is_err());
}

#[test]
fn pec_reflection_has_opposite_electric_sign() {
    let mut closed = Solver::new(&empty_scene(0.12, 0.04, 0.001, 0)).unwrap();
    let mut reference = Solver::new(&empty_scene(0.36, 0.04, 0.001, 0)).unwrap();
    let pulse = |x: f64, y: f64| (-((x - 0.06) / 0.008).powi(2)).exp() * (PI * y / 0.04).sin();
    for solver in [&mut closed, &mut reference] {
        let g = solver.grid;
        for j in 1..g.ny - 1 {
            for i in 1..g.nx - 1 {
                let k = g.index(i, j);
                solver.ez[k] = pulse(i as f64 * g.dx, j as f64 * g.dx);
                solver.hy[k] =
                    -pulse((i as f64 + 0.5) * g.dx + C0 * g.dt / 2.0, j as f64 * g.dx) / (MU0 * C0);
            }
        }
    }
    let mut reflected_peak: f64 = 0.0;
    let mut image_error: f64 = 0.0;
    for n in 0..180 {
        closed.step().unwrap();
        reference.step().unwrap();
        if n > 90 {
            let at = closed.grid.index(100, 20);
            let direct = reference.ez[reference.grid.index(100, 20)];
            let image = reference.ez[reference.grid.index(140, 20)];
            let reflected = closed.ez[at] - direct;
            image_error = image_error.max((reflected + image).abs());
            reflected_peak = reflected_peak.max(reflected.abs());
        }
    }
    assert!(reflected_peak > 0.3);
    assert!(
        image_error / reflected_peak < 1e-7,
        "PEC odd-image error={image_error}"
    );
    assert!(
        closed
            .ez
            .iter()
            .zip(&closed.mask)
            .all(|(&e, &m)| !m || e == 0.0)
    );
}

#[test]
fn guided_mode_above_and_below_cutoff() {
    let a = 0.022;
    let cutoff = C0 / (2.0 * a);
    assert!((cutoff / 1e9 - 6.8134649545).abs() < 1e-8);
    for f in [5e9, 10e9] {
        let mut s = Solver::new(&empty_scene(0.18, a, 0.001, 20)).unwrap();
        let g = s.grid;
        // Infinite parallel PEC walls continued through the axial CPML.
        for j in 0..g.ny {
            if j <= g.pml || j >= g.ny - 1 - g.pml {
                for i in 0..g.nx {
                    s.mask[g.index(i, j)] = true;
                }
            }
        }
        let positions = [g.pml + 60, g.pml + 70];
        let y = g.pml + 11;
        let mut phasors = [[0.0; 2]; 2];
        let period_steps = (1.0 / (f * g.dt)).round() as usize;
        let end = 2400;
        let begin = end - 12 * period_steps;
        for n in 0..end {
            s.step().unwrap();
            let phase = 2.0 * PI * f * s.time();
            let ramp = (s.time() / (8.0 / f)).min(1.0);
            let drive = phase.sin() * (0.5 - 0.5 * (PI * ramp).cos()) * 0.03;
            for j in g.pml + 1..g.ny - 1 - g.pml {
                s.ez[g.index(g.pml + 20, j)] += drive * (PI * (j - g.pml) as f64 / 22.0).sin();
            }
            if n >= begin {
                for (p, x) in phasors.iter_mut().zip(positions) {
                    p[0] += s.ez[g.index(x, y)] * phase.cos();
                    p[1] += s.ez[g.index(x, y)] * phase.sin();
                }
            }
        }
        if f > cutoff {
            let beta = ((2.0 * PI * f / C0).powi(2) - (PI / a).powi(2)).sqrt();
            let phase = (phasors[1][1] * phasors[0][0] - phasors[1][0] * phasors[0][1])
                .atan2(phasors[1][0] * phasors[0][0] + phasors[1][1] * phasors[0][1]);
            eprintln!(
                "guide above cutoff: beta_exact={beta:.9} rad/m, beta_measured={:.9}, relative_error={:.6e}",
                phase.abs() / 0.01,
                (phase.abs() / (beta * 0.01) - 1.0).abs()
            );
            assert!(
                (phase.abs() / (beta * 0.01) - 1.0).abs() < 0.02,
                "beta phase={phase}, target={}",
                beta * 0.01
            );
        } else {
            let gamma = ((PI / a).powi(2) - (2.0 * PI * f / C0).powi(2)).sqrt();
            let ratio = phasors[1][0].hypot(phasors[1][1]) / phasors[0][0].hypot(phasors[0][1]);
            eprintln!(
                "guide below cutoff: measured ratio={ratio:.9}, exact={:.9}, relative_error={:.6e}",
                (-gamma * 0.01).exp(),
                (ratio / (-gamma * 0.01).exp() - 1.0).abs()
            );
            assert!(
                (ratio / (-gamma * 0.01).exp() - 1.0).abs() < 0.05,
                "evanescent ratio={ratio}"
            );
        }
    }
}
