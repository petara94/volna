use volna::model::{EPS0, Point, Profile, Scene, Signal, Source};
use volna::solver::Solver;

#[test]
fn ribs_produce_a_measurable_scattered_field_in_the_shared_region() {
    let demo = Scene::interference_demo();
    let mut unobstructed = demo.clone();
    unobstructed.reflectors.clear();
    let mut with_ribs = Solver::new(&demo).unwrap();
    let mut reference = Solver::new(&unobstructed).unwrap();
    let grid = with_ribs.grid();
    let indices = demo
        .probes
        .iter()
        .map(|p| {
            let (i, j) = grid.nearest(p.position);
            grid.index(i, j)
        })
        .collect::<Vec<_>>();
    let mut incident_peak: f64 = 0.0;
    let mut scattered_peak: f64 = 0.0;
    for step in 0..1000 {
        with_ribs.step().unwrap();
        reference.step().unwrap();
        if step >= 600 {
            for &k in &indices {
                incident_peak = incident_peak.max(reference.ez()[k].abs());
                scattered_peak = scattered_peak.max((with_ribs.ez()[k] - reference.ez()[k]).abs());
            }
        }
    }
    assert!(incident_peak > 1e-4);
    assert!(
        scattered_peak > 0.02 * incident_peak,
        "incident={incident_peak}, scattered={scattered_peak}"
    );
    eprintln!(
        "interference demo: reference peak={incident_peak:.6e} V/m, scattered peak={scattered_peak:.6e} V/m (not a power/focusing metric)"
    );
}

fn source_scene() -> Scene {
    let mut scene = Scene::default();
    scene.domain.width_m = 0.16;
    scene.domain.height_m = 0.08;
    scene.probes.clear();
    scene.sources = vec![
        Source {
            id: 1,
            center: Point { x: 0.05, y: 0.025 },
            length_m: 0.06,
            ..Source::default()
        },
        Source {
            id: 2,
            center: Point { x: 0.05, y: 0.055 },
            length_m: 0.06,
            phase_rad: 0.8,
            ..Source::default()
        },
    ];
    scene
}

#[test]
fn bent_guide_cw_reaches_second_arm_but_not_through_a_pec_plug() {
    let mut scene = Scene::default();
    scene.domain.width_m = 0.2;
    scene.domain.height_m = 0.16;
    scene.probes.clear();
    scene.sources[0].center = Point { x: 0.065, y: 0.055 };
    scene.sources[0].length_m = 0.07;
    scene.sources[0].bend = Some(volna::model::GuideBend { length_m: 0.065 });
    let mut blocked_scene = scene.clone();
    blocked_scene.reflectors.push(volna::model::Reflector {
        id: 3,
        name: "Plug".into(),
        center: Point { x: 0.100, y: 0.080 },
        angle_deg: 0.0,
        shape: volna::model::Shape::Rectangle {
            width_m: 0.028,
            height_m: 0.004,
        },
    });
    let mut open = Solver::new(&scene).unwrap();
    let mut blocked = Solver::new(&blocked_scene).unwrap();
    let grid = open.grid();
    let (x, y) = grid.nearest(Point { x: 0.075, y: 0.055 });
    let incoming = grid.index(x, y);
    let (x, y) = grid.nearest(Point { x: 0.100, y: 0.100 });
    let outgoing = grid.index(x, y);
    assert!(!open.mask()[incoming] && !open.mask()[outgoing]);
    let mut first_peak: f64 = 0.0;
    let mut second_peak: f64 = 0.0;
    let mut blocked_peak: f64 = 0.0;
    for step in 0..900 {
        open.step().unwrap();
        blocked.step().unwrap();
        if step >= 350 {
            first_peak = first_peak.max(open.ez()[incoming].abs());
            second_peak = second_peak.max(open.ez()[outgoing].abs());
            blocked_peak = blocked_peak.max(blocked.ez()[outgoing].abs());
        }
    }
    assert!(
        first_peak > 1e-4 && second_peak > 1e-4,
        "incoming={first_peak}, outgoing={second_peak}"
    );
    assert!(
        blocked_peak < 1e-8 * second_peak,
        "PEC plug leakage={blocked_peak}"
    );
    eprintln!(
        "bent guide CW: incoming peak={first_peak:.6e} V/m, second arm={second_peak:.6e}, plugged={blocked_peak:.6e}"
    );
}

#[test]
fn two_currents_obey_superposition_in_identical_geometry() {
    let scene = source_scene();
    let mut only_a = scene.clone();
    only_a.sources[1].enabled = false;
    let mut only_b = scene.clone();
    only_b.sources[0].enabled = false;
    let mut a = Solver::new(&only_a).unwrap();
    let mut b = Solver::new(&only_b).unwrap();
    let mut both = Solver::new(&scene).unwrap();
    assert_eq!(a.mask(), b.mask());
    assert_eq!(a.mask(), both.mask());
    for _ in 0..100 {
        a.step().unwrap();
        b.step().unwrap();
        both.step().unwrap();
    }
    let mut error: f64 = 0.0;
    let mut peak: f64 = 0.0;
    for ((&a, &b), &sum) in a.ez().iter().zip(b.ez()).zip(both.ez()) {
        error = error.max((a + b - sum).abs());
        peak = peak.max(sum.abs());
    }
    assert!(peak > 1e-4);
    assert!(
        error / peak < 1e-12,
        "relative superposition error={}",
        error / peak
    );
}

#[test]
fn soft_current_uses_midpoint_and_preserves_physical_strip() {
    let mut scene = source_scene();
    scene.sources.truncate(1);
    scene.sources[0].signal = Signal::Formula {
        expression: "t*1e12".into(),
    };
    scene.sources[0].aperture = Profile::Uniform;
    scene.sources[0].current_a_m2 = 2.0;
    let mut solver = Solver::new(&scene).unwrap();
    let grid = solver.grid();
    solver.step().unwrap();
    let expected = -grid.dt / EPS0 * 2.0 * (0.5 * grid.dt * 1e12);
    let mut active = 0;
    for j in 0..grid.ny {
        for i in 0..grid.nx {
            let field = solver.ez()[grid.index(i, j)];
            if let Some((_, weight)) = scene.sources[0].excitation_weight(grid.point(i, j)) {
                assert!((field / (expected * weight) - 1.0).abs() < 1e-13);
                active += 1;
            } else {
                assert_eq!(field, 0.0);
            }
        }
    }
    assert!(active > 20);
    assert_eq!(solver.time(), grid.dt);
}

#[test]
fn normal_current_integral_is_preserved_under_refinement() {
    let mut values = Vec::new();
    for dx in [0.001, 0.0005] {
        let mut scene = source_scene();
        scene.sources.truncate(1);
        scene.domain.cell_m = dx;
        scene.sources[0].strip_m = 0.004;
        scene.sources[0].source_offset_m = 0.012;
        scene.sources[0].signal = Signal::Formula {
            expression: "1".into(),
        };
        scene.sources[0].aperture = Profile::Uniform;
        scene.sources[0].current_a_m2 = 2.0;
        let mut solver = Solver::new(&scene).unwrap();
        solver.step().unwrap();
        let g = solver.grid();
        let (_, j) = g.nearest(scene.sources[0].center);
        let integral: f64 = (0..g.nx)
            .map(|i| -solver.ez()[g.index(i, j)] * EPS0 / g.dt * g.dx)
            .sum();
        // Integral of J0*cos²(pi*n/strip) over the physical strip is J0*strip/2.
        let expected = scene.sources[0].current_a_m2 * scene.sources[0].strip_m / 2.0;
        assert!(
            (integral / expected - 1.0).abs() < 1e-12,
            "h={dx}: integral={integral}"
        );
        values.push(integral);
    }
    assert!((values[0] / values[1] - 1.0).abs() < 1e-12);
}

#[test]
fn excessive_combined_source_work_is_rejected() {
    let mut scene = Scene::default();
    scene.sources[0].width_m = 0.22;
    scene.sources[0].strip_m = 0.03;
    scene.sources[0].source_offset_m = 0.025;
    let mut nodes = vec!["t".to_owned(); 128];
    while nodes.len() > 1 {
        nodes = nodes
            .chunks_exact(2)
            .map(|pair| format!("({}+{})", pair[0], pair[1]))
            .collect();
    }
    scene.sources[0].aperture = Profile::Formula {
        expression: nodes.remove(0),
    };
    let grid = scene.validate().unwrap();
    let compiled = volna::sources::CompiledSource::new(&scene.sources[0]).unwrap();
    let active: usize = (0..grid.ny)
        .map(|j| {
            (0..grid.nx)
                .filter(|&i| {
                    scene.sources[0]
                        .excitation_coordinate(grid.point(i, j))
                        .is_some()
                })
                .count()
        })
        .sum();
    assert!(active * compiled.cost() > 1_000_000);
    assert!(matches!(Solver::new(&scene), Err(volna::Error::Invalid(_))));
}

#[test]
fn disabled_invalid_sources_are_rejected_consistently_with_persistence() {
    let signals = [
        Signal::Formula {
            expression: "sqrt(-1)".into(),
        },
        Signal::Formula {
            expression: "unknown_function(t)".into(),
        },
        Signal::Formula {
            expression: "sin(".into(),
        },
        Signal::Table {
            samples: vec![[1.0, 0.0], [0.0, 1.0]],
            periodic: false,
        },
    ];
    for signal in signals {
        let mut scene = source_scene();
        scene.sources[1].enabled = false;
        scene.sources[1].signal = signal;
        assert!(volna::persistence::scene_hash(&scene).is_err());
        assert!(
            Solver::new(&scene).is_err(),
            "muted invalid source must not bypass validation"
        );
    }
}

#[test]
fn source_intersecting_pec_is_rejected_before_stepping() {
    let mut scene = source_scene();
    let source = &scene.sources[0];
    let excitation = source.world(Point {
        x: -source.length_m / 2.0 + source.source_offset_m,
        y: 0.0,
    });
    scene.reflectors.push(volna::model::Reflector {
        id: 4,
        name: "Перекрытие".into(),
        center: excitation,
        angle_deg: 0.0,
        shape: volna::model::Shape::Circle { radius_m: 0.003 },
    });
    assert!(Solver::new(&scene).is_err());
}
