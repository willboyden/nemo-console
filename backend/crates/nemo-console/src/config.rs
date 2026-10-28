//! Console settings: env (`NEMO_CONSOLE_*`) plus an optional user env file. Port of `config.py` (pydantic-settings).
//!
//! Contract (PORTING.md section 4). Sources, lowest to highest precedence: defaults < the user env file
//! (`~/.config/nemo-console/env`, dotenv format, skipped when `NEMO_CONSOLE_NO_USER_ENV` is set) < the process
//! environment. Names are case-insensitive; list/dict/model fields take JSON; `NEMO_CONSOLE_<FIELD>__<KEY>=v` sets one
//! key of a dict field (deep-merged across sources). Values are validated with pydantic's lax rules, then each field
//! validator of the Python class runs. Secrets are referenced by env-var NAME only.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::services::pyfmt::{char_len, char_prefix, str_repr};
use crate::services::pyvalidate::{
    Loc, PyError, lax_bool, lax_float, lax_int, msg_ge, msg_le, msg_pattern, strict_str,
};

/// `nemo_console.__version__`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ENV_PREFIX: &str = "NEMO_CONSOLE_";
pub const NO_USER_ENV: &str = "NEMO_CONSOLE_NO_USER_ENV";

pub const LIGHTNING: &str = "ai-lab-vllm-nemotron-lightning-nemotron-lightning-1";
pub const FLASHNEXT: &str = "ai-lab-sglang-flashnext-flashnext-1";
pub const QWEN38: &str = "ai-lab-vllm-qwen38-vllm-1";
pub const BOLT: &str = "ai-lab-vllm-nemotron-bolt-nemotron-bolt-1";
pub const GLM: &str = "lil-glm53-tp2";
pub const CHATTERBOX: &str = "ai-lab-chatterbox-tts-tts-1";
pub const DYNAMO_WORKER: &str = "ai-lab-dynamo-dynamo-worker-1";

pub const DEFAULT_URLS: [(&str, &str); 8] = [
    ("switchyard", "http://127.0.0.1:4010"),
    ("relay", "http://127.0.0.1:4040"),
    ("dynamo", "http://127.0.0.1:8020"),
    ("dynamo_worker_metrics", "http://127.0.0.1:8081"),
    ("litellm", "http://127.0.0.1:4000"),
    ("dcgm", "http://127.0.0.1:9400"),
    ("prometheus", "http://127.0.0.1:9090"),
    ("guardrails", "http://127.0.0.1:8806"),
];
pub const DEFAULT_ENGINE_PROBES: [(&str, &str); 5] = [
    ("nemotron-3.5-super-vl", DYNAMO_WORKER),
    ("nemotron-3.5-bolt", BOLT),
    ("glm-5.3-flash-next-local", GLM),
    ("qwen3.8-flash-next", FLASHNEXT),
    ("qwen3.8-27b-1m", QWEN38),
];
pub const DEFAULT_ENGINE_START: [(&str, &str); 5] = [
    (
        DYNAMO_WORKER,
        "docker compose -f inference/dynamo/docker-compose.yml -f inference/dynamo/docker-compose.super-vl.yml up -d dynamo-frontend dynamo-worker   # Super VL, both cards: start lightning (TP2) first",
    ),
    (
        BOLT,
        "scripts/use-model.sh nemotron-3.5-bolt   # exclusive: this stops Super VL",
    ),
    (
        GLM,
        "scripts/use-model.sh glm53-flash-next   # exclusive, both cards: this stops Super VL",
    ),
    (
        FLASHNEXT,
        "inference/sglang-flashnext/flashnext.sh up   # only if flashnext should be resident: it takes BOTH cards",
    ),
    (
        QWEN38,
        "scripts/use-model.sh qwen3.8-27b-1m   # exclusive: this stops the engine on its card",
    ),
];
pub const DEFAULT_DYNAMO_START: &str = "scripts/use-model.sh qwen3.8-27b-dynamo";
pub const DEFAULT_MODEL_START: &str = "scripts/use-model.sh   # lists the engines; `scripts/use-model.sh <alias>` switches away from Dynamo";
pub const DEFAULT_DYNAMO_EXCLUSIVE: [&str; 4] = [BOLT, GLM, FLASHNEXT, QWEN38];
pub const DEFAULT_DYNAMO_COEXISTING: [&str; 3] = [LIGHTNING, "ai-lab-ollama-ollama-1", CHATTERBOX];
pub const DEFAULT_DYNAMO_IGNORED: [&str; 1] = ["ai-lab-observability-dcgm-exporter-1"];
pub const DEFAULT_DYNAMO_DARK: [&str; 4] = [
    "nemotron-3.5-bolt (route bolt)",
    "glm-5.3-flash-next-local",
    "qwen3.8-flash-next and -quick (auto-local and auto-flashnext capable tier)",
    "qwen3.8-27b-1m (direct-27b)",
];
pub const DEFAULT_KEY_ENV: [(&str, &str); 2] = [
    ("switchyard", "LITELLM_API_KEY"),
    ("relay", "LITELLM_API_KEY"),
];
pub const DCGM_EXPORTER_CONTAINER: &str = "ai-lab-observability-dcgm-exporter-1";
pub const DCGM_DIAG_IMAGE: &str = "nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1-ubuntu24.04@sha256:48464e50aa76671f5b204264cb4999739591112bf74330a727523f5655c6d7f2";
pub const DCGM_PROMETHEUS_JOBS: [&str; 3] = ["dcgm", "dcgm-aitop100", "dcgm-sparks"];
pub const OPENSHELL_CONTAINERS: [&str; 2] = ["ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"];
pub const GUARDRAILS_CONTAINER: &str = "ai-lab-nemo-guardrails";
pub const SWITCHYARD_CONTAINER: &str = "ai-lab-switchyard-switchyard-1";
pub const SWITCHYARD_COMPOSE_PROJECT: &str = "ai-lab-switchyard";
pub const SWITCHYARD_COMPOSE_SERVICE: &str = "switchyard";
pub const SWITCHYARD_IMAGE: &str = "ai-lab-switchyard:0.3.0";
pub const SWITCHYARD_LOG_PATH: &str = "/var/log/switchyard/routing.jsonl";
pub const RELAY_UNIT: &str = "nemo-relay-gateway.service";
pub const DYNAMO_COMPOSE_PROJECT: &str = "ai-lab-dynamo";
pub const DYNAMO_ROLES: [&str; 4] = ["frontend", "worker", "mocker", "sv_frontend"];
pub const DEFAULT_DYNAMO_SERVICES: [(&str, &str); 4] = [
    ("frontend", "dynamo-frontend"),
    ("worker", "dynamo-worker"),
    ("mocker", "dynamo-mocker"),
    ("sv_frontend", "dynamo-sv-frontend"),
];
pub const DEFAULT_DYNAMO_CONTAINERS: [(&str, &str); 4] = [
    ("frontend", "ai-lab-dynamo-dynamo-frontend-1"),
    ("worker", DYNAMO_WORKER),
    ("mocker", "ai-lab-dynamo-dynamo-mocker-1"),
    ("sv_frontend", "ai-lab-dynamo-dynamo-sv-frontend-1"),
];
pub const GUARDRAILS_COMPOSE_PROJECT: &str = "ai-lab-nemo-guardrails";
pub const DCGM_COMPOSE_PROJECT: &str = "ai-lab-observability";
pub const DEFAULT_LAYOUT: [(&str, &str); 12] = [
    ("switchyard", "inference/switchyard"),
    ("relay", "clients/nemo-relay"),
    ("dynamo", "inference/dynamo"),
    ("skills", ".claude/skills"),
    ("data_designer", "clients/data-designer"),
    ("openshell_runner", "clients/data-designer/runner"),
    ("observability", "observability"),
    ("aiperf", "clients/aiperf"),
    ("guardrails", "security/nemo-guardrails"),
    ("evaluator", "clients/nemo-evaluator"),
    ("skillevaluator", "clients/skillevaluator"),
    ("ollama", "inference/ollama"),
];

#[allow(clippy::expect_used)]
fn full(p: &str) -> Regex {
    Regex::new(&format!(r"\A(?:{p})\z")).expect("static regex")
}

pub static CONTAINER_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9_.\-]{0,127}"));
pub static IMAGE_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[a-z0-9][a-z0-9._/:\-]{0,254}(@sha256:[0-9a-f]{64})?"));
pub static COMPOSE_PROJECT_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[a-z0-9][a-z0-9_\-]{0,62}"));
pub static COMPOSE_SERVICE_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9_.\-]{0,62}"));
pub static UNIT_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9@_.:\-]{0,240}\.service"));
pub static IN_CONTAINER_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"(/[A-Za-z0-9_.\-]+){1,16}"));
pub static LAYOUT_SEGMENT_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9_.\-]{1,64}"));
pub static CREDENTIAL_ENV_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Z_][A-Z0-9_]{0,63}"));

/// Only credential-looking env names may be referenced: LITELLM_*, NEMO_CONSOLE_PLAYGROUND_*, or *_API_KEY.
#[must_use]
pub fn credential_env_allowed(name: &str) -> bool {
    CREDENTIAL_ENV_RE.is_match(name)
        && (name.starts_with("LITELLM_")
            || name.starts_with("NEMO_CONSOLE_PLAYGROUND_")
            || name.ends_with("_API_KEY"))
}

/// A repo-relative directory: '/'-separated plain segments, no '.', '..', absolute path, backslash or NUL.
pub fn layout_rel(key: &str, value: &str) -> Result<String, String> {
    let parts: Vec<&str> = if !value.is_empty() && !value.starts_with('/') {
        value.trim_matches('/').split('/').collect()
    } else {
        Vec::new()
    };
    if parts.is_empty()
        || parts.len() > 8
        || parts
            .iter()
            .any(|p| *p == "." || *p == ".." || !LAYOUT_SEGMENT_RE.is_match(p))
    {
        return Err(format!(
            "layout {}: not a relative directory under the repo root: {}",
            str_repr(key),
            str_repr(char_prefix(value, 80))
        ));
    }
    Ok(parts.join("/"))
}

/// One engine node of the topology. `id` and `container` are identifiers; `label` is display text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyEngine {
    pub id: String,
    pub label: String,
    pub container: String,
    pub opt_in: bool,
}

#[must_use]
pub fn default_topology_engines() -> Vec<TopologyEngine> {
    let e = |id: &str, label: &str, container: &str, opt_in: bool| TopologyEngine {
        id: id.to_owned(),
        label: label.to_owned(),
        container: container.to_owned(),
        opt_in,
    };
    vec![
        e(
            "vllm-lightning",
            "vLLM Lightning (efficient tier, judge)",
            LIGHTNING,
            false,
        ),
        e(
            "vllm-bolt",
            "vLLM Nemotron Bolt (opt-in, one card)",
            BOLT,
            true,
        ),
        e(
            "vllm-glm53",
            "vLLM GLM-5.3-Flash-Next (opt-in, both cards)",
            GLM,
            true,
        ),
        e(
            "sglang-flashnext",
            "SGLang FlashNext (opt-in, both cards)",
            FLASHNEXT,
            true,
        ),
        e("vllm-qwen38", "vLLM Qwen3.8 27B (opt-in)", QWEN38, true),
    ]
}

fn map_of(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn vec_of(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// `Path.home()`: $HOME, else the passwd entry.
#[must_use]
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|u| u.dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.config/nemo-console/env`, unless disabled (tests set NEMO_CONSOLE_NO_USER_ENV=1).
#[must_use]
pub fn user_env_file() -> Option<PathBuf> {
    user_env_file_in(&crate::services::environ::Environ::process())
}

/// [`user_env_file`] against a given environment (`HOME`, `NEMO_CONSOLE_NO_USER_ENV`); the test seam.
#[must_use]
pub fn user_env_file_in(environ: &crate::services::environ::Environ) -> Option<PathBuf> {
    match environ.get(NO_USER_ENV) {
        Some(v) if !v.is_empty() => None,
        _ => {
            let home = environ
                .get("HOME")
                .filter(|h| !h.is_empty())
                .map_or_else(home_dir, PathBuf::from);
            Some(home.join(".config").join("nemo-console").join("env"))
        }
    }
}

/// Standalone-safe: walk up from the executable to the directory holding the lab compose files, else the CWD.
/// (Python walks up from config.py; for a binary inside the checkout both walks pass the same ancestors.)
#[must_use]
pub fn default_repo_root() -> PathBuf {
    match std::env::current_exe() {
        Ok(exe) => default_repo_root_from(&exe, std::env::current_dir),
        Err(_) => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// [`default_repo_root`] walking up from `start` (Python: `__file__`), falling back to `cwd()` (test seam).
pub fn default_repo_root_from(
    start: &Path,
    cwd: impl FnOnce() -> std::io::Result<PathBuf>,
) -> PathBuf {
    let marker = Path::new("inference")
        .join("dynamo")
        .join("docker-compose.yml");
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    for parent in start.ancestors().skip(1) {
        if parent.join(&marker).is_file() {
            return parent.to_path_buf();
        }
    }
    cwd().unwrap_or_else(|_| PathBuf::from("."))
}

/// The checkout's `frontend/`: the first ancestor of the executable that holds `frontend/index.html` next to
/// `rust/` or `backend/`; else the directory this crate was built from (`<checkout>/frontend`).
#[must_use]
pub fn default_frontend_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) {
        for parent in exe.ancestors().skip(1) {
            if parent.join("frontend").join("index.html").is_file()
                && (parent.join("rust").is_dir() || parent.join("backend").is_dir())
            {
                return parent.join("frontend");
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("frontend")
}

/// The settings object. Every field of the Python `Settings` class, same names, same defaults.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    pub host: String,
    pub port: i64,
    pub state_dir: PathBuf,
    pub repo_root: PathBuf,
    pub frontend_dir: PathBuf,
    pub allowed_hosts: Vec<String>,
    pub urls: IndexMap<String, String>,
    pub allowed_url_hosts: Vec<String>,
    pub playground_key_env: IndexMap<String, String>,
    pub engine_probes: IndexMap<String, String>,
    pub engine_start: IndexMap<String, String>,
    pub topology_engines: Vec<TopologyEngine>,
    pub dynamo_opt_in: bool,
    pub dynamo_start: String,
    pub default_model_start: String,
    pub dynamo_exclusive_engines: Vec<String>,
    pub dynamo_coexisting_engines: Vec<String>,
    pub dynamo_ignored_containers: Vec<String>,
    pub dynamo_dark_while_held: Vec<String>,
    pub trust_loopback: bool,
    pub background_tasks: bool,
    pub poll_interval_s: f64,
    pub ring_seconds: i64,
    pub status_ttl_s: f64,
    pub adapter_timeout_s: f64,
    pub action_timeout_s: f64,
    pub action_timeouts: IndexMap<String, f64>,
    pub body_cap_bytes: i64,
    pub config_cap_bytes: i64,
    pub sse_max_connections: i64,
    pub sse_idle_timeout_s: f64,
    pub sse_heartbeat_s: f64,
    pub shutdown_grace_s: i64,
    pub playground_max_tokens: i64,
    pub revisions_keep: i64,
    pub otlp_endpoint: Option<String>,
    pub extra_bin_dirs: Vec<String>,
    pub dd_scripts_dir: PathBuf,
    pub dd_datasets_root: PathBuf,
    pub dd_config_dir: PathBuf,
    pub dcgm_exporter_container: String,
    pub dcgm_diag_image: String,
    pub dcgm_prometheus_jobs: Vec<String>,
    pub openshell_containers: Vec<String>,
    pub guardrails_container: String,
    pub switchyard_container: String,
    pub switchyard_compose_project: String,
    pub switchyard_compose_service: String,
    pub switchyard_image: String,
    pub switchyard_log_path: String,
    pub relay_unit: String,
    pub dynamo_compose_project: String,
    pub dynamo_services: IndexMap<String, String>,
    pub dynamo_containers: IndexMap<String, String>,
    pub guardrails_compose_project: String,
    pub dcgm_compose_project: String,
    pub layout: IndexMap<String, String>,
    pub capable_probe: bool,
    pub capable_probe_interval_s: f64,
    pub capable_probe_timeout_s: f64,
    pub capable_probe_retry_s: f64,
    pub capable_probe_key_env: String,
}

/// Field kinds, used for parsing env strings (complex fields take JSON) and nested `__` keys.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scalar,
    List,
    Dict,
}

/// (field name, kind) in the Python class's definition order (errors are reported in this order).
const FIELDS: &[(&str, Kind)] = &[
    ("host", Kind::Scalar),
    ("port", Kind::Scalar),
    ("state_dir", Kind::Scalar),
    ("repo_root", Kind::Scalar),
    ("frontend_dir", Kind::Scalar),
    ("allowed_hosts", Kind::List),
    ("urls", Kind::Dict),
    ("allowed_url_hosts", Kind::List),
    ("playground_key_env", Kind::Dict),
    ("engine_probes", Kind::Dict),
    ("engine_start", Kind::Dict),
    ("topology_engines", Kind::List),
    ("dynamo_opt_in", Kind::Scalar),
    ("dynamo_start", Kind::Scalar),
    ("default_model_start", Kind::Scalar),
    ("dynamo_exclusive_engines", Kind::List),
    ("dynamo_coexisting_engines", Kind::List),
    ("dynamo_ignored_containers", Kind::List),
    ("dynamo_dark_while_held", Kind::List),
    ("trust_loopback", Kind::Scalar),
    ("background_tasks", Kind::Scalar),
    ("poll_interval_s", Kind::Scalar),
    ("ring_seconds", Kind::Scalar),
    ("status_ttl_s", Kind::Scalar),
    ("adapter_timeout_s", Kind::Scalar),
    ("action_timeout_s", Kind::Scalar),
    ("action_timeouts", Kind::Dict),
    ("body_cap_bytes", Kind::Scalar),
    ("config_cap_bytes", Kind::Scalar),
    ("sse_max_connections", Kind::Scalar),
    ("sse_idle_timeout_s", Kind::Scalar),
    ("sse_heartbeat_s", Kind::Scalar),
    ("shutdown_grace_s", Kind::Scalar),
    ("playground_max_tokens", Kind::Scalar),
    ("revisions_keep", Kind::Scalar),
    ("otlp_endpoint", Kind::Scalar),
    ("extra_bin_dirs", Kind::List),
    ("dd_scripts_dir", Kind::Scalar),
    ("dd_datasets_root", Kind::Scalar),
    ("dd_config_dir", Kind::Scalar),
    ("dcgm_exporter_container", Kind::Scalar),
    ("dcgm_diag_image", Kind::Scalar),
    ("dcgm_prometheus_jobs", Kind::List),
    ("openshell_containers", Kind::List),
    ("guardrails_container", Kind::Scalar),
    ("switchyard_container", Kind::Scalar),
    ("switchyard_compose_project", Kind::Scalar),
    ("switchyard_compose_service", Kind::Scalar),
    ("switchyard_image", Kind::Scalar),
    ("switchyard_log_path", Kind::Scalar),
    ("relay_unit", Kind::Scalar),
    ("dynamo_compose_project", Kind::Scalar),
    ("dynamo_services", Kind::Dict),
    ("dynamo_containers", Kind::Dict),
    ("guardrails_compose_project", Kind::Scalar),
    ("dcgm_compose_project", Kind::Scalar),
    ("layout", Kind::Dict),
    ("capable_probe", Kind::Scalar),
    ("capable_probe_interval_s", Kind::Scalar),
    ("capable_probe_timeout_s", Kind::Scalar),
    ("capable_probe_retry_s", Kind::Scalar),
    ("capable_probe_key_env", Kind::Scalar),
];

/// Startup failure, rendered like pydantic's `ValidationError` / pydantic-settings' `SettingsError`.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{}", render_validation(.0))]
    Invalid(Vec<PyError>),
    #[error("error parsing value for field \"{field}\" from source \"{source_name}\"")]
    Parse {
        field: String,
        source_name: &'static str,
    },
}

fn render_validation(errors: &[PyError]) -> String {
    let n = errors.len();
    let mut out = format!(
        "{n} validation error{} for Settings",
        if n == 1 { "" } else { "s" }
    );
    for e in errors {
        let l: Vec<String> = e.loc.iter().map(ToString::to_string).collect();
        out.push_str(&format!("\n{}\n  {} [type={}]", l.join("."), e.msg, e.kind));
    }
    out
}

/// Parse a dotenv file like python-dotenv: `KEY=VALUE`, optional `export `, `#` comments, quotes.
#[must_use]
pub fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_owned();
        let v = v.trim_start();
        let value = if let Some(rest) = v.strip_prefix('"') {
            let mut s = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some(o) => s.push(o),
                        None => {}
                    },
                    o => s.push(o),
                }
            }
            s
        } else if let Some(rest) = v.strip_prefix('\'') {
            rest.split('\'').next().unwrap_or("").to_owned()
        } else {
            let cut = v.find(" #").or_else(|| v.find("\t#")).unwrap_or(v.len());
            v[..cut].trim_end().to_owned()
        };
        out.push((key, value));
    }
    out
}

fn deep_merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => deep_merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// Turn one source's `name -> raw string` pairs into field values (JSON for complex fields, nested keys exploded).
fn source_values(
    pairs: &[(String, String)],
    source_name: &'static str,
) -> Result<Map<String, Value>, SettingsError> {
    let mut out = Map::new();
    let prefix = ENV_PREFIX.to_ascii_lowercase();
    let mut nested: Vec<(String, Vec<String>, String)> = Vec::new();
    for (name, raw) in pairs {
        let lname = name.to_ascii_lowercase();
        let Some(rest) = lname.strip_prefix(&prefix) else {
            continue;
        };
        if let Some(&(field, kind)) = FIELDS.iter().find(|(f, _)| *f == rest) {
            let v = match kind {
                Kind::Scalar => Value::String(raw.clone()),
                Kind::List | Kind::Dict => {
                    serde_json::from_str::<Value>(raw).map_err(|_| SettingsError::Parse {
                        field: field.to_owned(),
                        source_name,
                    })?
                }
            };
            out.insert(field.to_owned(), v);
        } else if let Some((field, keys)) = rest.split_once("__")
            && let Some(&(f, Kind::Dict)) = FIELDS.iter().find(|(f, _)| *f == field)
        {
            nested.push((
                f.to_owned(),
                keys.split("__").map(str::to_owned).collect(),
                raw.clone(),
            ));
        }
    }
    for (field, keys, raw) in nested {
        let mut v = Value::String(raw);
        for k in keys.iter().rev() {
            let mut m = Map::new();
            m.insert(k.clone(), v);
            v = Value::Object(m);
        }
        let slot = out
            .entry(field)
            .or_insert_with(|| Value::Object(Map::new()));
        deep_merge(slot, v);
    }
    Ok(out)
}

impl Settings {
    /// `Settings(_env_file=user_env_file())`: the production constructor.
    pub fn load() -> Result<Settings, SettingsError> {
        Settings::from_sources(
            user_env_file().as_deref(),
            &std::env::vars().collect::<Vec<_>>(),
            Map::new(),
        )
    }

    /// `Settings(**init)` with the environment and env file ignored... except Python's init kwargs still sit on top
    /// of env and dotenv. Tests use [`Settings::with`] which applies init over the CURRENT process environment the
    /// same way Python does.
    pub fn with(init: Value) -> Result<Settings, SettingsError> {
        let init = match init {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        Settings::from_sources(None, &std::env::vars().collect::<Vec<_>>(), init)
    }

    /// Defaults < env file < environment < init values.
    pub fn from_sources(
        env_file: Option<&Path>,
        environ: &[(String, String)],
        init: Map<String, Value>,
    ) -> Result<Settings, SettingsError> {
        let mut merged = Map::new();
        if let Some(path) = env_file
            && let Ok(text) = std::fs::read_to_string(path)
        {
            for (k, v) in source_values(&parse_dotenv(&text), "DotEnvSettingsSource")? {
                deep_merge(merged.entry(k).or_insert(Value::Null), v);
            }
        }
        for (k, v) in source_values(environ, "EnvSettingsSource")? {
            let slot = merged.entry(k).or_insert(Value::Null);
            if slot.is_null() {
                *slot = v;
            } else {
                deep_merge(slot, v);
            }
        }
        for (k, v) in init {
            merged.insert(k, v);
        }
        Settings::validate(&merged)
    }

    #[allow(clippy::too_many_lines)]
    fn validate(m: &Map<String, Value>) -> Result<Settings, SettingsError> {
        let mut errs: Vec<PyError> = Vec::new();
        let home = home_dir();
        let mut v = Fields { m, errs: &mut errs };
        let host = v.string("host", "127.0.0.1");
        let port = v.int("port", 8793, None, None);
        let state_dir = v.path("state_dir", || {
            home.join(".local").join("share").join("nemo-console")
        });
        let repo_root = v.path("repo_root", default_repo_root);
        let frontend_dir = v.path("frontend_dir", default_frontend_dir);
        let allowed_hosts = v.str_list("allowed_hosts", &[]);
        let urls = v.str_map("urls", &DEFAULT_URLS, true);
        let allowed_url_hosts = v.str_list("allowed_url_hosts", &["127.0.0.1", "localhost", "::1"]);
        let playground_key_env = v.str_map("playground_key_env", &DEFAULT_KEY_ENV, true);
        let engine_probes = v.str_map("engine_probes", &DEFAULT_ENGINE_PROBES, true);
        let engine_start = v.str_map("engine_start", &DEFAULT_ENGINE_START, true);
        let topology_engines = v.topology_engines();
        let dynamo_opt_in = v.boolean("dynamo_opt_in", true);
        let dynamo_start = v.string("dynamo_start", DEFAULT_DYNAMO_START);
        let default_model_start = v.string("default_model_start", DEFAULT_MODEL_START);
        let dynamo_exclusive_engines =
            v.str_list("dynamo_exclusive_engines", &DEFAULT_DYNAMO_EXCLUSIVE);
        let dynamo_coexisting_engines =
            v.str_list("dynamo_coexisting_engines", &DEFAULT_DYNAMO_COEXISTING);
        let dynamo_ignored_containers =
            v.str_list("dynamo_ignored_containers", &DEFAULT_DYNAMO_IGNORED);
        let dynamo_dark_while_held = v.str_list("dynamo_dark_while_held", &DEFAULT_DYNAMO_DARK);
        let trust_loopback = v.boolean("trust_loopback", false);
        let background_tasks = v.boolean("background_tasks", true);
        let poll_interval_s = v.float("poll_interval_s", 2.0, None, None);
        let ring_seconds = v.int("ring_seconds", 900, None, None);
        let status_ttl_s = v.float("status_ttl_s", 2.0, None, None);
        let adapter_timeout_s = v.float("adapter_timeout_s", 8.0, None, None);
        let action_timeout_s = v.float("action_timeout_s", 900.0, None, None);
        let action_timeouts = v.float_map("action_timeouts");
        let body_cap_bytes = v.int("body_cap_bytes", 1024 * 1024, None, None);
        let config_cap_bytes = v.int("config_cap_bytes", 256 * 1024, None, None);
        let sse_max_connections = v.int("sse_max_connections", 8, None, None);
        let sse_idle_timeout_s = v.float("sse_idle_timeout_s", 300.0, None, None);
        let sse_heartbeat_s = v.float("sse_heartbeat_s", 15.0, None, None);
        let shutdown_grace_s = v.int("shutdown_grace_s", 5, Some(1), Some(60));
        let playground_max_tokens = v.int("playground_max_tokens", 4096, None, None);
        let revisions_keep = v.int("revisions_keep", 50, None, None);
        let otlp_endpoint = v.opt_string("otlp_endpoint");
        let extra_bin_dirs = v.str_list("extra_bin_dirs", &["~/.local/bin"]);
        let dd_scripts_dir = v.path("dd_scripts_dir", || {
            home.join("Datasets").join("data-designer").join("scripts")
        });
        let dd_datasets_root = v.path("dd_datasets_root", || home.join("Datasets"));
        let dd_config_dir = v.path("dd_config_dir", || home.join(".data-designer"));
        let dcgm_exporter_container = v.string("dcgm_exporter_container", DCGM_EXPORTER_CONTAINER);
        v.check("dcgm_exporter_container", &dcgm_exporter_container, |s| {
            container_name(s)
        });
        let dcgm_diag_image = v.string("dcgm_diag_image", DCGM_DIAG_IMAGE);
        v.check("dcgm_diag_image", &dcgm_diag_image, |s| image_ref(s));
        let dcgm_prometheus_jobs = v.str_list("dcgm_prometheus_jobs", &DCGM_PROMETHEUS_JOBS);
        let openshell_containers = v.str_list("openshell_containers", &OPENSHELL_CONTAINERS);
        if v.present_ok("openshell_containers") {
            v.check("openshell_containers", &(), |()| {
                openshell_containers
                    .iter()
                    .try_for_each(|n| container_name(n))
            });
        }
        let guardrails_container = v.string("guardrails_container", GUARDRAILS_CONTAINER);
        v.check("guardrails_container", &guardrails_container, |s| {
            container_name(s)
        });
        let switchyard_container = v.string("switchyard_container", SWITCHYARD_CONTAINER);
        v.check("switchyard_container", &switchyard_container, |s| {
            container_name(s)
        });
        let switchyard_compose_project =
            v.string("switchyard_compose_project", SWITCHYARD_COMPOSE_PROJECT);
        v.check(
            "switchyard_compose_project",
            &switchyard_compose_project,
            |s| compose_project(s),
        );
        let switchyard_compose_service =
            v.string("switchyard_compose_service", SWITCHYARD_COMPOSE_SERVICE);
        v.check(
            "switchyard_compose_service",
            &switchyard_compose_service,
            |s| {
                if COMPOSE_SERVICE_RE.is_match(s) {
                    Ok(())
                } else {
                    Err(format!("not a compose service name: {}", str_repr(s)))
                }
            },
        );
        let switchyard_image = v.string("switchyard_image", SWITCHYARD_IMAGE);
        v.check("switchyard_image", &switchyard_image, |s| image_ref(s));
        let switchyard_log_path = v.string("switchyard_log_path", SWITCHYARD_LOG_PATH);
        v.check("switchyard_log_path", &switchyard_log_path, |s| {
            if IN_CONTAINER_PATH_RE.is_match(s)
                && !s.split('/').any(|seg| seg == "." || seg == "..")
            {
                Ok(())
            } else {
                Err(format!(
                    "not an absolute in-container path: {}",
                    str_repr(s)
                ))
            }
        });
        let relay_unit = v.string("relay_unit", RELAY_UNIT);
        v.check("relay_unit", &relay_unit, |s| {
            if UNIT_RE.is_match(s) {
                Ok(())
            } else {
                Err(format!("not a systemd .service unit name: {}", str_repr(s)))
            }
        });
        let dynamo_compose_project = v.string("dynamo_compose_project", DYNAMO_COMPOSE_PROJECT);
        v.check("dynamo_compose_project", &dynamo_compose_project, |s| {
            compose_project(s)
        });
        let dynamo_services = v.str_map("dynamo_services", &DEFAULT_DYNAMO_SERVICES, true);
        v.check("dynamo_services", &dynamo_services, |m| {
            for (role, name) in m {
                if !DYNAMO_ROLES.contains(&role.as_str()) {
                    return Err(format!(
                        "unknown Dynamo role {} (one of {})",
                        str_repr(role),
                        DYNAMO_ROLES.join(", ")
                    ));
                }
                if !COMPOSE_SERVICE_RE.is_match(name) {
                    return Err(format!(
                        "dynamo_services {}: not a compose service name: {}",
                        str_repr(role),
                        str_repr(name)
                    ));
                }
            }
            Ok(())
        });
        let dynamo_containers = v.str_map("dynamo_containers", &DEFAULT_DYNAMO_CONTAINERS, true);
        v.check("dynamo_containers", &dynamo_containers, |m| {
            for (role, name) in m {
                if !DYNAMO_ROLES.contains(&role.as_str()) {
                    return Err(format!(
                        "unknown Dynamo role {} (one of {})",
                        str_repr(role),
                        DYNAMO_ROLES.join(", ")
                    ));
                }
                if !CONTAINER_NAME_RE.is_match(name) {
                    return Err(format!(
                        "dynamo_containers {}: not a container name: {}",
                        str_repr(role),
                        str_repr(name)
                    ));
                }
            }
            let distinct: std::collections::HashSet<&String> = m.values().collect();
            if distinct.len() != m.len() {
                return Err("dynamo_containers: each role needs its own container name".to_owned());
            }
            Ok(())
        });
        let guardrails_compose_project =
            v.string("guardrails_compose_project", GUARDRAILS_COMPOSE_PROJECT);
        v.check(
            "guardrails_compose_project",
            &guardrails_compose_project,
            |s| compose_project(s),
        );
        let dcgm_compose_project = v.string("dcgm_compose_project", DCGM_COMPOSE_PROJECT);
        v.check("dcgm_compose_project", &dcgm_compose_project, |s| {
            compose_project(s)
        });
        let layout = v.layout();
        let capable_probe = v.boolean("capable_probe", false);
        let capable_probe_interval_s = v.float(
            "capable_probe_interval_s",
            300.0,
            Some(300.0),
            Some(86400.0),
        );
        let capable_probe_timeout_s =
            v.float("capable_probe_timeout_s", 60.0, Some(2.0), Some(300.0));
        let capable_probe_retry_s = v.float("capable_probe_retry_s", 30.0, Some(0.0), Some(300.0));
        let capable_probe_key_env = v.string("capable_probe_key_env", "LITELLM_API_KEY");
        v.check("capable_probe_key_env", &capable_probe_key_env, |s| {
            if credential_env_allowed(s) {
                Ok(())
            } else {
                Err("capable_probe_key_env takes an ENV VAR NAME: LITELLM_*, NEMO_CONSOLE_PLAYGROUND_* or *_API_KEY"
                    .to_owned())
            }
        });
        if !errs.is_empty() {
            return Err(SettingsError::Invalid(errs));
        }
        Ok(Settings {
            host,
            port,
            state_dir,
            repo_root,
            frontend_dir,
            allowed_hosts,
            urls,
            allowed_url_hosts,
            playground_key_env,
            engine_probes,
            engine_start,
            topology_engines,
            dynamo_opt_in,
            dynamo_start,
            default_model_start,
            dynamo_exclusive_engines,
            dynamo_coexisting_engines,
            dynamo_ignored_containers,
            dynamo_dark_while_held,
            trust_loopback,
            background_tasks,
            poll_interval_s,
            ring_seconds,
            status_ttl_s,
            adapter_timeout_s,
            action_timeout_s,
            action_timeouts,
            body_cap_bytes,
            config_cap_bytes,
            sse_max_connections,
            sse_idle_timeout_s,
            sse_heartbeat_s,
            shutdown_grace_s,
            playground_max_tokens,
            revisions_keep,
            otlp_endpoint,
            extra_bin_dirs,
            dd_scripts_dir,
            dd_datasets_root,
            dd_config_dir,
            dcgm_exporter_container,
            dcgm_diag_image,
            dcgm_prometheus_jobs,
            openshell_containers,
            guardrails_container,
            switchyard_container,
            switchyard_compose_project,
            switchyard_compose_service,
            switchyard_image,
            switchyard_log_path,
            relay_unit,
            dynamo_compose_project,
            dynamo_services,
            dynamo_containers,
            guardrails_compose_project,
            dcgm_compose_project,
            layout,
            capable_probe,
            capable_probe_interval_s,
            capable_probe_timeout_s,
            capable_probe_retry_s,
            capable_probe_key_env,
        })
    }

    /// (trust, notice). Trust is forced OFF whenever a proxy/remote exposure is plausible.
    #[must_use]
    pub fn effective_loopback_trust(&self) -> (bool, Option<String>) {
        if !self.trust_loopback {
            return (false, None);
        }
        if !matches!(self.host.as_str(), "127.0.0.1" | "::1" | "localhost") {
            return (
                false,
                Some(format!(
                    "loopback trust disabled: host {} is not a loopback address",
                    str_repr(&self.host)
                )),
            );
        }
        if !self.allowed_hosts.is_empty() {
            return (
                false,
                Some("loopback trust disabled: allowed_hosts is set (a same-host reverse proxy would make callers admin)".to_owned()),
            );
        }
        (true, None)
    }

    /// The file that proves repo_root is the deployment checkout: the Dynamo compose file under the layout.
    #[must_use]
    pub fn repo_marker(&self) -> PathBuf {
        PathBuf::from(
            self.layout
                .get("dynamo")
                .map_or("inference/dynamo", String::as_str),
        )
        .join("docker-compose.yml")
    }

    #[must_use]
    pub fn repo_root_problem(&self) -> Option<String> {
        let marker = self.repo_marker();
        if self.repo_root.join(&marker).is_file() {
            return None;
        }
        Some(format!(
            "repo root {} does not contain {}; set NEMO_CONSOLE_REPO_ROOT to the lab repository checkout (see docs/OPERATIONS.md)",
            self.repo_root.display(),
            marker.display()
        ))
    }

    /// Named ABSOLUTE config-only paths for the adapters (`resolved_paths()`).
    #[must_use]
    pub fn resolved_paths(&self) -> IndexMap<String, PathBuf> {
        let r = &self.repo_root;
        let home = home_dir();
        let lay = |k: &str| r.join(self.layout.get(k).map_or("", String::as_str));
        let runner = lay("openshell_runner");
        let mut p = IndexMap::new();
        let mut put = |k: &str, v: PathBuf| {
            p.insert(k.to_owned(), v);
        };
        put("repo", r.clone());
        put("state", self.state_dir.clone());
        put("switchyard_dir", lay("switchyard"));
        put("switchyard_routes", lay("switchyard").join("routes.toml"));
        put(
            "switchyard_compose",
            lay("switchyard").join("docker-compose.yml"),
        );
        put("relay_routes", lay("relay").join("routes.toml"));
        put("relay_dir", lay("relay"));
        put(
            "relay_config",
            home.join(".config").join("nemo-relay").join("config.toml"),
        );
        put(
            "relay_plugins",
            home.join(".config").join("nemo-relay").join("plugins.toml"),
        );
        put(
            "relay_atof_dir",
            home.join(".local")
                .join("share")
                .join("nemo-relay")
                .join("atof"),
        );
        put(
            "relay_atif_dir",
            home.join(".local")
                .join("share")
                .join("nemo-relay")
                .join("atif"),
        );
        put("dynamo_dir", lay("dynamo"));
        put("dynamo_compose", lay("dynamo").join("docker-compose.yml"));
        put("skills_dir", lay("skills"));
        put("skillevaluator_dir", lay("skillevaluator"));
        put("ollama_dir", lay("ollama"));
        put("data_designer_dir", lay("data_designer"));
        put("scripts_dir", self.dd_scripts_dir.clone());
        put("datasets_root", self.dd_datasets_root.clone());
        put("dd_config_dir", self.dd_config_dir.clone());
        put(
            "dd_key_file",
            home.join(".config")
                .join("data-designer")
                .join("litellm.key"),
        );
        put("openshell_runner_dir", runner.clone());
        put(
            "openshell_gateway_config",
            runner.join("config").join("gateway.toml"),
        );
        put("openshell_policy", runner.join("policy.yaml"));
        put("openshell_compose", runner.join("docker-compose.yml"));
        put("observability_dir", lay("observability"));
        put(
            "observability_compose",
            lay("observability").join("docker-compose.yml"),
        );
        put(
            "dcgm_counters",
            lay("observability").join("dcgm").join("counters.csv"),
        );
        put("aiperf_dir", lay("aiperf"));
        put("aiperf_profiles", lay("aiperf").join("profiles.yaml"));
        put(
            "aiperf_key_file",
            home.join(".config").join("aiperf").join("litellm.key"),
        );
        put("guardrails_dir", lay("guardrails"));
        put(
            "guardrails_compose",
            lay("guardrails").join("docker-compose.yml"),
        );
        put("guardrails_config_dir", lay("guardrails").join("config"));
        put("evaluator_dir", lay("evaluator"));
        put("evaluator_suites", lay("evaluator").join("suites.yaml"));
        put(
            "evaluator_key_file",
            home.join(".config")
                .join("nemo-evaluator")
                .join("litellm.key"),
        );
        p
    }
}

fn container_name(s: &str) -> Result<(), String> {
    if CONTAINER_NAME_RE.is_match(s) {
        Ok(())
    } else {
        Err(format!("not a container name: {}", str_repr(s)))
    }
}

fn compose_project(s: &str) -> Result<(), String> {
    if COMPOSE_PROJECT_RE.is_match(s) {
        Ok(())
    } else {
        Err(format!("not a compose project name: {}", str_repr(s)))
    }
}

fn image_ref(s: &str) -> Result<(), String> {
    if IMAGE_REF_RE.is_match(s) {
        Ok(())
    } else {
        Err(format!("not an image reference: {}", str_repr(s)))
    }
}

/// Field-by-field validation with pydantic's lax rules; errors accumulate in definition order.
struct Fields<'a> {
    m: &'a Map<String, Value>,
    errs: &'a mut Vec<PyError>,
}

impl Fields<'_> {
    fn get(&self, k: &str) -> Option<&Value> {
        self.m.get(k)
    }

    fn fail(&mut self, field: &str, kind: &'static str, msg: impl Into<String>) {
        self.errs
            .push(PyError::new(vec![Loc::Key(field.to_owned())], kind, msg));
    }

    /// True when the field validated so far without an error (field validators run only then).
    fn present_ok(&self, field: &str) -> bool {
        !self
            .errs
            .iter()
            .any(|e| e.loc.first() == Some(&Loc::Key(field.to_owned())))
    }

    fn check<T: ?Sized>(
        &mut self,
        field: &str,
        value: &T,
        f: impl FnOnce(&T) -> Result<(), String>,
    ) {
        if !self.present_ok(field) {
            return;
        }
        if let Err(msg) = f(value) {
            self.fail(field, "value_error", format!("Value error, {msg}"));
        }
    }

    fn string(&mut self, k: &str, default: &str) -> String {
        match self.get(k) {
            None => default.to_owned(),
            Some(v) => strict_str(v).unwrap_or_else(|(kind, msg)| {
                self.fail(k, kind, msg);
                default.to_owned()
            }),
        }
    }

    fn opt_string(&mut self, k: &str) -> Option<String> {
        match self.get(k) {
            None | Some(Value::Null) => None,
            Some(v) => match strict_str(v) {
                Ok(s) => Some(s),
                Err((kind, msg)) => {
                    self.fail(k, kind, msg);
                    None
                }
            },
        }
    }

    fn path(&mut self, k: &str, default: impl FnOnce() -> PathBuf) -> PathBuf {
        match self.get(k) {
            None => default(),
            Some(Value::String(s)) => PathBuf::from(s),
            Some(_) => {
                self.fail(k, "path_type", "Input is not a valid path");
                default()
            }
        }
    }

    fn boolean(&mut self, k: &str, default: bool) -> bool {
        match self.get(k) {
            None => default,
            Some(v) => lax_bool(v).unwrap_or_else(|(kind, msg)| {
                self.fail(k, kind, msg);
                default
            }),
        }
    }

    fn int(&mut self, k: &str, default: i64, ge: Option<i64>, le: Option<i64>) -> i64 {
        let Some(v) = self.get(k) else { return default };
        match lax_int(v) {
            Err((kind, msg)) => {
                self.fail(k, kind, msg);
                default
            }
            Ok(i) => {
                if let Some(g) = ge
                    && i < g
                {
                    self.fail(k, "greater_than_equal", msg_ge(&g.to_string()));
                } else if let Some(l) = le
                    && i > l
                {
                    self.fail(k, "less_than_equal", msg_le(&l.to_string()));
                }
                i
            }
        }
    }

    /// pydantic-core (Rust) prints a float bound with Rust's `Display`: `ge=300.0` reads "300".
    fn float(&mut self, k: &str, default: f64, ge: Option<f64>, le: Option<f64>) -> f64 {
        let Some(v) = self.get(k) else { return default };
        match lax_float(v) {
            Err((kind, msg)) => {
                self.fail(k, kind, msg);
                default
            }
            Ok(f) => {
                if let Some(g) = ge
                    && f < g
                {
                    self.fail(k, "greater_than_equal", msg_ge(&format!("{g}")));
                } else if let Some(l) = le
                    && f > l
                {
                    self.fail(k, "less_than_equal", msg_le(&format!("{l}")));
                }
                f
            }
        }
    }

    fn str_list(&mut self, k: &str, default: &[&str]) -> Vec<String> {
        let Some(v) = self.get(k).cloned() else {
            return vec_of(default);
        };
        let Value::Array(items) = v else {
            self.fail(k, "list_type", "Input should be a valid list");
            return vec_of(default);
        };
        let mut out = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match strict_str(item) {
                Ok(s) => out.push(s),
                Err((kind, msg)) => self.errs.push(PyError::new(
                    vec![Loc::Key(k.to_owned()), Loc::Idx(i)],
                    kind,
                    msg,
                )),
            }
        }
        out
    }

    /// dict[str, str]; `merge_defaults`: the Python validator merges `{**DEFAULTS, **v}`.
    fn str_map(
        &mut self,
        k: &str,
        defaults: &[(&str, &str)],
        merge_defaults: bool,
    ) -> IndexMap<String, String> {
        let base = map_of(defaults);
        let Some(v) = self.get(k).cloned() else {
            return base;
        };
        let Value::Object(obj) = v else {
            self.fail(k, "dict_type", "Input should be a valid dictionary");
            return base;
        };
        let mut out = if merge_defaults {
            base
        } else {
            IndexMap::new()
        };
        for (key, val) in obj {
            match strict_str(&val) {
                Ok(s) => {
                    out.insert(key, s);
                }
                Err((kind, msg)) => self.errs.push(PyError::new(
                    vec![Loc::Key(k.to_owned()), Loc::Key(key)],
                    kind,
                    msg,
                )),
            }
        }
        out
    }

    fn float_map(&mut self, k: &str) -> IndexMap<String, f64> {
        let Some(v) = self.get(k).cloned() else {
            return IndexMap::new();
        };
        let Value::Object(obj) = v else {
            self.fail(k, "dict_type", "Input should be a valid dictionary");
            return IndexMap::new();
        };
        let mut out = IndexMap::new();
        for (key, val) in obj {
            match lax_float(&val) {
                Ok(f) => {
                    out.insert(key, f);
                }
                Err((kind, msg)) => self.errs.push(PyError::new(
                    vec![Loc::Key(k.to_owned()), Loc::Key(key)],
                    kind,
                    msg,
                )),
            }
        }
        out
    }

    fn layout(&mut self) -> IndexMap<String, String> {
        let mut out = map_of(&DEFAULT_LAYOUT);
        let Some(v) = self.get("layout").cloned() else {
            return out;
        };
        let Value::Object(obj) = v else {
            self.fail("layout", "dict_type", "Input should be a valid dictionary");
            return out;
        };
        let mut typed = Vec::new();
        for (key, val) in obj {
            match strict_str(&val) {
                Ok(s) => typed.push((key, s)),
                Err((kind, msg)) => {
                    self.errs.push(PyError::new(
                        vec![Loc::Key("layout".to_owned()), Loc::Key(key)],
                        kind,
                        msg,
                    ));
                }
            }
        }
        if !self.present_ok("layout") {
            return out;
        }
        for (key, rel) in typed {
            if !out.contains_key(&key) {
                let keys: Vec<&str> = DEFAULT_LAYOUT.iter().map(|(k, _)| *k).collect();
                self.fail(
                    "layout",
                    "value_error",
                    format!(
                        "Value error, unknown layout key {} (one of {})",
                        str_repr(&key),
                        keys.join(", ")
                    ),
                );
                return out;
            }
            match layout_rel(&key, &rel) {
                Ok(r) => {
                    out.insert(key, r);
                }
                Err(msg) => {
                    self.fail("layout", "value_error", format!("Value error, {msg}"));
                    return out;
                }
            }
        }
        out
    }

    fn topology_engines(&mut self) -> Vec<TopologyEngine> {
        let Some(v) = self.get("topology_engines").cloned() else {
            return default_topology_engines();
        };
        let Value::Array(items) = v else {
            self.fail(
                "topology_engines",
                "list_type",
                "Input should be a valid list",
            );
            return default_topology_engines();
        };
        static ID: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z0-9][a-z0-9_.\-]{0,63}"));
        static CTR: LazyLock<Regex> = LazyLock::new(|| full(r"[a-zA-Z0-9][a-zA-Z0-9_.\-]{0,127}"));
        let mut out = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let at = |f: &str| {
                vec![
                    Loc::Key("topology_engines".to_owned()),
                    Loc::Idx(i),
                    Loc::Key(f.to_owned()),
                ]
            };
            let Value::Object(o) = item else {
                self.errs.push(PyError::new(
                    vec![Loc::Key("topology_engines".to_owned()), Loc::Idx(i)],
                    "model_type",
                    "Input should be a valid dictionary or instance of TopologyEngine",
                ));
                continue;
            };
            let before = self.errs.len();
            let mut text =
                |f: &str, rx: Option<(&Regex, &str)>, len: Option<(usize, usize)>| -> String {
                    match o.get(f) {
                        None => {
                            self.errs
                                .push(PyError::new(at(f), "missing", "Field required"));
                            String::new()
                        }
                        Some(v) => match strict_str(v) {
                            Err((kind, msg)) => {
                                self.errs.push(PyError::new(at(f), kind, msg));
                                String::new()
                            }
                            Ok(s) => {
                                if let Some((rx, pat)) = rx
                                    && !rx.is_match(&s)
                                {
                                    self.errs.push(PyError::new(
                                        at(f),
                                        "string_pattern_mismatch",
                                        msg_pattern(pat),
                                    ));
                                }
                                if let Some((lo, hi)) = len {
                                    let n = char_len(&s);
                                    if n < lo {
                                        self.errs.push(PyError::new(
                                            at(f),
                                            "string_too_short",
                                            format!(
                                                "String should have at least {lo} character{}",
                                                if lo == 1 { "" } else { "s" }
                                            ),
                                        ));
                                    } else if n > hi {
                                        self.errs.push(PyError::new(
                                            at(f),
                                            "string_too_long",
                                            format!("String should have at most {hi} characters"),
                                        ));
                                    }
                                }
                                s
                            }
                        },
                    }
                };
            let id = text("id", Some((&ID, r"^[a-z0-9][a-z0-9_.\-]{0,63}$")), None);
            let label = text("label", None, Some((1, 80)));
            let container = text(
                "container",
                Some((&CTR, r"^[a-zA-Z0-9][a-zA-Z0-9_.\-]{0,127}$")),
                None,
            );
            let opt_in = match o.get("opt_in") {
                None => true,
                Some(v) => lax_bool(v).unwrap_or_else(|(kind, msg)| {
                    self.errs.push(PyError::new(at("opt_in"), kind, msg));
                    true
                }),
            };
            if self.errs.len() == before {
                out.push(TopologyEngine {
                    id,
                    label,
                    container,
                    opt_in,
                });
            }
        }
        out
    }
}

/// Test helper: `Settings(...)` from a JSON object of init values, isolated from the process environment.
pub fn settings_for_test(init: Value) -> Settings {
    let init = match init {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    match Settings::from_sources(None, &[], init) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("test settings invalid: {e}");
            #[allow(clippy::panic)]
            {
                panic!("invalid test settings: {e}")
            }
        }
    }
}

/// `json!` re-export for callers building init values.
#[must_use]
pub fn init(v: Value) -> Value {
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn env_parsing_like_pydantic_settings() {
        let s = Settings::from_sources(
            None,
            &env(&[
                ("NEMO_CONSOLE_TRUST_LOOPBACK", "YES"),
                ("NEMO_CONSOLE_PORT", "8_000"),
                ("NEMO_CONSOLE_URLS", r#"{"x":"http://h"}"#),
                ("NEMO_CONSOLE_URLS__RELAY", "http://r"),
                ("nemo_console_dynamo_services__worker", "w"),
                ("OTHER", "1"),
            ]),
            Map::new(),
        )
        .unwrap();
        assert!(s.trust_loopback);
        assert_eq!(s.port, 8000);
        assert_eq!(s.urls["x"], "http://h");
        assert_eq!(s.urls["relay"], "http://r");
        assert_eq!(s.urls["dcgm"], "http://127.0.0.1:9400");
        assert_eq!(s.dynamo_services["worker"], "w");
    }

    #[test]
    fn env_errors_like_pydantic_settings() {
        let e = Settings::from_sources(
            None,
            &env(&[("NEMO_CONSOLE_ALLOWED_HOSTS", "a,b")]),
            Map::new(),
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "error parsing value for field \"allowed_hosts\" from source \"EnvSettingsSource\""
        );
        let e = Settings::from_sources(
            None,
            &env(&[("NEMO_CONSOLE_SHUTDOWN_GRACE_S", "0")]),
            Map::new(),
        )
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("shutdown_grace_s\n  Input should be greater than or equal to 1"),
            "{e}"
        );
        let e = Settings::from_sources(
            None,
            &env(&[("NEMO_CONSOLE_LAYOUT__NOPE", "x")]),
            Map::new(),
        )
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("Value error, unknown layout key 'nope'"),
            "{e}"
        );
    }

    #[test]
    fn dotenv_parsing() {
        let p = parse_dotenv(
            "# c\nexport NEMO_CONSOLE_PORT=9001\nNEMO_CONSOLE_POLL_INTERVAL_S=\"3.5\"\nX=a # trailing\n",
        );
        assert_eq!(
            p,
            vec![
                ("NEMO_CONSOLE_PORT".to_owned(), "9001".to_owned()),
                ("NEMO_CONSOLE_POLL_INTERVAL_S".to_owned(), "3.5".to_owned()),
                ("X".to_owned(), "a".to_owned()),
            ]
        );
    }

    #[test]
    fn init_values_json() {
        let s = settings_for_test(json!({"trust_loopback": true, "sse_max_connections": 2}));
        assert!(s.trust_loopback);
        assert_eq!(s.sse_max_connections, 2);
    }
}
