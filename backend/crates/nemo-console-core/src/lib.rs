//! Nemo Console core: the pure, deterministic part of the backend that the browser also runs (ADR-0022).
//!
//! What lives here: the tomllib and PyYAML ports (`services::{pytoml, pyyaml, pyobj, pyutil, pyfmt, pystr}`,
//! `api::pyjson`), the secret scan the save path runs (`services::common`), the config-file schemas of the four
//! systems whose files the console validates itself (`schemas`), and [`check`], which composes them the way the
//! backend's validate route does. The module paths mirror the backend's (`services::pyyaml`, `api::pyjson`, ...) so
//! the code moved without edits and `nemo-console` re-exports every module at its old path.
//!
//! Rules: no async runtime, no file system, no clock, no threads on wasm32, no process or network access. The crate
//! compiles natively and for `wasm32-unknown-unknown` (`crates/nemo-console-wasm`), and the same test vectors
//! (`tests/vectors/`) run in both.

pub mod adapter_error;
pub mod diagnostics;
pub mod schemas;

/// The backend's `services::*` modules that are pure (same paths as in `nemo-console`).
pub mod services {
    pub mod common;
    pub mod pyfmt;
    pub mod pyobj;
    pub mod pystr;
    pub mod pytoml;
    pub mod pyutil;
    pub mod pyyaml;
}

/// The backend's `api::pyjson` (Python's `json` module), used by `services::pyutil`.
pub mod api {
    pub mod pyjson;
}
