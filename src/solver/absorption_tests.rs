//! Bounded acceptance experiment; run explicitly in release mode.
use super::*;
use crate::model::{C0, Domain};
use std::f64::consts::PI;

fn scene(cells_x: usize, cells_y: usize, pml: usize) -> Scene {
    Scene {
        domain: Domain {
            width_m: cells_x as f64 * 0.0008,
            height_m: cells_y as f64 * 0.0008,
            cell_m: 0.0008,
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

// Compact Gaussian-modulated finite-waist packet. The taper is below 1.3e-4
// of the peak, keeping all nonzero initial fields out of CPML. A finite waist
// has an angular spread; 0/45 degrees denote its carrier wave-vector.
fn packet(x: f64, y: f64, t: f64, angle: f64, frequency: f64) -> f64 {
    let (sn, cs) = angle.sin_cos();
    let u = cs * x + sn * y - C0 * t;
    let v = -sn * x + cs * y;
    let radius = u.hypot(v) / 0.0008;
    if radius >= 100.0 {
        return 0.0;
    }
    let taper = if radius <= 90.0 {
        1.0
    } else {
        0.5 + 0.5 * (PI * (radius - 90.0) / 10.0).cos()
    };
    (-(u * u + v * v) / 0.024_f64.powi(2)).exp() * (2.0 * PI * frequency / C0 * u).sin() * taper
}

fn initialize(s: &mut Solver, angle: f64, frequency: f64) {
    let g = s.grid;
    let center = (g.nx - 1) as f64 * g.dx / 2.0;
    let (sn, cs) = angle.sin_cos();
    for j in 1..g.ny - 1 {
        for i in 1..g.nx - 1 {
            let x = i as f64 * g.dx - center;
            let y = j as f64 * g.dx - center;
            let k = g.index(i, j);
            s.ez[k] = packet(x, y, 0.0, angle, frequency);
            s.hx[k] = sn / (MU0 * C0) * packet(x, y + 0.5 * g.dx, -0.5 * g.dt, angle, frequency);
            s.hy[k] = -cs / (MU0 * C0) * packet(x + 0.5 * g.dx, y, -0.5 * g.dt, angle, frequency);
        }
    }
}

#[test]
#[ignore = "6 bounded CPML cases: run cargo test --release --lib cpml_pulse_acceptance -- --ignored --nocapture"]
fn cpml_pulse_acceptance() {
    let mut failures = Vec::new();
    for frequency in [8e9, 10e9, 12e9] {
        for angle in [0.0, PI / 4.0] {
            let mut pml = Solver::new(&scene(240, 360, 20)).unwrap();
            let mut vacuum = Solver::new(&scene(700, 820, 0)).unwrap();
            let mut pec_control = Solver::new(&scene(240, 360, 20)).unwrap();
            let vacuum_coefficients = crate::model::GridSpec {
                pml: 0,
                ..pec_control.grid
            };
            pec_control.x = Axis::new(pec_control.grid.nx, vacuum_coefficients).unwrap();
            pec_control.y = Axis::new(pec_control.grid.ny, vacuum_coefficients).unwrap();
            initialize(&mut pml, angle, frequency);
            initialize(&mut vacuum, angle, frequency);
            initialize(&mut pec_control, angle, frequency);
            // At 45 degrees the ray hits the RIGHT wall, not a corner.
            // Physical cell coordinates: source (120,120), incidence probe
            // (210,210), wall (240,240), reflected probe (210,270).
            // Its reflected-packet center returns at about step 333, well
            // inside 480 steps; monitoring only the launch point would miss it.
            let (incident_center, reflected_center, split) = if angle == 0.0 {
                ((170, 140), (170, 140), 150)
            } else {
                ((230, 230), (230, 290), 260)
            };
            let pairs: Vec<_> = (-3..=3)
                .flat_map(|y| (-3..=3).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let ix = (incident_center.0 + x + 210) as usize;
                    let iy = (incident_center.1 + y + 210) as usize;
                    let rx = (reflected_center.0 + x) as usize;
                    let ry = (reflected_center.1 + y) as usize;
                    (
                        vacuum.grid.index(ix, iy),
                        pml.grid.index(rx, ry),
                        vacuum.grid.index(rx + 210, ry + 210),
                    )
                })
                .collect();
            // Initial E/H support is inside a radius of 102 cells. The
            // shortest return from a reference PEC wall to any monitor is
            // (350-102)+(350-93)=505 stencil cells. Each Yee E update propagates
            // dependence at most one cell. No reference-wall return can affect
            // these monitors within 480 steps (strict discrete causality).
            let reference_half_width = (vacuum.grid.nx - 1) / 2;
            assert!(480 < (reference_half_width - 102) + (reference_half_width - 93));
            let mut incident: f64 = 0.0;
            let mut reflected: f64 = 0.0;
            let mut early_error: f64 = 0.0;
            let mut control_reflection: f64 = 0.0;
            for n in 0..480 {
                pml.step().unwrap();
                vacuum.step().unwrap();
                pec_control.step().unwrap();
                for &(incident_index, a, b) in &pairs {
                    if n < split {
                        incident = incident.max(vacuum.ez[incident_index].abs());
                    }
                    let error = (pml.ez[a] - vacuum.ez[b]).abs();
                    if n < 100 {
                        early_error = early_error.max(error);
                    }
                    if n >= split {
                        reflected = reflected.max(error);
                        control_reflection =
                            control_reflection.max((pec_control.ez[a] - vacuum.ez[b]).abs());
                    }
                }
            }
            assert!(incident > 0.2, "incident normalization must not be vacuous");
            assert!(
                early_error / incident < 1e-5,
                "mismatched initial fields: {early_error}"
            );
            assert!(
                control_reflection / incident > 0.1,
                "monitor must detect the specular PEC return: {}",
                control_reflection / incident
            );
            let reflection = reflected / incident;
            eprintln!(
                "CPML f={} GHz angle={} deg |scattered|/|incident|={reflection:.6e}",
                frequency / 1e9,
                angle.to_degrees()
            );
            if reflection > 1e-3 {
                failures.push((frequency, angle, reflection));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "CPML amplitude threshold 1e-3 failed: {failures:?}"
    );
}
