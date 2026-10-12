//! Двумерная модель Максвелла TMz в вакууме с PEC и CPML.

pub mod model;
pub mod persistence;
pub mod runtime;
pub mod solver;
pub mod sources;
#[cfg(feature = "gui")]
pub mod ui;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Некорректная сцена: {0}")]
    Invalid(String),
    #[error("Ошибка выражения: {0}")]
    Expression(String),
    #[error("Расчёт остановлен: {0}")]
    Numerical(String),
    #[error("Ошибка файла: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ошибка формата JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
