//! SystemAdapter port: the single seam between the system-agnostic core and one NeMo system.
//! Port of `adapters/base.py`. Contract (PORTING.md section 6.3): workstreams implement this trait; they never change it.
//!
//! Rules every adapter MUST follow (each is enforced by the core or by tests):
//!   * Never shell out or touch docker/systemd directly: use `ctx.runner` (argv list, binary allowlist, timeouts,
//!     output caps). Never build a shell string.
//!   * Never read secret files. Credentials are referenced by env-var NAME only (`ctx.secret_env`).
//!   * Read paths: only the fixed set handed in via `ctx.paths`; resolve + containment-check every path.
//!   * Failure is data: a down system yields `health="down"` + a Problem with a `fix`, never an `Err` out of
//!     `status()` / `metrics()` unless the Python adapter would raise there.
//!   * Never fabricate a metric: if a canonical key cannot be read, omit it.
//!
//! Python exceptions map onto [`AdapterError`] variants; the core turns them into the same HTTP answers and the same
//! `f"{type(e).__name__}: {e}"` texts the Python core produces.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, ConfigFile, ConfigFileSpec, MetricsSnapshot, ParamSpec,
    PlaygroundSpec, SystemEvent, SystemId, SystemStatus, ValidationResult,
};
use crate::services::http::Http;
use crate::services::pyfmt::str_repr;

/// Result of one command. stdout/stderr are already redacted and capped by the runner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandResult {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: u64,
}

impl CommandResult {
    #[must_use]
    pub fn new(returncode: i32, stdout: &str, stderr: &str) -> Self {
        CommandResult {
            returncode,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            timed_out: false,
            duration_ms: 0,
        }
    }
}

/// Options of `CommandRunner.run` (Python keyword arguments, same defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct RunOpts {
    /// Seconds; capped by the runner per argv (see services/runner.rs `max_timeout`).
    pub timeout: f64,
    pub max_bytes: usize,
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            timeout: 15.0,
            max_bytes: 256_000,
            stdin: None,
            cwd: None,
        }
    }
}

impl RunOpts {
    #[must_use]
    pub fn timeout(timeout: f64) -> Self {
        RunOpts {
            timeout,
            ..RunOpts::default()
        }
    }

    #[must_use]
    pub fn max_bytes(mut self, n: usize) -> Self {
        self.max_bytes = n;
        self
    }

    #[must_use]
    pub fn stdin(mut self, data: Vec<u8>) -> Self {
        self.stdin = Some(data);
        self
    }

    #[must_use]
    pub fn cwd(mut self, dir: PathBuf) -> Self {
        self.cwd = Some(dir);
        self
    }
}

/// Refused before spawning, or the spawn itself failed. `py_class()` names the Python exception the Python runner
/// raises in the same situation (adapters print it).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunnerError {
    /// `RunnerError(...)`: bad binary, not on PATH, bad argv.
    #[error("{0}")]
    Refused(String),
    /// An `OSError` subclass from the spawn (class name, message), e.g. ("FileNotFoundError", "...").
    #[error("{1}")]
    Os(&'static str, String),
}

impl RunnerError {
    #[must_use]
    pub fn py_class(&self) -> &'static str {
        match self {
            RunnerError::Refused(_) => "RunnerError",
            RunnerError::Os(c, _) => c,
        }
    }
}

/// Provided by the core. Only allowlisted binaries run. `argv[0]` is the bare binary name; there is no shell.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, argv: &[String], opts: RunOpts) -> Result<CommandResult, RunnerError>;
}

/// Everything an adapter may touch. Built once by the core from settings; fakes in tests.
#[derive(Clone)]
pub struct AdapterContext {
    pub runner: Arc<dyn CommandRunner>,
    /// trust_env off, no redirects followed, httpx-compatible error classes.
    pub http: Http,
    /// Named base URLs, e.g. {"switchyard": "http://127.0.0.1:4010"} (effective at start).
    pub urls: IndexMap<String, String>,
    /// Named ABSOLUTE paths, config-only (`Settings::resolved_paths`).
    pub paths: IndexMap<String, PathBuf>,
    /// Logical name -> ENV VAR NAME (never a value).
    pub secret_env: IndexMap<String, String>,
    /// Console-owned writable dir (0700); an adapter may create subdirs.
    pub state_dir: PathBuf,
    /// The environment (never read `std::env` directly: tests pin it).
    pub environ: crate::services::environ::Environ,
}

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

impl From<RunnerError> for AdapterError {
    fn from(e: RunnerError) -> Self {
        AdapterError::failed(e.py_class(), e.to_string())
    }
}

/// A live event tail: ends when the adapter has nothing more, or with an error.
pub type EventStream = BoxStream<'static, Result<SystemEvent, AdapterError>>;

/// The adapter port (Python `SystemAdapter`). Default methods mirror the Python base-class defaults.
#[async_trait]
pub trait SystemAdapter: Send + Sync {
    fn id(&self) -> SystemId;
    fn label(&self) -> String;

    // ── observe ─────────────────────────────────────────────────────────────────────────────────
    async fn status(&self) -> Result<SystemStatus, AdapterError>;
    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError>;
    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError>;
    /// Live tail. Must end promptly when dropped; bounded memory.
    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream;

    // ── configure: files ────────────────────────────────────────────────────────────────────────
    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError>;
    /// Absolute path of an ALLOWLISTED file; `UnknownId` for an unknown id. The core does the compare-and-swap,
    /// backup and atomic write; the adapter never writes a config file.
    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError>;
    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError>;
    /// Validate CANDIDATE content without touching the live file.
    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError>;
    /// Called by the core AFTER it wrote the new content. Restart if asked, then health-check. `post_check =
    /// "unhealthy"` makes the core restore the backup and call this again.
    async fn post_apply(&self, file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError>;

    // ── configure: structured parameters (optional; Dynamo) ─────────────────────────────────────
    fn param_catalog(&self) -> Result<Vec<ParamSpec>, AdapterError> {
        Ok(Vec::new())
    }
    async fn preview_params(&self, _values: &Map<String, Value>) -> Result<String, AdapterError> {
        Err(AdapterError::NotImplemented)
    }
    async fn apply_params(
        &self,
        _values: &Map<String, Value>,
    ) -> Result<ApplyResult, AdapterError> {
        Err(AdapterError::NotImplemented)
    }
    /// Optional extra (Dynamo `param_warnings`): `None` when the adapter has no such hook.
    fn param_warnings(
        &self,
        _values: &Map<String, Value>,
    ) -> Option<Result<Vec<String>, AdapterError>> {
        None
    }

    // ── act ─────────────────────────────────────────────────────────────────────────────────────
    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError>;
    /// Unknown id -> `UnknownId`. Params are pre-validated against the ActionSpec by the core.
    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError>;

    // ── interact ────────────────────────────────────────────────────────────────────────────────
    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        Ok(None)
    }
    /// Absolute URL the core POSTs a playground request to (core adds the credential).
    fn playground_url(&self) -> Option<String> {
        None
    }

    // ── Data Designer run artifacts (addendum 9.7): other systems answer 404 ────────────────────
    /// `records_page(run_id, limit, offset)`; `UnknownId` -> 404 "unknown run".
    async fn records_page(
        &self,
        _run_id: &str,
        _limit: i64,
        _offset: i64,
    ) -> Result<Value, AdapterError> {
        Err(AdapterError::NoSuchEndpoint)
    }
    /// `run_files(run_id)`; `UnknownId` -> 404 "unknown run".
    async fn run_files(&self, _run_id: &str) -> Result<Value, AdapterError> {
        Err(AdapterError::NoSuchEndpoint)
    }
}

/// Shared handle to an adapter.
pub type AdapterRef = Arc<dyn SystemAdapter>;
