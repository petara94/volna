//! Строгий JSON сцены и атомарный файловый экспорт без неявной перезаписи.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::model::Scene;
use crate::sources::CompiledSource;
use crate::{Error, Result};

const MAX_SCENE_BYTES: u64 = 8 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn validate_scene(scene: &Scene) -> Result<()> {
    scene.validate()?;
    for source in &scene.sources {
        CompiledSource::new(source)?;
    }
    Ok(())
}

/// Загружает не более 8 МиБ, проверяя схему, геометрию и все выражения источников.
pub fn load_scene(path: &Path) -> Result<Scene> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_SCENE_BYTES {
        return Err(Error::Invalid("Размер файла сцены превышает 8 МиБ".into()));
    }
    // take защищает также от увеличения файла между metadata и чтением.
    let mut bytes = Vec::new();
    file.take(MAX_SCENE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SCENE_BYTES {
        return Err(Error::Invalid("Размер файла сцены превышает 8 МиБ".into()));
    }
    let scene: Scene = serde_json::from_slice(&bytes)?;
    validate_scene(&scene)?;
    Ok(scene)
}

/// Публикует полностью записанный файл в том же каталоге.
/// Без overwrite используется hard_link: существующий путь не заменяется даже при гонке.
/// Файловая система должна поддерживать hard links; небезопасного fallback нет.
/// С overwrite атомарный rename заменяет запись каталога, а не содержимое цели symlink.
/// На Unix после публикации синхронизируется каталог; её сбой возвращает явную ошибку
/// «Файл опубликован, но …»: новый файл при этом уже существует. Абсолютная сохранность
/// при отключении питания для всех платформ/файловых систем не обещается.
pub fn save_scene(path: &Path, scene: &Scene, overwrite: bool) -> Result<()> {
    validate_scene(scene)?;
    let bytes = serde_json::to_vec_pretty(scene)?;
    if bytes.len() as u64 > MAX_SCENE_BYTES {
        return Err(Error::Invalid(
            "Сериализованная сцена превышает 8 МиБ".into(),
        ));
    }
    atomic_write(path, overwrite, |writer| {
        writer.write_all(&bytes)?;
        Ok(())
    })
}

/// SHA-256 компактного JSON, без pretty-print и метаданных исполнения.
/// Порядок полей структур и элементов массивов детерминирован; порядок объектов сцены значим.
pub fn scene_hash(scene: &Scene) -> Result<String> {
    validate_scene(scene)?;
    let bytes = serde_json::to_vec(scene)?;
    let hash = Sha256::digest(&bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in hash {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(text)
}

/// Экспортирует синхронные отсчёты Ez (В/м); time_s — секунды.
/// Каждая строка содержит ровно одно значение для каждого имени в том же порядке.
/// Имена всегда заключены в кавычки и получают безопасный префикс Ez_V_m:,
/// поэтому имя, начинающееся с =, +, -, @ или whitespace, не становится CSV-формулой.
pub fn export_probes(
    path: &Path,
    names: &[String],
    samples: &[(f64, Vec<f64>)],
    overwrite: bool,
) -> Result<()> {
    export_probes_inner(path, names, samples, overwrite, None)
}

/// Вариант CSV с первой строкой-комментарием `# metadata: {compactJSON}`.
/// JSON и таблица публикуются одним атомарным файлом; sidecar не создаётся.
/// Стандартному CSV-reader следует пропустить первую строку перед чтением заголовка.
pub fn export_probes_with_metadata(
    path: &Path,
    names: &[String],
    samples: &[(f64, Vec<f64>)],
    overwrite: bool,
    metadata: &serde_json::Value,
) -> Result<()> {
    export_probes_inner(path, names, samples, overwrite, Some(metadata))
}

fn export_probes_inner(
    path: &Path,
    names: &[String],
    samples: &[(f64, Vec<f64>)],
    overwrite: bool,
    metadata: Option<&serde_json::Value>,
) -> Result<()> {
    let mut previous = None;
    for (time, values) in samples {
        if !time.is_finite() || *time < 0.0 || previous.is_some_and(|previous| *time <= previous) {
            return Err(Error::Invalid(
                "Время отсчётов должно быть конечным, неотрицательным и строго возрастать".into(),
            ));
        }
        if values.len() != names.len() || values.iter().any(|value| !value.is_finite()) {
            return Err(Error::Invalid(
                "Каждый кадр должен содержать конечное значение Ez для каждого зонда".into(),
            ));
        }
        previous = Some(*time);
    }
    atomic_write(path, overwrite, |writer| {
        if let Some(metadata) = metadata {
            writer.write_all(b"# metadata: ")?;
            serde_json::to_writer(&mut *writer, metadata)?;
            writer.write_all(b"\n")?;
        }
        writer.write_all(b"time_s")?;
        for name in names {
            writer.write_all(b",\"Ez_V_m:")?;
            for part in name.split_inclusive('"') {
                writer.write_all(part.as_bytes())?;
                if part.ends_with('"') {
                    writer.write_all(b"\"")?;
                }
            }
            writer.write_all(b"\"")?;
        }
        writer.write_all(b"\n")?;
        for (time, values) in samples {
            write!(writer, "{time}")?;
            for value in values {
                write!(writer, ",{value}")?;
            }
            writer.write_all(b"\n")?;
        }
        Ok(())
    })
}

struct TempFile {
    path: PathBuf,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        // Очистка при ошибке не должна подменять её первопричину.
        let _ = fs::remove_file(&self.path);
    }
}

fn atomic_write(
    path: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<()>,
) -> Result<()> {
    if path.file_name().is_none() {
        return Err(Error::Invalid("Необходимо указать имя файла".into()));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (temporary, file) = create_temporary(parent, path)?;
    let mut writer = BufWriter::new(file);
    write(&mut writer)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    if overwrite {
        fs::rename(&temporary.path, path)?;
    } else {
        // В отличие от exists()+rename, hard_link атомарно отказывает при существующей цели,
        // в том числе при dangling symlink. Временный файл находится на том же томе.
        fs::hard_link(&temporary.path, path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Invalid("Файл уже существует; перезапись необходимо разрешить явно".into())
            } else {
                Error::Io(error)
            }
        })?;
    }
    // После публикации временное имя — только уборка: её сбой не отменяет успех записи.
    drop(temporary);
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::Io(std::io::Error::new(
                error.kind(),
                format!(
                    "Файл {} опубликован, но синхронизация каталога {} не выполнена: {error}",
                    path.display(),
                    parent.display()
                ),
            ))
        })?;
    Ok(())
}

fn create_temporary(parent: &Path, destination: &Path) -> Result<(TempFile, File)> {
    for _ in 0..128 {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let name = format!(".volna-tmp-{}-{serial}", std::process::id());
        // Не выбираем саму цель даже на файловой системе без учёта регистра.
        if destination.file_name().is_some_and(|destination| {
            destination
                .as_encoded_bytes()
                .eq_ignore_ascii_case(name.as_bytes())
        }) {
            continue;
        }
        let path = parent.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((TempFile { path }, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Не удалось выбрать свободное имя временного файла",
    )))
}
