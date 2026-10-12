use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use volna::model::{Scene, Signal};
use volna::persistence::{
    export_probes, export_probes_with_metadata, load_scene, save_scene, scene_hash,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> std::io::Result<Self> {
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "volna-scene-io-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Не удалось создать каталог теста",
        ))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn bundled_waveguide_preset_matches_default_and_round_trips() -> volna::Result<()> {
    let preset = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/waveguide_10ghz.json");
    let scene = load_scene(&preset)?;
    assert_eq!(scene, Scene::default());
    let directory = TestDirectory::new()?;
    let path = directory.0.join("preset-round-trip.json");
    save_scene(&path, &scene, false)?;
    let restored = load_scene(&path)?;
    assert_eq!(restored, scene);
    assert_eq!(scene_hash(&restored)?, scene_hash(&Scene::default())?);
    Ok(())
}

#[test]
fn scene_round_trip_update_and_explicit_overwrite() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("scene.json");
    let scene = Scene::default();
    save_scene(&path, &scene, false)?;
    assert_eq!(load_scene(&path)?, scene);
    let original = fs::read(&path)?;
    let mut changed = scene.clone();
    changed.sources[0].name = "Изменённое имя".into();
    assert!(save_scene(&path, &changed, false).is_err());
    assert_eq!(fs::read(&path)?, original);
    save_scene(&path, &changed, true)?;
    assert_eq!(load_scene(&path)?, changed);
    assert_eq!(fs::read_dir(&directory.0)?.count(), 1);
    fs::remove_file(&path)?;
    assert!(load_scene(&path).is_err());
    Ok(())
}

#[test]
fn scene_loading_rejects_unknown_fields_invalid_expressions_and_versions() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("invalid.json");
    let scene = Scene::default();
    let original = serde_json::to_value(&scene)?;
    for field in ["unknown", "nested", "version", "formula"] {
        let mut value = original.clone();
        match field {
            "unknown" => value["unknown"] = true.into(),
            "nested" => value["sources"][0]["signal"]["unknown"] = true.into(),
            "version" => value["schema_version"] = 999.into(),
            "formula" => {
                value["sources"][0]["signal"] =
                    serde_json::json!({"kind":"Formula", "expression":"t=1"})
            }
            _ => unreachable!(),
        }
        fs::write(&path, serde_json::to_vec(&value)?)?;
        assert!(load_scene(&path).is_err(), "{field}");
    }
    Ok(())
}

#[test]
fn scene_loading_has_an_eight_mib_limit() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("too-large.json");
    let scene = Scene::default();
    let mut bytes = serde_json::to_vec(&scene)?;
    bytes.resize(8 * 1024 * 1024 + 1, b' ');
    fs::write(&path, &bytes[..bytes.len() - 1])?;
    assert_eq!(load_scene(&path)?, scene);
    fs::write(&path, &bytes)?;
    assert!(
        matches!(load_scene(&path), Err(volna::Error::Invalid(message)) if message.contains("8 МиБ"))
    );
    Ok(())
}

#[test]
fn invalid_scene_is_not_written_or_hashed() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("scene.json");
    let mut scene = Scene::default();
    scene.sources[0].signal = Signal::Formula {
        expression: "random()".into(),
    };
    assert!(save_scene(&path, &scene, false).is_err());
    assert!(!path.exists());
    assert!(scene_hash(&scene).is_err());
    scene.sources[0].signal = Signal::Continuous { ramp_periods: 1.0 };
    scene.domain.width_m = f64::NAN;
    assert!(save_scene(&path, &scene, false).is_err());
    assert!(scene_hash(&scene).is_err());
    Ok(())
}

#[test]
fn deterministic_hash_changes_with_scene_content() -> volna::Result<()> {
    let scene = Scene::default();
    let hash = scene_hash(&scene)?;
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|value| value.is_ascii_hexdigit()));
    assert_eq!(hash, scene_hash(&scene.clone())?);
    let mut changed = scene;
    changed.sources[0].phase_rad = 1.0;
    assert_ne!(hash, scene_hash(&changed)?);
    Ok(())
}

#[test]
fn csv_escapes_names_and_blocks_spreadsheet_formulas() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("probes.csv");
    let names = vec!["=SUM(A1:A2)".into(), "имя,\"зонд\"\nстрока".into()];
    let samples = vec![(0.0, vec![1.5, -2.0]), (1e-9, vec![0.0, 3.0])];
    export_probes(&path, &names, &samples, false)?;
    let csv = fs::read_to_string(&path)?;
    assert!(csv.starts_with("time_s,\"Ez_V_m:=SUM(A1:A2)\",\"Ez_V_m:имя,\"\"зонд\"\"\nстрока\"\n"));
    assert!(csv.contains("\n0,1.5,-2\n"));
    assert!(export_probes(&path, &names, &samples, false).is_err());
    assert_eq!(fs::read_to_string(&path)?, csv);
    export_probes(&path, &names, &[], true)?;
    assert!(!fs::read_to_string(&path)?.contains("1.5"));
    assert_eq!(fs::read_dir(&directory.0)?.count(), 1);
    Ok(())
}

#[test]
fn csv_metadata_is_compact_json_in_the_same_atomic_file() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("probes.csv");
    let metadata = serde_json::json!({
        "schema_version": 1,
        "scene_hash": scene_hash(&Scene::default())?,
        "description": "Одна\\строка\nи ещё строка",
    });
    let names = vec!["Зонд".into()];
    export_probes_with_metadata(&path, &names, &[(0.0, vec![2.0])], false, &metadata)?;
    let csv = fs::read_to_string(&path)?;
    let (line, table) = csv
        .split_once('\n')
        .expect("Строка метаданных должна завершаться переводом строки");
    let restored: serde_json::Value = serde_json::from_str(
        line.strip_prefix("# metadata: ")
            .expect("Префикс метаданных"),
    )?;
    assert_eq!(restored, metadata);
    assert_eq!(table, "time_s,\"Ez_V_m:Зонд\"\n0,2\n");
    assert!(
        export_probes_with_metadata(&path, &names, &[], false, &serde_json::Value::Null).is_err()
    );
    assert_eq!(fs::read_to_string(&path)?, csv);
    assert_eq!(fs::read_dir(&directory.0)?.count(), 1);
    Ok(())
}

#[test]
fn csv_rejects_nonfinite_and_mismatched_frames_without_partial_files() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("probes.csv");
    let names = vec!["Зонд".into()];
    for samples in [
        vec![(0.0, vec![])],
        vec![(f64::NAN, vec![1.0])],
        vec![(0.0, vec![f64::INFINITY])],
        vec![(1.0, vec![1.0]), (0.0, vec![1.0])],
    ] {
        assert!(export_probes(&path, &names, &samples, false).is_err());
        assert!(!path.exists());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn existing_symlink_is_not_followed_or_silently_overwritten() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let destination = directory.0.join("scene.json");
    let original = directory.0.join("original.json");
    fs::write(&original, "не изменять")?;
    std::os::unix::fs::symlink(&original, &destination)?;
    assert!(save_scene(&destination, &Scene::default(), false).is_err());
    assert_eq!(fs::read(&original)?, "не изменять".as_bytes());
    assert!(fs::symlink_metadata(&destination)?.file_type().is_symlink());
    save_scene(&destination, &Scene::default(), true)?;
    assert_eq!(load_scene(&destination)?, Scene::default());
    assert_eq!(fs::read(&original)?, "не изменять".as_bytes());
    Ok(())
}

#[test]
fn competing_nonoverwrite_saves_have_exactly_one_winner() -> volna::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.0.join("race.json");
    let barrier = &std::sync::Barrier::new(4);
    let winners = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = &path;
                scope.spawn(move || {
                    barrier.wait();
                    save_scene(path, &Scene::default(), false).is_ok()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("Поток теста завершился аварийно"))
            .filter(|won| *won)
            .count()
    });
    assert_eq!(winners, 1);
    assert_eq!(load_scene(&path)?, Scene::default());
    assert_eq!(fs::read_dir(&directory.0)?.count(), 1);
    Ok(())
}
