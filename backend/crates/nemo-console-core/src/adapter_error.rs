//! [`AdapterError`]: the Python exceptions an adapter method can raise, as data. Moved here from
//! `nemo-console`'s `adapters/base.rs` (ADR-0022) because the config-file validators in [`crate::schemas`] return it;
//! the backend re-exports it at its old path, and adds `From<RunnerError>` there.

use crate::services::pyfmt::str_repr;

/// Python exceptions an adapter method can raise, as data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// `KeyError(id)`: unknown file id / action id / run id.
    #[error("{}", str_repr(.0))]
    UnknownId(String),
    /// `FileNotFoundError` from read_config.
    #[error("{0}")]
    FileNotFound(String),
    /// `NotImplementedError` (params not supported).
    #[error("")]
    NotImplemented,
    /// `ValueError(msg)`: the adapter refuses the input (preview of params -> 422 params_invalid).
    #[error("{0}")]
    Invalid(String),
    /// The route exists only for another system (Python's `isinstance` check failed).
    #[error("no such endpoint for this system")]
    NoSuchEndpoint,
    /// `TimeoutError` (str() is empty in Python).
    #[error("")]
    Timeout,
    /// Any other exception: its Python class name and `str(e)`.
    #[error("{message}")]
    Failed { class: String, message: String },
}

impl AdapterError {
    #[must_use]
    pub fn failed(class: &str, message: impl Into<String>) -> Self {
        AdapterError::Failed {
            class: class.to_owned(),
            message: message.into(),
        }
    }

    /// `type(e).__name__`.
    #[must_use]
    pub fn py_class(&self) -> &str {
        match self {
            AdapterError::UnknownId(_) => "KeyError",
            AdapterError::FileNotFound(_) => "FileNotFoundError",
            AdapterError::NotImplemented => "NotImplementedError",
            AdapterError::Invalid(_) => "ValueError",
            AdapterError::NoSuchEndpoint => "LookupError",
            AdapterError::Timeout => "TimeoutError",
            AdapterError::Failed { class, .. } => class,
        }
    }

    /// `f"{type(e).__name__}: {e}"`.
    #[must_use]
    pub fn py_text(&self) -> String {
        format!("{}: {self}", self.py_class())
    }
}
