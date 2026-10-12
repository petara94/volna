use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use volna::{
    model::{ObjectPose, Point, Reflector, Scene, Shape},
    runtime::{Command, Event, Runtime, Snapshot},
    solver::Solver,
};

fn scene() -> Scene {
    let mut scene = Scene::default();
    scene.domain.width_m = 0.16;
    scene.domain.height_m = 0.10;
    scene.domain.pml_cells = 8;
    scene.sources[0].center = Point { x: 0.045, y: 0.05 };
    scene.sources[0].length_m = 0.06;
    scene.probes[0].position = Point { x: 0.11, y: 0.05 };
    scene.reflectors.push(Reflector {
        id: 3,
        name: "mirror".into(),
        center: Point { x: 0.13, y: 0.05 },
        angle_deg: 0.0,
        shape: Shape::Segment {
            length_m: 0.02,
            thickness_m: 0.002,
        },
    });
    scene.rms_window_s = 4.0 * scene.validate().unwrap().dt;
    scene
}

fn snapshot(runtime: &Runtime, condition: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "snapshot timeout");
        if let Some(s) = runtime.wait_snapshot(remaining).unwrap()
            && condition(&s)
        {
            return s;
        }
    }
}

fn boot_paused(scene: Scene) -> (Runtime, Arc<Snapshot>) {
    let runtime = Runtime::new(scene).unwrap();
    runtime.send(Command::Play(false)).unwrap();
    let s = snapshot(&runtime, |s| !s.running);
    assert_eq!((s.epoch, s.revision, s.run), (1, 1, 1));
    (runtime, s)
}

fn step(runtime: &Runtime, previous: &Snapshot) -> Arc<Snapshot> {
    runtime.send(Command::Step).unwrap();
    snapshot(runtime, |s| s.step == previous.step + 1 && !s.running)
}

fn pose_result(runtime: &Runtime, request_id: u64) -> std::result::Result<u64, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "pose timeout");
        if let Some(Event::PoseResult {
            request_id: id,
            result,
            ..
        }) = runtime.wait_event(remaining).unwrap()
            && id == request_id
        {
            return result;
        }
    }
}

fn pose(runtime: &Runtime, base: &Snapshot, request_id: u64, pose: ObjectPose) -> Arc<Snapshot> {
    runtime
        .send(Command::SetPose {
            epoch: base.epoch,
            request_id,
            pose,
        })
        .unwrap();
    let revision = pose_result(runtime, request_id).unwrap();
    snapshot(runtime, |s| s.revision == revision)
}

fn event(runtime: &Runtime, condition: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "event timeout");
        if let Some(e) = runtime.wait_event(remaining).unwrap()
            && condition(&e)
        {
            return e;
        }
    }
}

struct TempDirectory(std::path::PathBuf);
impl TempDirectory {
    fn new() -> Self {
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "volna-live-runtime-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn barriers_reset_load_and_invalid_cold_reject_old_gestures_but_allow_fresh_ones() {
    let files = TempDirectory::new();
    let loaded_path = files.0.join("load.json");
    let mut loaded = scene();
    loaded.probes[0].position = Point { x: 0.10, y: 0.06 };
    volna::persistence::save_scene(&loaded_path, &loaded, false).unwrap();
    let (runtime, mut s) = boot_paused(scene());
    s = step(&runtime, &s);
    for command in [
        Command::Reset,
        Command::Load {
            request_id: 20,
            path: loaded_path,
        },
        Command::Load {
            request_id: 21,
            path: files.0.join("missing.json"),
        },
        Command::Apply {
            request_id: 22,
            scene: {
                let mut bad = scene();
                bad.domain.cell_m = f64::NAN;
                bad
            },
        },
    ] {
        let old = Arc::clone(&s);
        let succeeds = matches!(
            command,
            Command::Reset | Command::Load { request_id: 20, .. }
        );
        runtime.send(command).unwrap();
        assert!(
            runtime
                .send(Command::SetPose {
                    epoch: old.epoch,
                    request_id: 23,
                    pose: ObjectPose::Probe {
                        id: 2,
                        position: Point { x: 0.10, y: 0.05 }
                    }
                })
                .is_err()
        );
        s = snapshot(&runtime, |s| s.epoch > old.epoch);
        assert_eq!(s.epoch, old.epoch + 1);
        assert_eq!(s.run, old.run + u64::from(succeeds));
        assert_eq!(s.revision, old.revision + u64::from(succeeds));
        if succeeds {
            assert_eq!(s.step, 0);
            assert!(!s.inherited_fields);
        } else {
            assert_eq!(s.ez, old.ez);
            assert_eq!(s.step, old.step);
        }
        s = pose(
            &runtime,
            &s,
            24,
            ObjectPose::Probe {
                id: 2,
                position: Point {
                    x: 0.10,
                    y: 0.04 + 0.001 * s.epoch as f64,
                },
            },
        );
        s = step(&runtime, &s);
    }
}

#[test]
fn files_survive_barriers_save_draft_and_export_only_accepted_segment_metadata() {
    let files = TempDirectory::new();
    let (runtime, mut s) = boot_paused(scene());
    for _ in 0..6 {
        s = step(&runtime, &s);
    }
    let source_moved = pose(
        &runtime,
        &s,
        2,
        ObjectPose::Source {
            id: 1,
            center: Point { x: 0.05, y: 0.05 },
            angle_deg: 0.0,
        },
    );
    let moved = pose(
        &runtime,
        &source_moved,
        3,
        ObjectPose::Probe {
            id: 2,
            position: Point { x: 0.1, y: 0.04 },
        },
    );
    assert!(
        moved.inherited_fields,
        "probe movement must not clear inherited EM state"
    );
    let measured = step(&runtime, &moved);
    assert_eq!(measured.probes[0].samples.len(), 1);
    assert!(measured.probes[0].samples[0][0] > moved.history_start_time_s);
    let csv_path = files.0.join("before-reset.csv");
    let draft_path = files.0.join("draft.json");
    let reset_csv = files.0.join("after-reset.csv");
    let mut draft = (*measured.scene).clone();
    draft.sources[0].current_a_m2 = 3.0;
    runtime
        .send(Command::Save {
            path: draft_path.clone(),
            scene: draft.clone(),
            overwrite: false,
        })
        .unwrap();
    runtime
        .send(Command::Export {
            path: csv_path.clone(),
            overwrite: false,
        })
        .unwrap();
    runtime.send(Command::Reset).unwrap();
    runtime
        .send(Command::Export {
            path: reset_csv.clone(),
            overwrite: false,
        })
        .unwrap();
    runtime.send(Command::Play(false)).unwrap();
    let mut saved = std::collections::HashSet::new();
    while saved.len() < 3 {
        if let Event::FileSaved { path } = event(&runtime, |e| matches!(e, Event::FileSaved { .. }))
        {
            saved.insert(path);
        }
    }
    assert_eq!(volna::persistence::load_scene(&draft_path).unwrap(), draft);
    let csv = std::fs::read_to_string(csv_path).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(
        csv.lines()
            .next()
            .unwrap()
            .strip_prefix("# metadata: ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["epoch"], measured.epoch);
    assert_eq!(metadata["revision"], measured.revision);
    assert_eq!(metadata["run"], measured.run);
    assert_eq!(metadata["inherited_fields"], true);
    assert_eq!(metadata["history_start_step"], moved.step);
    // Check the exact serialized decimal, not a second binary rounding by the
    // JSON parser (serde_json here does not enable its float_roundtrip feature).
    assert!(csv.lines().next().unwrap().contains(&format!(
        "\"history_start_time_s\":{}",
        serde_json::to_string(&moved.time_s).unwrap()
    )));
    let expected_scene: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(measured.scene.as_ref()).unwrap()).unwrap();
    assert_eq!(metadata["scene"], expected_scene);
    assert_eq!(
        metadata["scene_sha256"],
        volna::persistence::scene_hash(&measured.scene).unwrap()
    );
    assert_eq!(
        csv.lines().count(),
        3,
        "metadata + header + exactly one new sample"
    );
    let reset_csv = std::fs::read_to_string(reset_csv).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(
        reset_csv
            .lines()
            .next()
            .unwrap()
            .strip_prefix("# metadata: ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["run"], measured.run + 1);
    assert_eq!(metadata["revision"], measured.revision + 1);
    assert_eq!(metadata["inherited_fields"], false);
    assert_eq!(metadata["history_start_step"], 0);
    assert_eq!(reset_csv.lines().count(), 2, "no fabricated t=0 sample");
}

#[test]
fn numerical_failure_rejects_every_live_pose_and_keeps_safe_last_good_across_cold_error() {
    let mut input = scene();
    input.sources[0].signal = volna::model::Signal::Formula {
        expression: "sqrt(1 - t / 6e-12)".into(),
    };
    let (runtime, mut s) = boot_paused(input);
    while s.step < 2 {
        s = step(&runtime, &s);
    }
    let moved = pose(
        &runtime,
        &s,
        2,
        ObjectPose::Source {
            id: 1,
            center: Point { x: 0.05, y: 0.05 },
            angle_deg: 0.0,
        },
    );
    let safe = step(&runtime, &moved);
    runtime.send(Command::Step).unwrap();
    event(&runtime, |e| {
        matches!(e, Event::Error { request_id: 0, .. })
    });
    let failed = snapshot(&runtime, |s| !s.running);
    assert_eq!(failed.ez, safe.ez);
    assert_eq!((failed.step, failed.revision), (safe.step, safe.revision));
    for (request_id, update) in [
        (
            3,
            ObjectPose::Probe {
                id: 2,
                position: Point { x: 0.1, y: 0.04 },
            },
        ),
        (4, safe.scene.pose(1).unwrap()),
    ] {
        runtime
            .send(Command::SetPose {
                epoch: safe.epoch,
                request_id,
                pose: update,
            })
            .unwrap();
        assert!(pose_result(&runtime, request_id).is_err());
    }
    let mut invalid = (*safe.scene).clone();
    invalid.domain.cell_m = f64::NAN;
    runtime
        .send(Command::Apply {
            request_id: 5,
            scene: invalid,
        })
        .unwrap();
    event(&runtime, |e| {
        matches!(
            e,
            Event::Error {
                request_id: 5,
                epoch: 2,
                ..
            }
        )
    });
    let fresh_epoch = snapshot(&runtime, |s| s.epoch == 2);
    assert_eq!(fresh_epoch.ez, safe.ez);
    assert_eq!(
        (fresh_epoch.step, fresh_epoch.run, fresh_epoch.revision),
        (safe.step, safe.run, safe.revision)
    );
    runtime
        .send(Command::SetPose {
            epoch: 2,
            request_id: 6,
            pose: ObjectPose::Probe {
                id: 2,
                position: Point { x: 0.1, y: 0.04 },
            },
        })
        .unwrap();
    assert!(pose_result(&runtime, 6).is_err());
    runtime.send(Command::Play(true)).unwrap();
    event(&runtime, |e| {
        matches!(e, Event::Error { request_id: 0, .. })
    });
    runtime.send(Command::Reset).unwrap();
    let reset = snapshot(&runtime, |s| s.epoch == 3);
    assert_eq!(reset.step, 0);
    let recovered = pose(
        &runtime,
        &reset,
        7,
        ObjectPose::Probe {
            id: 2,
            position: Point { x: 0.1, y: 0.04 },
        },
    );
    assert_eq!(recovered.revision, reset.revision + 1);
}

#[test]
fn continuous_requests_publish_intermediate_frames_without_starving_physics() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (runtime, base) = boot_paused(scene());
    runtime.send(Command::Speed(1)).unwrap();
    runtime.send(Command::Play(true)).unwrap();
    let stop = AtomicBool::new(false);
    let last_request = std::thread::scope(|scope| {
        let producer = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut request_id = 2;
            while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                runtime
                    .send(Command::SetPose {
                        epoch: base.epoch,
                        request_id,
                        pose: ObjectPose::Source {
                            id: 1,
                            center: Point {
                                x: 0.05 + 0.0001 * (request_id % 20) as f64,
                                y: 0.05,
                            },
                            angle_deg: 0.0,
                        },
                    })
                    .unwrap();
                request_id += 1;
                std::thread::yield_now();
            }
            request_id
        });
        let mut previous = Arc::clone(&base);
        for _ in 0..3 {
            let next = snapshot(&runtime, |s| {
                s.revision > previous.revision && s.step > previous.step
            });
            assert!(next.running);
            assert_eq!(next.run, base.run);
            previous = next;
        }
        stop.store(true, Ordering::Release);
        producer.join().unwrap()
    });
    runtime
        .send(Command::SetPose {
            epoch: base.epoch,
            request_id: last_request,
            pose: ObjectPose::Source {
                id: 1,
                center: Point { x: 0.055, y: 0.05 },
                angle_deg: 0.0,
            },
        })
        .unwrap();
    let revision = pose_result(&runtime, last_request).unwrap();
    let final_pose = snapshot(&runtime, |s| s.revision == revision);
    assert_eq!(final_pose.scene.sources[0].center.x, 0.055);
    assert!(final_pose.step > base.step);
}

#[test]
#[ignore = "explicit bounded interactive latency experiment, <=10 seconds, run in release"]
fn default_grid_live_latency_budget() {
    let runtime = Runtime::new(Scene::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut previous = snapshot(&runtime, |s| s.step > 0);
    assert_eq!((previous.grid.nx, previous.grid.ny), (441, 341));
    let start = Instant::now();
    let mut latencies = Vec::new();
    let mut physical_frames = 0;
    for request_id in 2..22 {
        assert!(Instant::now() < deadline);
        let sent = Instant::now();
        runtime
            .send(Command::SetPose {
                epoch: previous.epoch,
                request_id,
                pose: ObjectPose::Source {
                    id: 1,
                    center: Point {
                        x: 0.08 + 0.0002 * (request_id % 10) as f64,
                        y: 0.15,
                    },
                    angle_deg: (request_id % 4) as f64,
                },
            })
            .unwrap();
        let next = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "10-second experiment budget exhausted"
            );
            if let Some(s) = runtime.wait_snapshot(remaining).unwrap()
                && s.revision > previous.revision
            {
                break s;
            }
        };
        physical_frames += usize::from(next.step > previous.step);
        latencies.push(sent.elapsed().as_secs_f64() * 1000.0);
        previous = next;
    }
    let total = start.elapsed().as_secs_f64();
    eprintln!(
        "default grid: 20 accepted moves in {total:.3}s, {:.1} moves/s; latency mean {:.2}ms max {:.2}ms; physics advanced on {physical_frames}/20 accepted frames; build={}",
        20.0 / total,
        latencies.iter().sum::<f64>() / 20.0,
        latencies.iter().copied().fold(0.0, f64::max),
        env!("VOLNA_BUILD_PROFILE")
    );
    assert!(physical_frames > 0);
    assert!(Instant::now() < deadline);
}

#[test]
fn paused_source_and_reflector_moves_preserve_clock_and_nonzero_field() {
    let (runtime, mut s) = boot_paused(scene());
    for _ in 0..10 {
        s = step(&runtime, &s);
    }
    assert!(s.ez.iter().any(|v| *v != 0.0));
    assert!(s.rms.is_some());
    for (request_id, update) in [
        (
            2,
            ObjectPose::Source {
                id: 1,
                center: Point { x: 0.05, y: 0.05 },
                angle_deg: 5.0,
            },
        ),
        (
            3,
            ObjectPose::Reflector {
                id: 3,
                center: Point { x: 0.12, y: 0.05 },
                angle_deg: 20.0,
            },
        ),
    ] {
        let after = pose(&runtime, &s, request_id, update.clone());
        assert_eq!(
            (after.step, after.time_s, after.run),
            (s.step, s.time_s, s.run)
        );
        assert!(!after.running);
        assert_eq!(after.revision, s.revision + 1);
        assert_eq!(after.scene.pose(update.id()), Some(update));
        assert!(after.ez.iter().any(|v| *v != 0.0));
        for (i, value) in after.ez.iter().enumerate() {
            assert_eq!(
                value.to_bits(),
                if after.mask[i] {
                    0.0_f64.to_bits()
                } else {
                    s.ez[i].to_bits()
                }
            );
        }
        assert!(after.rms.is_none());
        assert!(after.inherited_fields);
        assert!(after.probes.iter().all(|p| p.samples.is_empty()));
        assert_eq!(after.history_start_step, s.step);
        assert_eq!(after.history_start_time_s, s.time_s);
        s = after;
    }
}

#[test]
fn probe_only_and_noop_preserve_solver_and_next_step() {
    let scene = scene();
    let mut control = Solver::new(&scene).unwrap();
    let (runtime, mut s) = boot_paused(scene);
    for _ in 0..8 {
        s = step(&runtime, &s);
    }
    for _ in 0..s.step {
        control.step().unwrap();
    }
    let update = ObjectPose::Probe {
        id: 2,
        position: Point { x: 0.09, y: 0.04 },
    };
    let after = pose(&runtime, &s, 2, update.clone());
    assert_eq!(after.ez, s.ez);
    assert_eq!(after.rms, s.rms);
    assert_eq!(after.rms_progress, s.rms_progress);
    assert!(!after.inherited_fields);
    assert!(after.probes[0].samples.is_empty());
    let next = step(&runtime, &after);
    control.step().unwrap();
    assert_eq!(next.ez, control.ez());
    assert_eq!(next.rms.as_deref(), control.rms());
    let noop = pose(&runtime, &next, 3, update);
    assert_eq!(noop.revision, next.revision);
    assert_eq!(noop.probes[0].samples, next.probes[0].samples);
    assert_eq!(noop.history_start_step, after.history_start_step);
    assert_eq!(noop.ez, next.ez);
}

#[test]
fn invalid_pose_is_consumed_and_does_not_poison_another_object() {
    let (runtime, mut s) = boot_paused(scene());
    s = step(&runtime, &s);
    runtime
        .send(Command::SetPose {
            epoch: s.epoch,
            request_id: 2,
            pose: ObjectPose::Source {
                id: 1,
                center: Point {
                    x: f64::NAN,
                    y: 0.05,
                },
                angle_deg: 0.0,
            },
        })
        .unwrap();
    assert!(pose_result(&runtime, 2).is_err());
    let after = pose(
        &runtime,
        &s,
        3,
        ObjectPose::Probe {
            id: 2,
            position: Point { x: 0.10, y: 0.04 },
        },
    );
    assert_eq!(after.scene.sources, s.scene.sources);
    assert_eq!(after.ez, s.ez);
    assert_eq!(after.revision, s.revision + 1);
    let fixed = pose(
        &runtime,
        &after,
        4,
        ObjectPose::Source {
            id: 1,
            center: Point { x: 0.05, y: 0.05 },
            angle_deg: 0.0,
        },
    );
    assert_eq!(fixed.revision, after.revision + 1);
    assert_eq!(fixed.time_s, s.time_s);
}
