//! [`Severity`] and [`Diagnostic`]: one finding of a config-file validator. Moved here from `nemo-console`'s
//! `domain/models.rs` (ADR-0022); the backend re-exports them there. Contract v1: field names, order and JSON are
//! unchanged (PORTING.md section 6.1).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub line: Option<i64>,
}
