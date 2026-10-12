use std::time::{Duration, Instant};
use volna::{
    model::Scene,
    runtime::{Command, Event, Runtime},
};

#[test]
fn numerical_failure_never_publishes_partial_fields_on_pause_or_resume() {
    let mut scene = Scene::default();
    scene.sources[0].signal = volna::model::Signal::Formula {
        expression: "sqrt(1 - t / 1e-12)".into(),
    };
    let runtime = Runtime::new(scene).expect("worker");
    runtime.send(Command::Play(false)).expect("пауза");
    assert!(matches!(
        runtime.wait_event(Duration::from_secs(5)).expect("старт"),
        Some(Event::Applied { .. })
    ));
    runtime
        .send(Command::Step)
        .expect("шаг с ошибкой области определения");
    assert!(matches!(
        runtime.wait_event(Duration::from_secs(5)).expect("ошибка"),
        Some(Event::Error { request_id: 0, .. })
    ));
    runtime
        .send(Command::Play(true))
        .expect("попытка продолжить");
    assert!(matches!(
        runtime
            .wait_event(Duration::from_secs(5))
            .expect("требуется сброс"),
        Some(Event::Error { request_id: 0, .. })
    ));
    runtime.send(Command::Play(false)).expect("повторная пауза");
    let snapshot = runtime
        .wait_snapshot(Duration::from_secs(5))
        .expect("снимок")
        .expect("последний корректный кадр");
    assert_eq!(snapshot.step, 0);
    assert!(!snapshot.running);
    assert!(snapshot.ez.iter().all(|x| *x == 0.0));
}

fn small_scene() -> Scene {
    let mut scene = Scene::default();
    scene.sources.clear();
    scene.probes.clear();
    scene.domain.width_m = 0.04;
    scene.domain.height_m = 0.03;
    scene.domain.pml_cells = 8;
    scene
}

#[test]
fn rejected_revision_does_not_replace_active_scene() {
    let runtime = Runtime::new(small_scene()).expect("worker");
    assert!(matches!(
        runtime.wait_event(Duration::from_secs(5)).expect("событие"),
        Some(Event::Applied { revision: 1, .. })
    ));
    runtime.send(Command::Play(false)).expect("пауза");
    let mut invalid = small_scene();
    invalid.domain.cell_m = f64::NAN;
    runtime
        .send(Command::Apply {
            request_id: 2,
            scene: invalid,
        })
        .expect("очередь");
    assert!(matches!(
        runtime.wait_event(Duration::from_secs(5)).expect("событие"),
        Some(Event::Error { request_id: 2, .. })
    ));
    runtime.send(Command::Step).expect("шаг");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "нет снимка после шага");
        if let Some(snapshot) = runtime
            .wait_snapshot(Duration::from_millis(100))
            .expect("снимок")
        {
            assert_eq!(snapshot.revision, 1);
            if !snapshot.running && snapshot.step > 0 {
                break;
            }
        }
    }
}

#[test]
fn pause_step_reset_and_revision_are_explicit() {
    let runtime = Runtime::new(small_scene()).expect("worker");
    runtime.send(Command::Play(false)).expect("пауза");
    runtime.wait_event(Duration::from_secs(5)).expect("старт");
    runtime.send(Command::Reset).expect("сброс");
    runtime.send(Command::Step).expect("шаг");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        if let Some(snapshot) = runtime
            .wait_snapshot(Duration::from_millis(100))
            .expect("снимок")
            && snapshot.step == 1
            && !snapshot.running
        {
            assert!(snapshot.ez.iter().all(|x| *x == 0.0));
            break;
        }
    }
    runtime
        .send(Command::Apply {
            request_id: 7,
            scene: small_scene(),
        })
        .expect("сцена");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        if let Some(Event::Applied {
            request_id: 7,
            revision,
            epoch,
            ..
        }) = runtime
            .wait_event(deadline.saturating_duration_since(Instant::now()))
            .expect("событие")
        {
            assert_eq!(revision, 3, "accepted revision, not request ID");
            assert_eq!(epoch, 3);
            break;
        }
    }
}
