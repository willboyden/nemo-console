//! Typed errors mapped to the contract's `{error:{code,message}}` envelope. Port of `services/errors.py`.
//! Contract (PORTING.md section 6.2). `api::errors::problem` renders one (message redacted, capped at 1200 chars).

/// An HTTP-level refusal: status, stable code, human message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{status} {code}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    #[must_use]
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// The last-line-of-defence 500 (`internal_error`, "internal error").
    #[must_use]
    pub fn internal() -> Self {
        ApiError::new(500, "internal_error", "internal error")
    }
}
