//! Core services (owned at integration). Stable APIs: PORTING.md section 6.

pub mod audit;
pub mod auth;
pub mod common;
pub mod config_store;
pub mod difflib;
pub mod engines;
pub mod environ;
pub mod errors;
pub mod events;
pub mod gzip;
pub mod http;
pub mod metrics;
pub mod missing;
pub mod params;
pub mod playground;
pub mod plumbing;
pub mod pyvalidate;
pub mod routing;
pub mod runner;
pub mod settings;
pub mod shlex;
pub mod status;
pub mod store;
pub mod support;
pub mod telemetry;
pub mod topology;

// The pure modules live in nemo-console-core since ADR-0022 (the browser runs them as WebAssembly); same paths here.
pub use nemo_console_core::services::{pyfmt, pyobj, pystr, pytoml, pyutil, pyyaml};
