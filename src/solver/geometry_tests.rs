use super::*;
use crate::model::{Domain, ObjectPose, Point, Probe, Profile, Reflector, Shape, Signal, Source};

#[test]
fn moving_a_reflector_releases_old_pec_and_next_step_uses_the_new_mask() {
    let previous = scene();
    let mut s = Solver::new(&previous).unwrap();
    for _ in 0..300 {
        s.step().unwrap();
    }
    assert!(s.ez.iter().any(|e| e.abs() > 1e-4));
    let old_mask = s.mask.clone();
    let candidate = previous
        .with_pose(&reflector_pose(Point { x: 0.085, y: 0.06 }))
        .unwrap();
    let prepared = s.prepare_geometry(&previous, &candidate).unwrap();
    let steps = s.steps;
    s.commit_geometry(prepared);
    assert_eq!(s.steps, steps);
    // Independently apply the local vacuum Yee equations at a released cell.
    // Select a former reflector cell, far from the source strip and CPML.
    let g = s.grid;
    let ch = g.dt / (MU0 * g.dx);
    let ce = g.dt / (EPS0 * g.dx);
    let mut probe = None;
    let mut expected: f64 = 0.0;
    for (k, &was_pec) in old_mask.iter().enumerate() {
        let p = g.point(k % g.nx, k / g.nx);
        if !was_pec || s.mask[k] || !(0.08..0.10).contains(&p.x) || !(0.03..0.05).contains(&p.y) {
            continue;
        }
        assert_eq!(s.ez[k], 0.0);
        let hx_top = s.hx[k] - ch * (s.ez[k + g.nx] - s.ez[k]);
        let hx_bottom = s.hx[k - g.nx] - ch * (s.ez[k] - s.ez[k - g.nx]);
        let hy_right = s.hy[k] + ch * (s.ez[k + 1] - s.ez[k]);
        let hy_left = s.hy[k - 1] + ch * (s.ez[k] - s.ez[k - 1]);
        let next_e = ce * (hy_right - hy_left - hx_top + hx_bottom);
        if next_e.abs() > expected.abs() {
            probe = Some(k);
            expected = next_e;
        }
    }
    let k = probe.expect("a released boundary cell with an incident wave");
    assert!(
        expected.abs() > 1e-5,
        "test must contain a nonzero incident/reflected wave"
    );
    s.step().unwrap();
    assert!(!s.mask[k]);
    assert!((s.ez[k] - expected).abs() < 1e-12 * expected.abs().max(1.0));
    assert_ne!(
        s.ez[k], 0.0,
        "the old PEC must not continue clamping released cells"
    );
}

#[test]
fn bent_source_pose_preserves_fields_but_changing_arm_length_is_not_a_pose() {
    let mut previous = scene();
    previous.sources[0].bend = Some(crate::model::GuideBend { length_m: 0.025 });
    let mut s = Solver::new(&previous).unwrap();
    for _ in 0..4 {
        s.step().unwrap();
    }
    s.psi_hx_y.fill(1e-8);
    s.psi_hy_x.fill(2e-8);
    s.psi_e_x.fill(3e-8);
    s.psi_e_y.fill(4e-8);
    let candidate = previous
        .with_pose(&ObjectPose::Source {
            id: 1,
            center: Point { x: 0.04, y: 0.04 },
            angle_deg: -90.0,
        })
        .unwrap();
    let mut bad = candidate.clone();
    bad.sources[0].bend.as_mut().unwrap().length_m = 0.03;
    assert!(s.prepare_geometry(&previous, &bad).is_err());
    let before_e = bits(s.ez());
    let before_other = other_fields(&s);
    let steps = s.steps;
    let prepared = s.prepare_geometry(&previous, &candidate).unwrap();
    s.commit_geometry(prepared);
    assert_eq!(other_fields(&s), before_other);
    assert_eq!(s.steps, steps);
    for (k, &old) in before_e.iter().enumerate() {
        assert_eq!(
            s.ez[k].to_bits(),
            if s.mask[k] { 0.0_f64.to_bits() } else { old }
        );
    }
    assert_eq!(s.mask, candidate.pec_mask(s.grid));
}

fn scene() -> Scene {
    let mut s = Scene {
        domain: Domain {
            width_m: 0.12,
            height_m: 0.08,
            cell_m: 0.001,
            cfl: 0.9,
            pml_cells: 20,
        },
        sources: vec![Source {
            center: Point { x: 0.035, y: 0.04 },
            length_m: 0.04,
            width_m: 0.018,
            strip_m: 0.004,
            source_offset_m: 0.010,
            ..Source::default()
        }],
        reflectors: vec![Reflector {
            id: 3,
            name: "PEC".into(),
            center: Point { x: 0.09, y: 0.04 },
            angle_deg: 0.0,
            shape: Shape::Rectangle {
                width_m: 0.006,
                height_m: 0.010,
            },
        }],
        probes: vec![Probe {
            id: 2,
            name: "Probe".into(),
            position: Point { x: 0.075, y: 0.04 },
        }],
        ..Scene::default()
    };
    s.rms_window_s = 2.5 * s.validate().unwrap().dt;
    s
}

#[test]
fn geometry_changes_preserve_an_exact_zero_state_without_active_currents() {
    let mut previous = scene();
    previous.sources[0].enabled = false;
    let mut s = Solver::new(&previous).unwrap();
    let candidate = previous
        .with_pose(&reflector_pose(Point { x: 0.08, y: 0.05 }))
        .unwrap();
    let prepared = s.prepare_geometry(&previous, &candidate).unwrap();
    s.commit_geometry(prepared);
    for _ in 0..5 {
        s.step().unwrap();
    }
    assert!(
        s.ez.iter()
            .chain(&s.hx)
            .chain(&s.hy)
            .chain(&s.psi_hx_y)
            .chain(&s.psi_hy_x)
            .chain(&s.psi_e_x)
            .chain(&s.psi_e_y)
            .all(|&value| value == 0.0)
    );
}

fn bits(values: &[f64]) -> Vec<u64> {
    values.iter().map(|v| v.to_bits()).collect()
}

// Includes H in metal and all CPML coefficients/memories; a field plot alone
// cannot establish that an interactive update preserved numerical state.
fn other_fields(s: &Solver) -> Vec<Vec<u64>> {
    let mut all = [
        &s.hx,
        &s.hy,
        &s.psi_hx_y,
        &s.psi_hy_x,
        &s.psi_e_x,
        &s.psi_e_y,
    ]
    .iter()
    .map(|a| bits(a))
    .collect::<Vec<_>>();
    for axis in [&s.x, &s.y] {
        for coefficients in [&axis.electric, &axis.magnetic] {
            all.push(
                coefficients
                    .iter()
                    .flat_map(|c| [c.inverse_kappa.to_bits(), c.a.to_bits(), c.b.to_bits()])
                    .collect(),
            );
        }
    }
    all
}

fn assert_same(a: &Solver, b: &Solver) {
    assert_eq!(bits(a.ez()), bits(b.ez()));
    assert_eq!(other_fields(a), other_fields(b));
    assert_eq!(a.mask, b.mask);
    assert_eq!(a.steps, b.steps);
    assert_eq!(a.time().to_bits(), b.time().to_bits());
    assert_eq!(a.failed, b.failed);
    assert_eq!(a.rms, b.rms);
    for (a, b) in a.sources.iter().zip(&b.sources) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.samples, b.samples);
    }
}

fn reflector_pose(center: Point) -> ObjectPose {
    ObjectPose::Reflector {
        id: 3,
        center,
        angle_deg: 90.0,
    }
}

#[test]
fn geometry_commit_preserves_all_staggered_state_and_resets_only_rms() {
    let previous = scene();
    let mut s = Solver::new(&previous).unwrap();
    for _ in 0..4 {
        s.step().unwrap();
    }
    assert!(s.rms().is_some());
    assert!(s.rms_progress() > 0.0);
    // Arbitrary finite state is deliberate: exercise preservation even where
    // naturally generated fields in this short run would still be zero.
    for k in 0..s.grid.len() {
        if !s.mask[k] {
            s.ez[k] = 1e-3 + (k % 17) as f64 * 1e-5;
        }
        s.hx[k] = 1e-5 + (k % 13) as f64 * 1e-7;
        s.hy[k] = -2e-5;
        s.psi_hx_y[k] = 3e-8;
        s.psi_hy_x[k] = -4e-8;
        s.psi_e_x[k] = 5e-9;
        s.psi_e_y[k] = -6e-9;
    }
    let before_e = bits(s.ez());
    let before_other = other_fields(&s);
    let before_rms = s.rms.clone();
    let before_mask = s.mask.clone();
    let before_steps = s.steps;
    let before_time = s.time().to_bits();
    let compiled_address = &s.sources[0].compiled as *const CompiledSource;
    let candidate = previous
        .with_pose(&reflector_pose(Point { x: 0.075, y: 0.05 }))
        .unwrap();
    let new_mask = candidate.pec_mask(s.grid);
    let prepared = s.prepare_geometry(&previous, &candidate).unwrap();
    assert_eq!(bits(s.ez()), before_e);
    assert_eq!(other_fields(&s), before_other);
    assert_eq!(s.rms, before_rms);
    assert_eq!(s.mask, before_mask);
    s.commit_geometry(prepared);
    for (k, &old) in before_e.iter().enumerate() {
        assert_eq!(
            s.ez[k].to_bits(),
            if new_mask[k] { 0.0_f64.to_bits() } else { old }
        );
    }
    assert_eq!(other_fields(&s), before_other);
    assert_eq!(s.steps, before_steps);
    assert_eq!(s.time().to_bits(), before_time);
    assert_eq!(s.mask, new_mask);
    assert!(s.rms().is_none());
    assert_eq!(s.rms_progress(), 0.0);
    assert_eq!(
        compiled_address,
        &s.sources[0].compiled as *const CompiledSource
    );
    let (x, y) = s.grid.nearest(Point { x: 0.075, y: 0.05 });
    assert!(s.mask[s.grid.index(x, y)]);
    let (x, y) = s.grid.nearest(Point { x: 0.09, y: 0.04 });
    let k = s.grid.index(x, y);
    assert!(!s.mask[k]);
    assert_eq!(s.ez[k], 0.0);
    assert_ne!(
        s.hx[k], 0.0,
        "H inside the former conductor must be retained"
    );
}

#[test]
fn no_op_and_probe_only_defensively_preserve_full_solver_state() {
    let previous = scene();
    for candidate in [
        previous.clone(),
        previous
            .with_pose(&ObjectPose::Probe {
                id: 2,
                position: Point { x: 0.08, y: 0.045 },
            })
            .unwrap(),
    ] {
        let mut s = Solver::new(&previous).unwrap();
        let mut control = Solver::new(&previous).unwrap();
        for _ in 0..4 {
            s.step().unwrap();
            control.step().unwrap();
        }
        let prepared = s.prepare_geometry(&previous, &candidate).unwrap();
        s.commit_geometry(prepared);
        assert_same(&s, &control);
        s.step().unwrap();
        control.step().unwrap();
        assert_same(&s, &control);
    }
}

#[test]
fn rejected_geometry_is_atomic_and_next_step_matches_control() {
    let previous = scene();
    let mut s = Solver::new(&previous).unwrap();
    let mut control = Solver::new(&previous).unwrap();
    for _ in 0..4 {
        s.step().unwrap();
        control.step().unwrap();
    }
    let mut outside = previous.clone();
    outside.reflectors[0].center.x = -1.0;
    let mut nan = previous.clone();
    nan.reflectors[0].center.y = f64::NAN;
    let collision = previous
        .with_pose(&reflector_pose(Point { x: 0.025, y: 0.04 }))
        .unwrap();
    for candidate in [outside, nan, collision] {
        assert!(s.prepare_geometry(&previous, &candidate).is_err());
        assert_same(&s, &control);
    }
    s.step().unwrap();
    control.step().unwrap();
    assert_same(&s, &control);
}

#[test]
fn source_pose_rebuilds_weights_without_restarting_global_time() {
    let mut previous = scene();
    previous.sources[0].signal = Signal::Formula {
        expression: "t*1e12".into(),
    };
    previous.sources[0].aperture = Profile::Uniform;
    let mut s = Solver::new(&previous).unwrap();
    let mut control = Solver::new(&previous).unwrap();
    for _ in 0..6 {
        s.step().unwrap();
        control.step().unwrap();
    }
    let candidate = previous
        .with_pose(&ObjectPose::Source {
            id: 1,
            center: Point { x: 0.06, y: 0.045 },
            angle_deg: 90.0,
        })
        .unwrap();
    let compiled_address = &s.sources[0].compiled as *const CompiledSource;
    let old_indices = s.sources[0].samples.iter().map(|v| v.0).collect::<Vec<_>>();
    let prepared = s.prepare_geometry(&previous, &candidate).unwrap();
    s.commit_geometry(prepared);
    let prepared = control.prepare_geometry(&previous, &candidate).unwrap();
    control.commit_geometry(prepared);
    assert_eq!(
        compiled_address,
        &s.sources[0].compiled as *const CompiledSource
    );
    assert_ne!(
        old_indices,
        s.sources[0].samples.iter().map(|v| v.0).collect::<Vec<_>>()
    );
    for &(k, coordinate, weight) in &s.sources[0].samples {
        assert_eq!(
            candidate.sources[0].excitation_weight(s.grid.point(k % s.grid.nx, k / s.grid.nx)),
            Some((coordinate, weight))
        );
    }
    let mut silent = candidate.sources[0].clone();
    silent.current_a_m2 = 0.0;
    control.sources[0].compiled = CompiledSource::new(&silent).unwrap();
    let midpoint = s.time() + 0.5 * s.grid.dt;
    let dt = s.grid.dt;
    assert!(midpoint > 6.0 * dt);
    s.step().unwrap();
    control.step().unwrap();
    for &(k, _, weight) in &s.sources[0].samples {
        let expected = -dt / EPS0 * weight * midpoint * 1e12;
        assert!(((s.ez[k] - control.ez[k]) / expected - 1.0).abs() < 1e-12);
    }
}

#[test]
fn moved_source_domain_error_is_detected_before_mutating_fields() {
    let mut previous = scene();
    previous.sources[0].center.y = 0.0405;
    previous.sources[0].signal = Signal::Formula {
        expression: "t*1e12/s".into(),
    };
    let mut s = Solver::new(&previous).unwrap();
    let mut control = Solver::new(&previous).unwrap();
    for _ in 0..4 {
        s.step().unwrap();
        control.step().unwrap();
    }
    let candidate = previous
        .with_pose(&ObjectPose::Source {
            id: 1,
            center: Point { x: 0.035, y: 0.04 },
            angle_deg: 0.0,
        })
        .unwrap();
    assert!(s.prepare_geometry(&previous, &candidate).is_err());
    assert_same(&s, &control);
    s.step().unwrap();
    control.step().unwrap();
    assert_same(&s, &control);
}

#[test]
fn geometry_preparation_rejects_cold_edits_wrong_cache_ids_and_failed_solver() {
    let previous = scene();
    let mut s = Solver::new(&previous).unwrap();
    let edits: [fn(&mut Scene); 9] = [
        |c| c.domain.cfl = 0.8,
        |c| c.domain.pml_cells = 21,
        |c| c.rms_window_s *= 2.0,
        |c| c.sources[0].enabled = false,
        |c| c.sources[0].current_a_m2 = 2.0,
        |c| c.sources[0].width_m = 0.02,
        |c| {
            c.sources[0].signal = Signal::Formula {
                expression: "1".into(),
            }
        },
        |c| c.reflectors[0].shape = Shape::Circle { radius_m: 0.004 },
        |c| c.probes[0].name = "Renamed".into(),
    ];
    for edit in edits {
        let mut candidate = previous.clone();
        edit(&mut candidate);
        assert!(s.prepare_geometry(&previous, &candidate).is_err());
    }
    let mut wrong_previous = previous.clone();
    wrong_previous.sources[0].id = 77;
    let mut candidate = wrong_previous.clone();
    candidate.sources[0].center.x += 0.001;
    assert!(s.prepare_geometry(&wrong_previous, &candidate).is_err());
    s.failed = true;
    assert!(s.prepare_geometry(&previous, &previous).is_err());
}
