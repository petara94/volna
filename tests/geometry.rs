use volna::model::{GuideBend, ObjectPose, Point, Reflector, Scene, Shape};

#[test]
fn interference_demo_is_valid_and_preserves_the_numerical_baseline() {
    let baseline = Scene::default();
    let demo = Scene::interference_demo();
    assert_eq!(demo.domain, baseline.domain);
    assert_eq!(demo.sources, baseline.sources);
    assert_eq!(demo.rms_window_s, baseline.rms_window_s);
    assert_eq!(demo.reflectors.len(), 6);
    assert_eq!(demo.probes.len(), 3);
    assert_eq!(demo.sources[0].frequency_hz, 10e9);
    let solver = volna::solver::Solver::new(&demo).unwrap();
    for probe in &demo.probes {
        let (i, j) = solver.grid().nearest(probe.position);
        assert!(!solver.mask()[solver.grid().index(i, j)]);
    }
    let old_json = include_str!("../presets/waveguide_10ghz.json");
    assert_eq!(serde_json::from_str::<Scene>(old_json).unwrap(), baseline);
    // The shared region is not a cavity: a whole vertical escape column
    // above/below it remains open towards CPML, and the ribs are disjoint.
    let grid = solver.grid();
    let (x, _) = grid.nearest(Point { x: 0.235, y: 0.15 });
    for y in grid.pml..grid.ny - grid.pml {
        assert!(!solver.mask()[grid.index(x, y)]);
    }
    for pair in demo.reflectors.windows(2) {
        let separation =
            (pair[1].center.x - pair[0].center.x).hypot(pair[1].center.y - pair[0].center.y);
        assert!(
            separation > 0.030,
            "24 mm ribs with 2 mm thickness need an open gap"
        );
    }
    // Geometric optics is only a construction aid, not a claim of wave focus.
    let mouth = Point { x: 0.12, y: 0.15 };
    let target = Point { x: 0.235, y: 0.15 };
    for rib in &demo.reflectors {
        let dx = rib.center.x - mouth.x;
        let dy = rib.center.y - mouth.y;
        let norm = dx.hypot(dy);
        let (sn, cs) = rib.angle_deg.to_radians().sin_cos();
        let normal = Point { x: -sn, y: cs };
        let dot = dx / norm * normal.x + dy / norm * normal.y;
        let reflected = Point {
            x: dx / norm - 2.0 * dot * normal.x,
            y: dy / norm - 2.0 * dot * normal.y,
        };
        let distance = (target.x - rib.center.x).hypot(target.y - rib.center.y);
        let predicted = Point {
            x: rib.center.x + distance * reflected.x,
            y: rib.center.y + distance * reflected.y,
        };
        assert!((predicted.x - target.x).hypot(predicted.y - target.y) < 0.001);
    }
}

#[test]
fn interference_preset_loads_as_the_demo_constructor() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/interference_10ghz.json");
    let loaded = volna::persistence::load_scene(&path).unwrap();
    assert_eq!(loaded, Scene::interference_demo());
    assert!(volna::solver::Solver::new(&loaded).is_ok());
    let encoded = serde_json::to_string(&loaded).unwrap();
    assert_eq!(serde_json::from_str::<Scene>(&encoded).unwrap(), loaded);
}

#[test]
fn bent_guide_has_connected_open_channel_correct_walls_and_real_footprint() {
    let mut source = Scene::default().sources.remove(0);
    source.bend = Some(GuideBend { length_m: 0.06 });
    for angle in [0.0, 90.0, 37.0] {
        source.angle_deg = angle;
        for p in [
            Point { x: 0.0, y: 0.0 },
            Point { x: 0.04, y: 0.0 },
            Point { x: 0.036, y: 0.011 },
            Point { x: 0.04, y: 0.04 },
            Point { x: 0.04, y: 0.06 },
        ] {
            assert!(
                !source.is_pec(source.world(p)),
                "blocked channel at {p:?}, angle={angle}"
            );
            assert!(source.contains_footprint(source.world(p)));
        }
        for p in [
            Point { x: 0.0, y: -0.012 },
            Point { x: 0.0, y: 0.012 },
            Point { x: 0.052, y: 0.02 },
            Point { x: 0.028, y: 0.04 },
            Point { x: 0.05, y: -0.012 },
            Point { x: -0.039, y: 0.0 },
        ] {
            assert!(
                source.is_pec(source.world(p)),
                "missing wall at {p:?}, angle={angle}"
            );
        }
        assert!(!source.contains_footprint(source.world(Point { x: -0.03, y: 0.05 })));
        assert!(!source.is_pec(source.world(Point { x: 0.04, y: 0.062 })));
        assert_eq!(source.outline().len(), 6);
    }
    source.closed_back = false;
    assert!(!source.is_pec(source.world(Point { x: -0.039, y: 0.0 })));
    assert!(source.contains_footprint(source.world(Point { x: -0.039, y: 0.0 })));
}

#[test]
fn bent_guide_validation_uses_actual_rotated_vertices_and_rejects_bad_arms() {
    let mut scene = Scene::default();
    scene.sources[0].bend = Some(GuideBend { length_m: 0.06 });
    scene.sources[0].center.x = 0.05;
    scene.sources[0].angle_deg = 45.0;
    assert!(
        scene.sources[0].world(Point { x: -0.04, y: 0.06 }).x < 0.0,
        "phantom bounding-box corner is outside the domain"
    );
    assert!(scene.validate().is_ok(), "actual six vertices are inside");
    for length in [0.0, -0.1, f64::NAN, f64::INFINITY, 0.014] {
        let mut bad = scene.clone();
        bad.sources[0].bend.as_mut().unwrap().length_m = length;
        assert!(bad.validate().is_err());
    }
    let mut bad = scene.clone();
    bad.sources[0].length_m = 0.014;
    assert!(bad.validate().is_err());
    let mut bad = scene.clone();
    bad.sources[0].source_offset_m = 0.069;
    assert!(bad.validate().is_err());
    let mut bad = scene;
    bad.sources[0].center.x = 0.02;
    assert!(bad.validate().is_err());
}

#[test]
fn optional_bend_round_trips_without_changing_old_json() {
    let scene = Scene::default();
    let old_json = include_str!("../presets/waveguide_10ghz.json");
    assert_eq!(serde_json::from_str::<Scene>(old_json).unwrap(), scene);
    assert!(!serde_json::to_string(&scene).unwrap().contains("bend"));
    let mut bent = scene;
    bent.sources[0].bend = Some(GuideBend { length_m: 0.06 });
    let json = serde_json::to_string(&bent).unwrap();
    assert_eq!(serde_json::from_str::<Scene>(&json).unwrap(), bent);
    let mut value = serde_json::to_value(bent).unwrap();
    value["sources"][0]["bend"]["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Scene>(value).is_err());
}

#[test]
fn bundled_bent_preset_loads_with_free_probes_and_six_vertex_outline() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/waveguide_bend_10ghz.json");
    let scene = volna::persistence::load_scene(&path).unwrap();
    assert_eq!(scene.sources[0].bend.as_ref().unwrap().length_m, 0.08);
    assert_eq!(scene.sources[0].outline().len(), 6);
    let solver = volna::solver::Solver::new(&scene).unwrap();
    for probe in &scene.probes {
        let (i, j) = solver.grid().nearest(probe.position);
        assert!(!solver.mask()[solver.grid().index(i, j)]);
    }
    let json = serde_json::to_string(&scene).unwrap();
    assert_eq!(serde_json::from_str::<Scene>(&json).unwrap(), scene);
}

#[test]
fn typed_pose_round_trip_changes_only_the_requested_object() {
    let mut scene = Scene::default();
    scene.reflectors.push(Reflector {
        id: 3,
        name: "Зеркало".into(),
        center: Point { x: 0.25, y: 0.2 },
        angle_deg: 0.0,
        shape: Shape::Circle { radius_m: 0.004 },
    });
    let original = scene.clone();
    for pose in [
        ObjectPose::Source {
            id: 1,
            center: Point { x: 0.09, y: 0.151 },
            angle_deg: 15.0,
        },
        ObjectPose::Reflector {
            id: 3,
            center: Point { x: 0.26, y: 0.21 },
            angle_deg: 45.0,
        },
        ObjectPose::Probe {
            id: 2,
            position: Point { x: 0.21, y: 0.16 },
        },
    ] {
        let old_pose = scene.pose(pose.id()).unwrap();
        let candidate = scene.with_pose(&pose).unwrap();
        assert_eq!(candidate.pose(pose.id()), Some(pose));
        assert_eq!(candidate.with_pose(&old_pose).unwrap(), original);
        assert_eq!(scene, original);
    }
    assert_eq!(scene.pose(999), None);
}

#[test]
fn typed_pose_rejects_unknown_mismatched_or_invalid_positions() {
    let scene = Scene::default();
    for pose in [
        ObjectPose::Probe {
            id: 999,
            position: Point::default(),
        },
        ObjectPose::Probe {
            id: 1,
            position: Point::default(),
        },
        ObjectPose::Source {
            id: 2,
            center: Point { x: 0.08, y: 0.15 },
            angle_deg: 0.0,
        },
        ObjectPose::Source {
            id: 1,
            center: Point { x: -1.0, y: 0.15 },
            angle_deg: 0.0,
        },
        ObjectPose::Source {
            id: 1,
            center: Point { x: 0.08, y: 0.15 },
            angle_deg: f64::NAN,
        },
        ObjectPose::Probe {
            id: 2,
            position: Point {
                x: f64::NAN,
                y: 0.1,
            },
        },
    ] {
        assert!(scene.with_pose(&pose).is_err());
    }
    assert_eq!(scene, Scene::default());
}

#[test]
fn default_scene_has_si_scale_and_resolved_grid() {
    let scene = Scene::default();
    let g = scene.validate().expect("валидный пресет");
    assert_eq!((g.nx, g.ny), (441, 341));
    assert!((g.dt - 2.122_788_903_074_715e-12).abs() < 1e-24);
    assert_eq!(g.point(g.pml, g.pml), Point::default());
}

#[test]
fn rejects_invalid_numeric_and_resource_inputs() {
    for value in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
        let mut scene = Scene::default();
        scene.domain.cell_m = value;
        assert!(scene.validate().is_err());
    }
    let mut scene = Scene::default();
    scene.domain.cell_m = 1e-12;
    assert!(scene.validate().is_err());
    scene = Scene::default();
    scene.domain.cfl = 1.01;
    assert!(scene.validate().is_err());
    scene = Scene::default();
    scene.sources[0].fmax_hz = 100e9;
    assert!(scene.validate().is_err());
}

#[test]
fn rejects_duplicate_ids_unknown_schema_and_outside_objects() {
    let mut scene = Scene::default();
    scene.probes[0].id = scene.sources[0].id;
    assert!(scene.validate().is_err());
    scene = Scene::default();
    scene.schema_version = 42;
    assert!(scene.validate().is_err());
    scene = Scene::default();
    scene.sources[0].center.x = -0.1;
    assert!(scene.validate().is_err());
}

#[test]
fn guide_has_walls_open_mouth_and_physical_source_strip() {
    let scene = Scene::default();
    let source = &scene.sources[0];
    assert!(source.is_pec(Point { x: 0.08, y: 0.162 }));
    assert!(!source.is_pec(Point { x: 0.12, y: 0.15 }));
    assert!(source.is_pec(Point { x: 0.041, y: 0.15 }));
    assert_eq!(
        source.excitation_coordinate(Point { x: 0.051, y: 0.15 }),
        Some(0.0)
    );
    assert!(
        source
            .excitation_coordinate(Point { x: 0.055, y: 0.15 })
            .is_none()
    );
    // Десятичные координаты границ не должны сдвигать одну стенку на целую ячейку.
    for y in [0.139, 0.161] {
        assert!(source.is_pec(Point { x: 0.051, y }));
        assert!(
            source
                .excitation_coordinate(Point { x: 0.051, y })
                .is_none()
        );
    }
}

#[test]
fn rotation_and_thickness_have_geometric_meaning() {
    let mirror = Reflector {
        id: 3,
        name: "Зеркало".into(),
        center: Point { x: 0.2, y: 0.15 },
        angle_deg: 90.0,
        shape: Shape::Segment {
            length_m: 0.1,
            thickness_m: 0.004,
        },
    };
    assert!(mirror.contains(Point { x: 0.2, y: 0.19 }));
    assert!(!mirror.contains(Point { x: 0.21, y: 0.15 }));
}

#[test]
fn raster_boundary_and_source_are_consistent() {
    let scene = Scene::default();
    let grid = scene.validate().expect("сцена");
    let mask = scene.pec_mask(grid);
    assert_eq!(mask.len(), grid.len());
    assert!(mask[0]);
    let (x, y) = grid.nearest(Point { x: 0.051, y: 0.15 });
    assert!(!mask[grid.index(x, y)]);
    let (x, y) = grid.nearest(Point { x: 0.08, y: 0.162 });
    assert!(mask[grid.index(x, y)]);
}
