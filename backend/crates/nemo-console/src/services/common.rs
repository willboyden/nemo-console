//! Redaction, secret detection and small helpers shared by the whole core. Port of `services/common.py`.
//!
//! The pure part (patterns, [`redact`], [`find_secret`], ...) lives in `nemo-console-core` since ADR-0022, so the
//! browser's live config checks run the exact secret scan the save path runs; it is re-exported here unchanged, so
//! every `crate::services::common::*` path keeps working. Only [`contained`] stays here: it needs `tracing` and an
//! async runtime, which the WebAssembly build does not have.

pub use nemo_console_core::services::common::*;

/// Run one unit of background work (a poller tick, a probe pass, a tool run) so that a panic is contained like
/// Python's `except Exception: log.exception(...)`: caught, logged once with the message redacted, and the task
/// carries on (`None`) instead of dying silently (audit finding 3).
pub async fn contained<T>(what: &str, fut: impl std::future::Future<Output = T>) -> Option<T> {
    use futures::FutureExt as _;
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(v) => Some(v),
        Err(p) => {
            tracing::error!("{what} failed: {}", redact(&panic_text(p.as_ref())));
            None
        }
    }
}
