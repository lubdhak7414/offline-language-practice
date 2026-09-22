//! Unified backend error type.
//!
//! Only `db.rs` and new scheduler/lib code paths use [`AppError`] so far;
//! older `String`-error call sites are left untouched (minimal churn).
//! Tauri commands convert via `map_err(String::from)`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("ASR error: {0}")]
    Asr(String),
    #[error("TTS error: {0}")]
    Tts(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("scheduler error: {0}")]
    Scheduler(String),
    #[error("engine busy: {0}")]
    Busy(String),
    #[error("bad input: {0}")]
    BadInput(String),
    #[error("network error: {0}")]
    Network(String),
}

impl From<AppError> for String {
    fn from(e: AppError) -> String {
        e.to_string()
    }
}
