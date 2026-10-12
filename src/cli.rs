//! Минимальные аргументы запуска; разбор и загрузка не требуют окна.

use std::{ffi::OsString, path::Path};
use volna::{
    persistence,
    ui::{FieldMode, StartupOptions},
};

pub const USAGE: &str = "Usage: volna [--scene <path>] [--view ez|abs|rms] [--help]\n\
    \n\
    --scene <path>       Загрузить и проверить JSON-сцену до открытия окна (путь UTF-8).\n\
    --view ez|abs|rms    Начальный режим: Ez, |Ez| или RMS(Ez).\n\
    --help              Показать справку без GUI и загрузки сцены.\n\
    \n\
    По умолчанию: встроенная interference_demo, режим ez.\n\
    Каждый аргумент допускается один раз; значения отделяются пробелом.\n";

#[derive(Debug)]
pub enum Action {
    Help,
    Run(Options),
}

#[derive(Debug)]
pub struct Options {
    scene_path: Option<String>,
    view: FieldMode,
}

impl Options {
    pub fn load(self) -> Result<StartupOptions, String> {
        match self.scene_path {
            Some(path) => {
                let scene = persistence::load_scene(Path::new(&path))
                    .map_err(|error| format!("Не удалось загрузить сцену {path:?}: {error}"))?;
                Ok(StartupOptions {
                    scene,
                    view: self.view,
                    scene_path: path,
                })
            }
            None => Ok(StartupOptions {
                view: self.view,
                ..StartupOptions::default()
            }),
        }
    }
}

/// Принимает аргументы без имени программы. Справка не скрывает ошибки синтаксиса.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let mut scene_path = None;
    let mut view = None;
    let mut help = false;
    while let Some(arg) = args.next() {
        let arg = utf8(arg)?;
        match arg.as_str() {
            "--scene" => {
                if scene_path.is_some() {
                    return Err("Повторный аргумент --scene".into());
                }
                scene_path = Some(value(&mut args, "--scene", "<path>")?);
            }
            "--view" => {
                if view.is_some() {
                    return Err("Повторный аргумент --view".into());
                }
                let value = value(&mut args, "--view", "ez|abs|rms")?;
                view = Some(match value.as_str() {
                    "ez" => FieldMode::Ez,
                    "abs" => FieldMode::Absolute,
                    "rms" => FieldMode::Rms,
                    _ => {
                        return Err(format!(
                            "Недопустимый --view {value:?}; ожидается ez|abs|rms"
                        ));
                    }
                });
            }
            "--help" => {
                if help {
                    return Err("Повторный аргумент --help".into());
                }
                help = true;
            }
            _ => return Err(format!("Неизвестный аргумент {arg:?}")),
        }
    }
    if help {
        Ok(Action::Help)
    } else {
        Ok(Action::Run(Options {
            scene_path,
            view: view.unwrap_or_default(),
        }))
    }
}

fn utf8(arg: OsString) -> Result<String, String> {
    arg.into_string()
        .map_err(|arg| format!("Аргумент {arg:?} должен быть в UTF-8"))
}

fn value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &str,
    expected: &str,
) -> Result<String, String> {
    let value = args.next().map(utf8).transpose()?;
    value
        .filter(|value| !value.is_empty() && !value.starts_with("--"))
        .ok_or_else(|| format!("{flag} требует значение {expected}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, path::PathBuf};
    use volna::{model::Scene, ui::FieldMode};

    fn run_options(args: &[&str]) -> Options {
        let Action::Run(options) = parse(args.iter().map(OsString::from)).unwrap() else {
            panic!("ожидался запуск, не справка");
        };
        options
    }

    #[test]
    fn no_arguments_keep_interference_demo_and_ez() {
        let options = run_options(&[]);
        assert_eq!(options.scene_path, None);
        assert_eq!(options.view, FieldMode::Ez);
        let startup = options.load().unwrap();
        assert_eq!(startup.scene, Scene::interference_demo());
        assert_eq!(startup.view, FieldMode::Ez);
        assert_eq!(startup.scene_path, "presets/interference_10ghz.json");
    }

    #[test]
    fn scene_and_all_views_work_in_either_order() {
        for (name, mode) in [
            ("ez", FieldMode::Ez),
            ("abs", FieldMode::Absolute),
            ("rms", FieldMode::Rms),
        ] {
            for args in [
                ["--scene", "сцена с пробелами.json", "--view", name],
                ["--view", name, "--scene", "сцена с пробелами.json"],
            ] {
                let options = run_options(&args);
                assert_eq!(
                    options.scene_path.as_deref(),
                    Some("сцена с пробелами.json")
                );
                assert_eq!(options.view, mode);
            }
        }
        assert_eq!(run_options(&["--scene", "scene.json"]).view, FieldMode::Ez);
        let startup = run_options(&["--view", "rms"]).load().unwrap();
        assert_eq!(startup.scene, Scene::interference_demo());
        assert_eq!(startup.view, FieldMode::Rms);
    }

    #[test]
    fn help_does_not_load_files() {
        for args in [
            vec!["--help"],
            vec!["--help", "--scene", "missing.json", "--view", "abs"],
            vec!["--scene", "missing.json", "--help"],
        ] {
            assert!(matches!(
                parse(args.into_iter().map(OsString::from)),
                Ok(Action::Help)
            ));
        }
        for text in ["Usage:", "--scene <path>", "--view ez|abs|rms", "--help"] {
            assert!(USAGE.contains(text), "справка должна содержать {text}");
        }
    }

    #[test]
    fn rejects_unknown_missing_duplicate_and_invalid_arguments() {
        let cases: &[(&[&str], &str)] = &[
            (&["--unknown"], "Неизвестный аргумент"),
            (&["scene.json"], "Неизвестный аргумент"),
            (&["--scene=scene.json"], "Неизвестный аргумент"),
            (&["--scene"], "--scene требует"),
            (&["--view"], "--view требует"),
            (&["--scene", ""], "--scene требует"),
            (&["--view", ""], "--view требует"),
            (&["--scene", "--view", "rms"], "--scene требует"),
            (&["--view", "--scene", "scene.json"], "--view требует"),
            (
                &["--scene", "a", "--scene", "b"],
                "Повторный аргумент --scene",
            ),
            (
                &["--view", "ez", "--view", "ez"],
                "Повторный аргумент --view",
            ),
            (
                &["--view", "ez", "--view", "abs"],
                "Повторный аргумент --view",
            ),
            (&["--help", "--help"], "Повторный аргумент --help"),
            (&["--view", "RMS"], "Недопустимый --view"),
            (&["--view", "other"], "Недопустимый --view"),
            (&["--help", "--unknown"], "Неизвестный аргумент"),
            (&["--unknown", "--help"], "Неизвестный аргумент"),
            (&["--help", "--view", "other"], "Недопустимый --view"),
            (&["--help", "--scene"], "--scene требует"),
        ];
        for (args, expected) in cases {
            let error = parse(args.iter().map(OsString::from)).unwrap_err();
            assert!(error.contains(expected), "{args:?}: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_return_errors_instead_of_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let invalid = OsString::from_vec(vec![0xff]);
        for args in [
            vec![invalid.clone()],
            vec!["--view".into(), invalid.clone()],
            vec!["--scene".into(), invalid],
        ] {
            assert!(parse(args).unwrap_err().contains("UTF-8"));
        }
    }

    #[test]
    fn selected_file_is_loaded_instead_of_demo() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/waveguide_10ghz.json");
        let path = path.to_str().unwrap();
        let startup = run_options(&["--scene", path, "--view", "abs"])
            .load()
            .unwrap();
        assert_eq!(startup.scene, Scene::default());
        assert_ne!(startup.scene, Scene::interference_demo());
        assert_eq!(startup.view, FieldMode::Absolute);
        assert_eq!(startup.scene_path, path);
    }

    #[test]
    fn file_errors_include_path_and_never_fall_back() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for path in [
            root.join("Cargo.toml/missing.json"),
            root.join("Cargo.toml"),
            root.join("presets"),
        ] {
            let path = path.to_str().unwrap();
            let error = run_options(&["--scene", path]).load().unwrap_err();
            assert!(error.contains(path), "нет пути: {error}");
            assert!(error.contains("Не удалось загрузить сцену"), "{error}");
        }
    }
}
