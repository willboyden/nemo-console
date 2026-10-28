//! NVIDIA Dynamo adapter (frontend + vLLM worker, image nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.5.0). OWNER: workstream A
//! (PORTING.md section 9). Port of `backend/src/nemo_console/adapters/dynamo.py`; its module docstring lists the
//! provenance of every fact used here (flags and choices from `--help` of the pinned image, metric names from live
//! scrapes, the worker read through `docker exec` because rootless Docker hides its port, argv from `docker inspect`).
//!
//! The compose files IN USE come from the running containers' `com.docker.compose.project.config_files` label
//! (untrusted input: every file must resolve inside the configured compose directory); Params and the console override
//! are refused while a repo override file is in use. The hand-editable `override` is a CLOSED allowlist (only
//! worker/frontend `command`/`environment`, allowlisted flags, no new `$`, `--model` pinned), then `docker compose
//! config -q` as the authoritative parse. Lifecycle is `docker compose` on the files in use; the model-switch script
//! is never run, only SHOWN (CardPolicy).

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Map, Number, Value};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, RunOpts, SystemAdapter,
};
use crate::adapters::switchyard::re;
use crate::api::pyjson::PyJson;
use crate::config::{
    DEFAULT_DYNAMO_COEXISTING, DEFAULT_DYNAMO_DARK, DEFAULT_DYNAMO_EXCLUSIVE,
    DEFAULT_DYNAMO_IGNORED, DEFAULT_DYNAMO_START, DEFAULT_MODEL_START, Settings,
};
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType,
    PlaygroundSpec, PostCheck, Problem, Severity, SystemEvent, SystemId, SystemStatus,
    ValidationResult, now_ts,
};
use crate::services::common::sha256_hex;
use crate::services::config_store::{atomic_write, mtime_f64};
use crate::services::pyfmt::{
    char_len, char_prefix, char_suffix_from, float_repr, json_dumps, list_repr, str_repr,
};
use crate::services::pyutil::{
    fromisoformat, json_loads_bytes, pj_str, pj_truthy, py_float, py_split_ws, py_splitlines,
    py_strip,
};
use crate::services::settings::resolve_lenient;

use crate::services::pyyaml::{PyValue, safe_load_value as safe_load};
use crate::services::shlex;

/// `DynamoAdapter.label` in Python.
pub const LABEL: &str = "NVIDIA Dynamo";

// ── fixed identities (container names come from the compose project name `ai-lab-dynamo`) ──────
pub const PROJECT: &str = "ai-lab-dynamo";
pub const FRONTEND: &str = "ai-lab-dynamo-dynamo-frontend-1";
pub const WORKER: &str = "ai-lab-dynamo-dynamo-worker-1";
pub const MOCKER: &str = "ai-lab-dynamo-dynamo-mocker-1";
/// docker-compose.super-vl.yml: the real frontend behind a gateway.
pub const SV_FRONTEND: &str = "ai-lab-dynamo-dynamo-sv-frontend-1";
pub const SVC_FRONTEND: &str = "dynamo-frontend";
pub const SVC_WORKER: &str = "dynamo-worker";
pub const SVC_MOCKER: &str = "dynamo-mocker";
pub const SVC_SV_FRONTEND: &str = "dynamo-sv-frontend";
pub const DYNAMO_NAMES: [&str; 4] = [FRONTEND, WORKER, MOCKER, SV_FRONTEND];
pub const LABEL_FILES: &str = "com.docker.compose.project.config_files";
pub const LABEL_WORKDIR: &str = "com.docker.compose.project.working_dir";
pub const MAX_LABEL_FILES: usize = 8;
/// The merged-config GPU probe is a subprocess: cache it.
pub const UUID_TTL: f64 = 300.0;

/// The ONE container `up` may stop (stop_sharing_engine=true), verified by name + compose labels.
pub const STOPPABLE_ENGINE: &str = "ai-lab-vllm-qwen38-vllm-1";
pub const STOPPABLE_PROJECT: &str = "ai-lab-vllm-qwen38";
pub const STOPPABLE_SERVICE: &str = "vllm";

pub const DEFAULT_URL: &str = "http://127.0.0.1:8020";
/// The worker's --served-model-name in the compose.
pub const DEFAULT_MODEL: &str = "unsloth/Qwen3.8-27B-NVFP4";
/// DYN_SYSTEM_PORT in the compose.
pub const WORKER_METRICS_PORT: u16 = 8081;

const MAX_CONFIG_BYTES: usize = 256 * 1024;
const STRUCTURAL: &str = "--dyn-enable-structural-tag";
const COMPOSE_NAMES: [&str; 4] = [
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

/// Choices copied from `--help` of the pinned image (dynamo.vllm / dynamo.frontend).
pub const TOOL_PARSERS: [&str; 24] = [
    "qwen3_coder",
    "qwen25",
    "hermes",
    "llama3_json",
    "mistral",
    "pythonic",
    "phi4",
    "jamba",
    "harmony",
    "gemma4",
    "glm47",
    "kimi_k2",
    "kimi_k3",
    "nemotron_deci",
    "nemotron_nano",
    "deepseek_v3",
    "deepseek_v3_1",
    "deepseek_v3_2",
    "deepseek_v4",
    "minimax_m2",
    "minimax_m3",
    "inkling",
    "muse",
    "default",
];
pub const REASONING_PARSERS: [&str; 25] = [
    "none",
    "qwen3",
    "deepseek_r1",
    "gpt_oss",
    "glm45",
    "granite",
    "mistral",
    "step3",
    "nemotron_v3",
    "nemotron_nano",
    "nemotron3",
    "nemotron_deci",
    "minimax_m2",
    "minimax_m3",
    "kimi",
    "kimi_k25",
    "kimi_k3",
    "gemma4",
    "deepseek_v3",
    "deepseek_v3_1",
    "deepseek_v3_2",
    "deepseek_v4",
    "inkling",
    "basic",
    "muse",
];
pub const ROUTER_MODES: [&str; 6] = [
    "round-robin",
    "random",
    "power-of-two",
    "kv",
    "least-loaded",
    "device-aware-weighted",
];
const FIXED_FLAGS_WORKER: [&str; 4] = [
    "--model",
    "--served-model-name",
    "--discovery-backend",
    "--request-plane",
];
const FIXED_FLAGS_FRONTEND: [&str; 4] = [
    "--http-host",
    "--http-port",
    "--discovery-backend",
    "--request-plane",
];

static NAME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}$"));
static UUID_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^GPU-[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$"));
static COMPOSE_FILE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^(docker-)?compose(\.[A-Za-z0-9_.-]+)?\.ya?ml$"));

/// The compose project, services and containers the adapter acts on (addendum 9.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamoNames {
    pub project: String,
    pub frontend: String,
    pub worker: String,
    pub mocker: String,
    pub sv_frontend: String,
    pub svc_frontend: String,
    pub svc_worker: String,
    pub svc_mocker: String,
    pub svc_sv_frontend: String,
}

impl Default for DynamoNames {
    fn default() -> Self {
        DynamoNames {
            project: PROJECT.into(),
            frontend: FRONTEND.into(),
            worker: WORKER.into(),
            mocker: MOCKER.into(),
            sv_frontend: SV_FRONTEND.into(),
            svc_frontend: SVC_FRONTEND.into(),
            svc_worker: SVC_WORKER.into(),
            svc_mocker: SVC_MOCKER.into(),
            svc_sv_frontend: SVC_SV_FRONTEND.into(),
        }
    }
}

impl DynamoNames {
    /// (frontend, worker, mocker, sv_frontend).
    #[must_use]
    pub fn all(&self) -> Vec<String> {
        vec![
            self.frontend.clone(),
            self.worker.clone(),
            self.mocker.clone(),
            self.sv_frontend.clone(),
        ]
    }

    /// (container, component) read for events.
    #[must_use]
    pub fn log_sources(&self) -> Vec<(String, &'static str)> {
        vec![
            (self.frontend.clone(), "frontend"),
            (self.sv_frontend.clone(), "frontend"),
            (self.worker.clone(), "worker"),
            (self.mocker.clone(), "mocker"),
        ]
    }

    /// From the `dynamo_*` settings (container._configure_dynamo).
    #[must_use]
    pub fn from_settings(cfg: &Settings) -> Self {
        let d = DynamoNames::default();
        let c = |k: &str, dflt: &str| {
            cfg.dynamo_containers
                .get(k)
                .cloned()
                .unwrap_or_else(|| dflt.to_owned())
        };
        let s = |k: &str, dflt: &str| {
            cfg.dynamo_services
                .get(k)
                .cloned()
                .unwrap_or_else(|| dflt.to_owned())
        };
        DynamoNames {
            project: cfg.dynamo_compose_project.clone(),
            frontend: c("frontend", &d.frontend),
            worker: c("worker", &d.worker),
            mocker: c("mocker", &d.mocker),
            sv_frontend: c("sv_frontend", &d.sv_frontend),
            svc_frontend: s("frontend", &d.svc_frontend),
            svc_worker: s("worker", &d.svc_worker),
            svc_mocker: s("mocker", &d.svc_mocker),
            svc_sv_frontend: s("sv_frontend", &d.svc_sv_frontend),
        }
    }
}

/// The repo compose files the live stack runs from (never the console's own override).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeFiles {
    pub files: Vec<PathBuf>,
    pub base: Option<PathBuf>,
    /// Read from the containers' compose label (false: no Dynamo container, the configured base applies).
    pub from_label: bool,
    /// The label could not be trusted (or the containers disagree): lifecycle actions and Params refuse.
    pub error: Option<String>,
}

impl ComposeFiles {
    #[must_use]
    pub fn new(files: Vec<PathBuf>, base: Option<PathBuf>) -> Self {
        ComposeFiles {
            files,
            base,
            from_label: false,
            error: None,
        }
    }

    /// Repo files beyond the configured base (e.g. docker-compose.super-vl.yml).
    #[must_use]
    pub fn overrides(&self) -> Vec<PathBuf> {
        let base = self.base.as_ref().map(|b| resolve_lenient(b));
        self.files
            .iter()
            .filter(|f| Some(*f) != base.as_ref())
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn names(&self) -> String {
        let n: Vec<String> = self.files.iter().map(|f| file_name(f)).collect();
        if n.is_empty() {
            "none".into()
        } else {
            n.join(", ")
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Who may share the worker's GPU, and the text the problems name (settings-driven; `start` / `restore` are DISPLAY
/// ONLY).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardPolicy {
    pub opt_in: bool,
    pub start: String,
    pub restore: String,
    pub exclusive: Vec<String>,
    pub coexisting: Vec<String>,
    pub ignored: Vec<String>,
    pub dark: Vec<String>,
    /// Dynamo's own containers: never an 'exclusive engine'.
    pub own: Vec<String>,
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

impl Default for CardPolicy {
    fn default() -> Self {
        CardPolicy {
            opt_in: true,
            start: DEFAULT_DYNAMO_START.into(),
            restore: DEFAULT_MODEL_START.into(),
            exclusive: owned(&DEFAULT_DYNAMO_EXCLUSIVE),
            coexisting: owned(&DEFAULT_DYNAMO_COEXISTING),
            ignored: owned(&DEFAULT_DYNAMO_IGNORED),
            dark: owned(&DEFAULT_DYNAMO_DARK),
            own: owned(&DYNAMO_NAMES),
        }
    }
}

impl CardPolicy {
    /// From the `dynamo_*` settings (container._configure_dynamo); `own` = the configured containers.
    #[must_use]
    pub fn from_settings(cfg: &Settings, names: &DynamoNames) -> Self {
        CardPolicy {
            opt_in: cfg.dynamo_opt_in,
            start: cfg.dynamo_start.clone(),
            restore: cfg.default_model_start.clone(),
            exclusive: cfg.dynamo_exclusive_engines.clone(),
            coexisting: cfg.dynamo_coexisting_engines.clone(),
            ignored: cfg.dynamo_ignored_containers.clone(),
            dark: cfg.dynamo_dark_while_held.clone(),
            own: names.all(),
        }
    }

    /// exclusive | coexisting | ignored | unknown. Exclusive wins a name listed twice (the safe reading).
    #[must_use]
    pub fn kind(&self, name: &str) -> &'static str {
        if self.exclusive.iter().any(|n| n == name) {
            "exclusive"
        } else if self.coexisting.iter().any(|n| n == name) {
            "coexisting"
        } else if self.ignored.iter().any(|n| n == name) {
            "ignored"
        } else {
            "unknown"
        }
    }

    #[must_use]
    pub fn exclusive_names(&self) -> Vec<String> {
        self.exclusive
            .iter()
            .filter(|n| NAME_RE.is_match(n) && !self.own.contains(n))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn dark_text(&self) -> String {
        if self.dark.is_empty() {
            "nothing configured (setting dynamo_dark_while_held)".into()
        } else {
            self.dark.join(", ")
        }
    }
}

// ── redaction / log parsing ─────────────────────────────────────────────────────────────────────

static ANSI: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\x1b\[[0-9;?]*[ -/]*[@-~]"));
static LEAD_TS: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^\d{4}-\d{2}-\d{2}T[\d:.]+Z?\s+"));
static DOCKER_TS: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?s)^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(\.\d+)?Z\s?(.*)$"));
static LEVEL: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"\b(TRACE|DEBUG|INFO|WARNING|WARN|ERROR|CRITICAL|FATAL)\b"));
static SECRET_BEARER: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{6,}"));
static SECRET_SK: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\bsk-[A-Za-z0-9_-]{8,}"));
static SECRET_HF: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\bhf_[A-Za-z0-9]{10,}"));
static SECRET_KV: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(r"(?i)\b(api[_-]?key|token|secret|password|authorization)\b(\s*[=:]\s*)\S+")
});
#[allow(clippy::expect_used)]
static ABS_PATH: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?<![\w:/.~-])(?:/[\w.@+=,-]+){2,}/?").expect("static regex")
});
static NOISE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"http response sent.*status\W*200"));
static OVERRIDE_SECRET: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)(\bsk-[A-Za-z0-9_-]{8,}|bearer\s+\S{6,}|\bhf_[A-Za-z0-9]{10,})"));

/// Strip ANSI, mask secrets and collapse absolute paths to `<path>/leaf` (no host layout leaks).
#[must_use]
pub fn redact(text: &str) -> String {
    let out = ANSI.replace_all(text, "").into_owned();
    let out = SECRET_BEARER
        .replace_all(&out, "Bearer [redacted]")
        .into_owned();
    let out = SECRET_SK.replace_all(&out, "[redacted]").into_owned();
    let out = SECRET_HF.replace_all(&out, "[redacted]").into_owned();
    let out = SECRET_KV.replace_all(&out, "$1$2[redacted]").into_owned();
    let mut res = String::with_capacity(out.len());
    let mut last = 0;
    for m in ABS_PATH.find_iter(&out) {
        let Ok(m) = m else { break };
        res.push_str(&out[last..m.start()]);
        let s = m.as_str().trim_end_matches('/');
        let leaf = s.rsplit('/').next().unwrap_or("");
        res.push_str("<path>/");
        res.push_str(leaf);
        last = m.end();
    }
    res.push_str(&out[last..]);
    res
}

/// One `docker logs --timestamps` line -> event (None for blanks and the health-poll noise).
#[must_use]
pub fn parse_log_line(raw: &str, component: &str) -> Option<SystemEvent> {
    let line = raw.trim_end_matches('\n');
    let m = DOCKER_TS.captures(line)?;
    let base = &m[1];
    let frac = m.get(2).map_or("", |g| g.as_str());
    let rest = m.get(3).map_or("", |g| g.as_str());
    let frac7: String = frac.chars().take(7).collect();
    let ts = fromisoformat(&format!("{base}{frac7}"))?;
    let ts = crate::services::pyutil::PyDateTime {
        offset_us: Some(0),
        ..ts
    }
    .timestamp();
    let plain = ANSI.replace_all(rest, "").into_owned();
    let plain = py_strip(&plain).to_owned();
    if plain.is_empty() || NOISE.is_match(&plain) {
        return None;
    }
    let plain = LEAD_TS.replace(&plain, "").into_owned();
    let level = LEVEL.captures(&plain).map_or("INFO", |c| match &c[1] {
        "WARNING" | "WARN" => "WARN",
        "TRACE" => "TRACE",
        "DEBUG" => "DEBUG",
        "INFO" => "INFO",
        _ => "ERROR",
    });
    let mut summary = redact(&plain);
    if char_len(&summary) > 400 {
        summary = format!("{}...", char_prefix(&summary, 397));
    }
    let mut data = Map::new();
    data.insert("component".into(), Value::String(component.to_owned()));
    data.insert("level".into(), Value::String(level.to_owned()));
    Some(SystemEvent {
        ts,
        system: SystemId::Dynamo,
        kind: "dynamo.log".into(),
        summary,
        data,
    })
}

// ── Prometheus text parsing (no client lib: the exposition is plain text) ───────────────────────

static SAMPLE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{(.*)\})?\s+(\S+)(?:\s+\S+)?$"));
static PLABEL: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r#"([A-Za-z_][A-Za-z0-9_]*)="((?:[^"\\]|\\.)*)""#));

/// (name, labels, value)
pub type Sample = (String, IndexMap<String, String>, f64);

#[must_use]
pub fn parse_prom(text: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in py_splitlines(text) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(m) = SAMPLE.captures(py_strip(line)) else {
            continue;
        };
        let Some(val) = m.get(3).and_then(|v| py_float(v.as_str())) else {
            continue;
        };
        let mut labels = IndexMap::new();
        for c in PLABEL.captures_iter(m.get(2).map_or("", |g| g.as_str())) {
            labels.insert(c[1].to_owned(), c[2].to_owned());
        }
        out.push((m[1].to_owned(), labels, val));
    }
    out
}

fn sum(samples: &[Sample], name: &str) -> Option<f64> {
    let vals: Vec<f64> = samples
        .iter()
        .filter(|(n, _, _)| n == name)
        .map(|s| s.2)
        .collect();
    if vals.is_empty() {
        None
    } else {
        Some(vals.iter().fold(0.0, |a, b| a + b))
    }
}

/// Lifetime quantile from cumulative `_bucket` samples, summed across label sets, linear interpolation.
#[must_use]
pub fn histogram_quantile(samples: &[Sample], base: &str, q: f64) -> Option<f64> {
    let bucket = format!("{base}_bucket");
    let mut cum: Vec<(f64, f64)> = Vec::new();
    for (n, lb, v) in samples {
        if *n == bucket
            && let Some(le) = lb.get("le")
            && let Some(le) = py_float(le)
        {
            #[allow(clippy::float_cmp)]
            match cum.iter_mut().find(|(b, _)| *b == le) {
                Some(slot) => slot.1 += v,
                None => cum.push((le, 0.0 + v)),
            }
        }
    }
    if cum.is_empty() {
        return None;
    }
    cum.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total = cum.last().map_or(0.0, |x| x.1);
    if total <= 0.0 {
        return None;
    }
    let rank = q * total;
    let (mut prev_b, mut prev_c) = (0.0f64, 0.0f64);
    for &(b, c) in &cum {
        if c >= rank {
            if b == f64::INFINITY {
                return cum.iter().rev().map(|x| x.0).find(|x| *x != f64::INFINITY);
            }
            #[allow(clippy::float_cmp)]
            if c == prev_c {
                return Some(b);
            }
            return Some(prev_b + (b - prev_b) * (rank - prev_c) / (c - prev_c));
        }
        prev_b = b;
        prev_c = c;
    }
    None
}

// ── override rendering (pure) ───────────────────────────────────────────────────────────────────

/// (flag, args); the pre-flag prefix is handled separately.
pub type Item = (String, Vec<String>);

/// `python3 -m dynamo.vllm --a 1 --b` -> (prefix, [(--a,[1]), (--b,[])]). Value tokens never start `--`.
#[must_use]
pub fn split_command(tokens: &[String]) -> (Vec<String>, Vec<Item>) {
    let mut prefix = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    for tok in tokens {
        if tok.starts_with("--") {
            items.push((tok.clone(), Vec::new()));
        } else if let Some(last) = items.last_mut() {
            last.1.push(tok.clone());
        } else {
            prefix.push(tok.clone());
        }
    }
    (prefix, items)
}

#[must_use]
pub fn join_command(prefix: &[String], items: &[Item]) -> Vec<String> {
    let mut out = prefix.to_vec();
    for (f, a) in items {
        out.push(f.clone());
        out.extend(a.iter().cloned());
    }
    out
}

/// Replace `flag` (and any `also` aliases) at its original position; None removes it; new flags append.
fn set_flag(items: &mut Vec<Item>, flag: &str, args: Option<Vec<String>>, also: &[&str]) {
    let hit = |f: &str| f == flag || also.contains(&f);
    let pos = items.iter().position(|(f, _)| hit(f));
    items.retain(|(f, _)| !hit(f));
    let Some(args) = args else { return };
    let at = pos.map_or(items.len(), |p| p.min(items.len()));
    items.insert(at, (flag.to_owned(), args));
}

fn norm_flag(f: &str) -> String {
    match f.strip_prefix("--") {
        Some(r) => format!("--{}", r.replace('_', "-")),
        None => f.to_owned(),
    }
}

/// `--no-x` and `--x` are the same switch for duplicate detection.
fn dup_key(flag: &str) -> String {
    match flag.strip_prefix("--") {
        Some(r) => format!("--{}", r.strip_prefix("no-").unwrap_or(r)),
        None => flag.to_owned(),
    }
}

/// Tokens -> ([(normalised flag, values, raw flag)], stray positional tokens). Handles `--flag=value`.
/// (normalised flag, values, raw flag)
type Flag = (String, Vec<String>, String);

fn parse_flags(tokens: &[String]) -> (Vec<Flag>, Vec<String>) {
    let mut out: Vec<Flag> = Vec::new();
    let mut stray = Vec::new();
    for tok in tokens {
        if tok.starts_with('-') && tok.chars().count() > 1 {
            let (raw, val) = match tok.split_once('=') {
                Some((r, v)) => (r.to_owned(), Some(v.to_owned())),
                None => (tok.clone(), None),
            };
            out.push((norm_flag(&raw), val.into_iter().collect(), raw));
        } else if let Some(last) = out.last_mut() {
            last.1.push(tok.clone());
        } else {
            stray.push(tok.clone());
        }
    }
    (out, stray)
}

fn spec_ok(vals: &[String]) -> bool {
    if vals.len() != 1 {
        return false;
    }
    let Some(PyJson::Dict(cfg)) = json_loads_bytes(vals[0].as_bytes()) else {
        return false;
    };
    let keys: HashSet<&str> = cfg.keys().map(String::as_str).collect();
    keys == HashSet::from(["method", "num_speculative_tokens"])
        && matches!(cfg.get("method"), Some(PyJson::Str(m)) if m == "mtp")
        && matches!(cfg.get("num_speculative_tokens"), Some(PyJson::Int(n)) if (1..=8).contains(n))
}

fn find<'a>(items: &'a [Item], flag: &str) -> Option<&'a Vec<String>> {
    items.iter().find(|(f, _)| f == flag).map(|(_, a)| a)
}

/// `json.dumps(v, ensure_ascii=True)` of a string: a YAML double-quoted scalar. `$` is NOT escaped: base-carried
/// `${VAR:-x}` tokens stay live so compose interpolates them exactly as for the base file.
fn yq(s: &str) -> String {
    json_dumps(&Value::String(s.to_owned()))
}

/// `json.dumps(v, separators=(",", ":"), sort_keys=...)` (ensure_ascii).
#[must_use]
pub fn compact_json(v: &Value, sort_keys: bool) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|x| compact_json(x, sort_keys))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            if sort_keys {
                keys.sort();
            }
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        json_dumps(&Value::String(k.clone())),
                        m.get(k)
                            .map_or_else(|| "null".into(), |x| compact_json(x, sort_keys))
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => json_dumps(other),
    }
}

/// A cleaned parameter value (Python int / float / bool / str).
fn int_v(i: i64) -> Value {
    Value::from(i)
}

fn float_v(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

fn value_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => value_repr(other),
    }
}

fn value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => str_repr(s),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(value_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, x)| format!("{}: {}", str_repr(k), value_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => value_str(other),
    }
}

fn param_error(msg: String) -> AdapterError {
    AdapterError::Invalid(msg)
}

fn ints(v: &Value, key: &str, lo: i64, hi: i64) -> Result<i64, AdapterError> {
    if v.is_boolean() {
        return Err(param_error(format!(
            "{key}: expected an integer, got a boolean"
        )));
    }
    let n: Option<i64> = match v {
        Value::String(s) => {
            let t = py_strip(s);
            let digits = t.strip_prefix('-').unwrap_or(t);
            if !digits.is_empty()
                && digits.len() <= 18
                && digits.bytes().all(|b| b.is_ascii_digit())
            {
                t.parse::<i64>().ok()
            } else {
                None
            }
        }
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            #[allow(clippy::cast_possible_truncation)]
            if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.2e18 {
                Some(f as i64)
            } else {
                None
            }
        }
        Value::Number(n) => n.as_i64(),
        _ => None,
    };
    let Some(n) = n else {
        return Err(param_error(format!("{key}: expected an integer")));
    };
    if !(lo..=hi).contains(&n) {
        return Err(param_error(format!("{key}: {n} outside [{lo}, {hi}]")));
    }
    Ok(n)
}

fn floats(v: &Value, key: &str, lo: f64, hi: f64) -> Result<f64, AdapterError> {
    let f = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float(s),
        _ => return Err(param_error(format!("{key}: expected a number"))),
    };
    let Some(f) = f else {
        return Err(param_error(format!("{key}: expected a number")));
    };
    if f.is_nan() || !(lo..=hi).contains(&f) {
        return Err(param_error(format!(
            "{key}: {} outside [{}, {}]",
            value_str(v),
            float_repr(lo),
            float_repr(hi)
        )));
    }
    Ok(f)
}

fn bools(v: &Value, key: &str) -> Result<bool, AdapterError> {
    v.as_bool()
        .ok_or_else(|| param_error(format!("{key}: expected true/false")))
}

fn enums(v: &Value, key: &str, choices: &[String]) -> Result<String, AdapterError> {
    match v {
        Value::String(s) if choices.contains(s) => Ok(s.clone()),
        _ => Err(param_error(format!(
            "{key}: {} is not one of {}",
            value_repr(v),
            list_repr(choices)
        ))),
    }
}

fn spec(
    key: &str,
    flag: &str,
    label: &str,
    t: ParamType,
    group: &str,
    default: Value,
    help: &str,
) -> ParamSpec {
    let mut p = ParamSpec::new(key, label, help, t);
    p.flag = Some(flag.to_owned());
    p.group = group.to_owned();
    p.default = default;
    p
}

/// The parameter catalog (flags and choices from `--help` of the pinned image).
#[must_use]
pub fn catalog() -> Vec<ParamSpec> {
    let w = "worker (dynamo.vllm)";
    let mm = |mut p: ParamSpec, lo: f64, hi: f64| {
        p.min = Some(lo);
        p.max = Some(hi);
        p
    };
    let ch = |mut p: ParamSpec, c: &[&str]| {
        p.choices = Some(owned(c));
        p
    };
    let adv = |mut p: ParamSpec| {
        p.advanced = true;
        p
    };
    vec![
        mm(
            spec(
                "max_model_len",
                "--max-model-len",
                "Context length (tokens)",
                ParamType::Int,
                w,
                int_v(262_144),
                "Model context length (prompt and output). Bigger contexts need more KV cache; the KV budget is set \
             separately by kv_cache_memory_bytes.",
            ),
            1024.0,
            1_048_576.0,
        ),
        mm(
            spec(
                "gpu_memory_utilization",
                "--gpu-memory-utilization",
                "GPU memory utilization",
                ParamType::Float,
                w,
                float_v(0.5),
                "Fraction of GPU memory for this vLLM instance (0..1), per instance. IGNORED by vLLM when \
             kv_cache_memory_bytes is set.",
            ),
            0.05,
            1.0,
        ),
        mm(
            spec(
                "kv_cache_memory_bytes",
                "--kv-cache-memory-bytes",
                "KV cache size (bytes)",
                ParamType::Int,
                w,
                int_v(20_000_000_000),
                "KV cache bytes per GPU. Overrides gpu_memory_utilization when set. (vLLM 0.28 name; 0.27's \
             --kv-cache-memory does not exist here.)",
            ),
            268_435_456.0,
            100_000_000_000.0,
        ),
        mm(
            spec(
                "max_num_seqs",
                "--max-num-seqs",
                "Max concurrent sequences",
                ParamType::Int,
                w,
                int_v(64),
                "Maximum number of sequences processed in a single iteration.",
            ),
            1.0,
            1024.0,
        ),
        mm(
            spec(
                "max_num_batched_tokens",
                "--max-num-batched-tokens",
                "Max batched tokens",
                ParamType::Int,
                w,
                int_v(8192),
                "Maximum tokens processed in a single iteration (chunked-prefill budget).",
            ),
            256.0,
            131_072.0,
        ),
        spec(
            "speculative_enabled",
            "--speculative-config",
            "Speculative decoding (MTP)",
            ParamType::Bool,
            w,
            Value::Bool(true),
            "Multi-token-prediction speculative decoding; the pinned checkpoint ships an MTP draft. Off removes \
             --speculative-config.",
        ),
        mm(
            spec(
                "num_speculative_tokens",
                "--speculative-config",
                "Speculative tokens",
                ParamType::Int,
                w,
                int_v(2),
                "num_speculative_tokens inside the speculative config (method stays mtp).",
            ),
            1.0,
            8.0,
        ),
        ch(
            spec(
                "tool_call_parser",
                "--dyn-tool-call-parser",
                "Tool-call parser",
                ParamType::Enum,
                w,
                Value::from("qwen3_coder"),
                "Dynamo's own dialect (NOT vLLM's --tool-call-parser, which does nothing here). Qwen3.8 uses \
             qwen3_coder (the XML format vLLM calls qwen3_xml).",
            ),
            &TOOL_PARSERS,
        ),
        ch(
            spec(
                "reasoning_parser",
                "--dyn-reasoning-parser",
                "Reasoning parser",
                ParamType::Enum,
                w,
                Value::from("qwen3"),
                "Splits <think> output into reasoning_content. `none` = no reasoning parsing.",
            ),
            &REASONING_PARSERS,
        ),
        adv(ch(
            spec(
                "default_thinking_mode",
                "--dyn-default-thinking-mode",
                "Default thinking mode",
                ParamType::Enum,
                w,
                Value::from("unset"),
                "Deployment-level default for the chat template; a request's chat_template_kwargs win.",
            ),
            &["unset", "enabled", "disabled"],
        )),
        spec(
            "enable_prefix_caching",
            "--enable-prefix-caching",
            "Prefix caching",
            ParamType::Bool,
            w,
            Value::Bool(true),
            "Reuse KV blocks across requests that share a prefix.",
        ),
        spec(
            "enable_chunked_prefill",
            "--enable-chunked-prefill",
            "Chunked prefill",
            ParamType::Bool,
            w,
            Value::Bool(true),
            "Prefill requests are chunked to the remaining max_num_batched_tokens.",
        ),
        adv(spec(
            "enable_structural_tag",
            STRUCTURAL,
            "Structural-tag tool decoding",
            ParamType::Bool,
            w,
            Value::Bool(true),
            "Guided decoding for tool calls. MUST stay ON: without it tool_choice=\"required\" degenerates into \
             whitespace in reasoning_content and returns no tool call (measured 2026-09-23).",
        )),
        ch(
            spec(
                "router_mode",
                "--router-mode",
                "Router mode",
                ParamType::Enum,
                "frontend (dynamo.frontend)",
                Value::from("round-robin"),
                "How the frontend picks a worker. `kv` only helps with more than one worker and needs NATS/ZMQ KV \
             events published by the workers; with one worker it adds a hop for nothing.",
            ),
            &ROUTER_MODES,
        ),
    ]
}

static CATALOG: LazyLock<Vec<ParamSpec>> = LazyLock::new(catalog);

fn worker_flags() -> Vec<String> {
    let mut v: Vec<String> = CATALOG
        .iter()
        .filter(|p| p.group.starts_with("worker"))
        .filter_map(|p| p.flag.clone())
        .collect();
    for f in [
        "--enable-prefix-caching",
        "--no-enable-prefix-caching",
        "--enable-chunked-prefill",
        "--no-enable-chunked-prefill",
        "--no-dyn-enable-structural-tag",
    ] {
        v.push(f.into());
    }
    v.extend(FIXED_FLAGS_WORKER.iter().map(|s| (*s).to_owned()));
    v
}

fn frontend_flags() -> Vec<String> {
    let mut v = vec!["--router-mode".to_owned()];
    v.extend(FIXED_FLAGS_FRONTEND.iter().map(|s| (*s).to_owned()));
    v
}

/// Allowlist + type + range check. Unknown keys are refused (never silently dropped). `ValueError` -> Invalid.
pub fn validate_values(values: &Map<String, Value>) -> Result<Map<String, Value>, AdapterError> {
    let mut clean = Map::new();
    for (key, raw) in values {
        let Some(spec) = CATALOG.iter().find(|p| p.key == *key) else {
            return Err(param_error(format!("unknown parameter {}", str_repr(key))));
        };
        #[allow(clippy::cast_possible_truncation)]
        let v = match spec.type_ {
            ParamType::Int => int_v(ints(
                raw,
                key,
                spec.min.unwrap_or(0.0) as i64,
                spec.max.unwrap_or(0.0) as i64,
            )?),
            ParamType::Float => float_v(floats(
                raw,
                key,
                spec.min.unwrap_or(0.0),
                spec.max.unwrap_or(1.0),
            )?),
            ParamType::Bool => Value::Bool(bools(raw, key)?),
            _ => Value::String(enums(raw, key, spec.choices.as_deref().unwrap_or(&[]))?),
        };
        clean.insert(key.clone(), v);
    }
    Ok(clean)
}

fn as_f(v: Option<&Value>, dflt: f64) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(dflt)
}

/// Warnings for a validated set of values.
#[must_use]
pub fn param_warnings(values: &Map<String, Value>) -> Vec<String> {
    let mut w = Vec::new();
    if values.get("enable_structural_tag") == Some(&Value::Bool(false)) {
        w.push(
            "enable_structural_tag=false: tool_choice=\"required\" will return no tool call on this model (guided \
             decoding degenerates into whitespace). Keep it ON."
                .to_owned(),
        );
    }
    if values.get("router_mode") == Some(&Value::from("kv")) {
        w.push(
            "router_mode=kv needs more than one worker and NATS/ZMQ KV events from the workers; with the single \
             worker (file discovery, no NATS) it adds a hop for nothing and may not route."
                .to_owned(),
        );
    }
    if values.contains_key("kv_cache_memory_bytes") && values.contains_key("gpu_memory_utilization")
    {
        w.push(
            "kv_cache_memory_bytes is set, so vLLM ignores gpu_memory_utilization (per --help)."
                .to_owned(),
        );
    }
    if as_f(values.get("gpu_memory_utilization"), 0.0) > 0.95 {
        w.push("gpu_memory_utilization above 0.95 leaves little headroom on a card shared with other engines.".to_owned());
    }
    #[allow(clippy::cast_possible_truncation)]
    let as_i = |k: &str, d: i64| {
        values
            .get(k)
            .and_then(Value::as_f64)
            .map_or(d, |f| f as i64)
    };
    if values.get("enable_chunked_prefill") == Some(&Value::Bool(false))
        && as_i("max_num_batched_tokens", 1 << 30) < as_i("max_model_len", 0)
    {
        w.push(
            "chunked prefill is off and max_num_batched_tokens < max_model_len; vLLM may refuse to start (recalled \
             from vLLM behaviour, not verified against this image)."
                .to_owned(),
        );
    }
    if values.get("reasoning_parser") == Some(&Value::from("none")) {
        w.push("reasoning_parser=none: <think> text stays inline in content.".to_owned());
    }
    w
}

fn spec_json(items: &[Item], enabled: Option<bool>, n: Option<i64>) -> Option<Vec<String>> {
    let cur = find(items, "--speculative-config");
    let mut cfg: Value = serde_json::json!({"method": "mtp"});
    if let Some(cur) = cur
        && let Some(first) = cur.first()
        && let Some(PyJson::Dict(d)) = json_loads_bytes(first.as_bytes())
    {
        cfg = PyJson::Dict(d).to_value();
    }
    if enabled == Some(false) {
        return None;
    }
    if enabled.is_none() && n.is_none() {
        return cur.cloned();
    }
    if let (Some(n), Value::Object(m)) = (n, &mut cfg) {
        m.insert("num_speculative_tokens".into(), Value::from(n));
    }
    Some(vec![compact_json(&cfg, false)])
}

/// PURE. Base commands + validated values -> (compose override YAML, warnings). Only supplied keys change.
pub fn render_override(
    values: &Map<String, Value>,
    base_worker: &[String],
    base_frontend: Option<&[String]>,
    worker_service: &str,
    frontend_service: &str,
) -> Result<(String, Vec<String>), AdapterError> {
    let v = validate_values(values)?;
    let (wp, wi) = split_command(base_worker);
    let (fp, fi) = split_command(base_frontend.unwrap_or(&[]));
    let mut win = wi;
    let mut fin = fi.clone();
    let num = |win: &mut Vec<Item>, key: &str, flag: &str, two: bool| {
        if let Some(x) = v.get(key) {
            let s = if two {
                format!("{:.2}", x.as_f64().unwrap_or(0.0))
            } else {
                value_str(x)
            };
            set_flag(win, flag, Some(vec![s]), &[]);
        }
    };
    num(&mut win, "max_model_len", "--max-model-len", false);
    num(
        &mut win,
        "gpu_memory_utilization",
        "--gpu-memory-utilization",
        true,
    );
    num(
        &mut win,
        "kv_cache_memory_bytes",
        "--kv-cache-memory-bytes",
        false,
    );
    num(&mut win, "max_num_seqs", "--max-num-seqs", false);
    num(
        &mut win,
        "max_num_batched_tokens",
        "--max-num-batched-tokens",
        false,
    );
    if v.contains_key("speculative_enabled") || v.contains_key("num_speculative_tokens") {
        let args = spec_json(
            &win,
            v.get("speculative_enabled").and_then(Value::as_bool),
            v.get("num_speculative_tokens").and_then(Value::as_i64),
        );
        set_flag(&mut win, "--speculative-config", args, &[]);
    }
    if let Some(Value::String(p)) = v.get("tool_call_parser") {
        set_flag(
            &mut win,
            "--dyn-tool-call-parser",
            Some(vec![p.clone()]),
            &[],
        );
    }
    if let Some(Value::String(p)) = v.get("reasoning_parser") {
        set_flag(
            &mut win,
            "--dyn-reasoning-parser",
            if p == "none" {
                None
            } else {
                Some(vec![p.clone()])
            },
            &[],
        );
    }
    if let Some(Value::String(p)) = v.get("default_thinking_mode") {
        set_flag(
            &mut win,
            "--dyn-default-thinking-mode",
            if p == "unset" {
                None
            } else {
                Some(vec![p.clone()])
            },
            &[],
        );
    }
    for (key, flag) in [
        ("enable_prefix_caching", "--enable-prefix-caching"),
        ("enable_chunked_prefill", "--enable-chunked-prefill"),
    ] {
        if let Some(Value::Bool(on)) = v.get(key) {
            let no = format!("--no{}", &flag[1..]);
            set_flag(
                &mut win,
                flag,
                if *on { Some(Vec::new()) } else { None },
                &[&no],
            );
            if !*on {
                win.push((no, Vec::new()));
            }
        }
    }
    if let Some(Value::Bool(on)) = v.get("enable_structural_tag") {
        set_flag(
            &mut win,
            STRUCTURAL,
            if *on { Some(Vec::new()) } else { None },
            &["--no-dyn-enable-structural-tag"],
        );
    }
    if let Some(Value::String(m)) = v.get("router_mode") {
        set_flag(
            &mut fin,
            "--router-mode",
            if m == "round-robin" {
                None
            } else {
                Some(vec![m.clone()])
            },
            &[],
        );
    }
    let mut lines: Vec<String> = vec![
        "# Generated by Nemo Console (ADR 8). The repo compose is never edited: delete this file to revert.".into(),
        "# Compose REPLACES `command` (it does not merge it), so the full argv is restated below; flags the".into(),
        "# console does not manage are carried over verbatim from the base compose.".into(),
        format!("# nemo-console-values: {}", compact_json(&Value::Object(v.clone()), true)),
    ];
    let warns = param_warnings(&v);
    lines.extend(warns.iter().map(|w| format!("# WARNING: {w}")));
    lines.push("services:".into());
    lines.push(format!("  {worker_service}:"));
    lines.push("    command:".into());
    lines.extend(
        join_command(&wp, &win)
            .iter()
            .map(|t| format!("      - {}", yq(t))),
    );
    if fin != fi && base_frontend.is_some() {
        lines.push(format!("  {frontend_service}:"));
        lines.push("    command:".into());
        lines.extend(
            join_command(&fp, &fin)
                .iter()
                .map(|t| format!("      - {}", yq(t))),
        );
    }
    Ok((lines.join("\n") + "\n", warns))
}

/// The values saved in an override's header (`# nemo-console-values: {...}`), or `{}`.
#[must_use]
pub fn parse_values_header(content: &str) -> Map<String, Value> {
    for line in py_splitlines(content).into_iter().take(12) {
        if line.starts_with("# nemo-console-values:")
            && let Some((_, j)) = line.split_once(':')
            && let Some(PyJson::Dict(d)) = json_loads_bytes(j.as_bytes())
            && let Value::Object(m) = PyJson::Dict(d).to_value()
        {
            return m;
        }
    }
    Map::new()
}

fn attr_err(v: &PyValue, what: &str) -> AdapterError {
    AdapterError::failed(
        "AttributeError",
        format!("'{}' object has no attribute '{what}'", v.type_name()),
    )
}

/// `(doc or {}).get(key)` on a YAML value: None for falsy docs, AttributeError for a truthy non-dict.
fn ydict_get<'a>(v: &'a PyValue, key: &str) -> Result<Option<&'a PyValue>, AdapterError> {
    if !v.truthy() {
        return Ok(None);
    }
    if !v.is_map() {
        return Err(attr_err(v, "get"));
    }
    Ok(v.get(key))
}

/// Worker and frontend argv from the raw base compose (folded `>` string or list).
pub fn load_base_commands(
    text: &str,
    worker_service: &str,
    frontend_service: &str,
) -> Result<(Vec<String>, Vec<String>), AdapterError> {
    let doc = safe_load(text).map_err(|e| AdapterError::failed(e.class(), e.text()))?;
    let mut out = Vec::new();
    for svc in [worker_service, frontend_service] {
        let empty = PyValue::Dict(Vec::new());
        let services = ydict_get(&doc, "services")?
            .cloned()
            .unwrap_or(PyValue::None);
        let services = if services.truthy() {
            services
        } else {
            empty.clone()
        };
        if !services.is_map() {
            return Err(attr_err(&services, "get"));
        }
        let body = services.get(svc).cloned().unwrap_or(empty);
        if !body.is_map() {
            return Err(attr_err(&body, "get"));
        }
        match body.get("command") {
            Some(PyValue::Str(s)) => {
                out.push(shlex::split(s).map_err(|e| AdapterError::Invalid(e.0.to_owned()))?);
            }
            Some(PyValue::List(a)) if a.iter().all(|t| matches!(t, PyValue::Str(_))) => {
                out.push(a.iter().map(PyValue::py_str).collect());
            }
            _ => {
                return Err(param_error(format!(
                    "base compose has no usable command for {svc}"
                )));
            }
        }
    }
    let f = out.pop().unwrap_or_default();
    let w = out.pop().unwrap_or_default();
    Ok((w, f))
}

fn jd(v: Option<&PyJson>) -> Option<&IndexMap<String, PyJson>> {
    match v {
        Some(PyJson::Dict(d)) if !d.is_empty() => Some(d),
        _ => None,
    }
}

/// `((d or {}).get(k1) or {}).get(k2) ...` over a docker inspect document.
fn jpath<'a>(d: Option<&'a PyJson>, path: &[&str]) -> Option<&'a PyJson> {
    let mut cur = d;
    for k in path {
        cur = jd(cur)?.get(*k);
    }
    cur
}

fn jstr(v: Option<&PyJson>) -> Option<String> {
    match v {
        Some(PyJson::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn image_of(d: Option<&PyJson>) -> String {
    let img = jpath(d, &["Config", "Image"]);
    let s = if pj_truthy(img) {
        pj_str(img)
    } else {
        String::new()
    };
    s.split('@').next().unwrap_or("").to_owned()
}

fn is_dynamo_image(d: Option<&PyJson>) -> bool {
    let image = image_of(d);
    if image.is_empty() {
        return false;
    }
    let repo = image.rsplit_once(':').map_or(image.as_str(), |(r, _)| r);
    repo.to_lowercase().contains("dynamo")
}

/// The tag of a Dynamo image, never a digest: `repo:tag@sha256:...` -> tag; no tag, or not a Dynamo image -> None.
#[must_use]
pub fn image_version(d: Option<&PyJson>) -> Option<String> {
    let image = image_of(d);
    let (repo, tag) = image.rsplit_once(':').unwrap_or(("", &image));
    if repo.is_empty() || tag.is_empty() || tag.contains('/') || !is_dynamo_image(d) {
        return None;
    }
    Some(tag.to_owned())
}

/// Every GPU UUID the worker reserves in `docker compose ... config --format json` output (order kept, deduped).
#[must_use]
pub fn merged_worker_uuids(config_json: &str, worker_service: &str) -> Vec<String> {
    let Some(doc) = json_loads_bytes(config_json.as_bytes()) else {
        return Vec::new();
    };
    let doc = match doc {
        PyJson::Dict(d) => Some(PyJson::Dict(d)),
        _ => None,
    };
    let svc = jpath(doc.as_ref(), &["services", worker_service]);
    let devices = jpath(svc, &["deploy", "resources", "reservations", "devices"]);
    let mut out: Vec<String> = Vec::new();
    if let Some(PyJson::List(devs)) = devices {
        for dev in devs {
            if let PyJson::Dict(dev) = dev
                && let Some(PyJson::List(ids)) =
                    dev.get("device_ids").filter(|x| pj_truthy(Some(x)))
            {
                for u in ids {
                    if let PyJson::Str(u) = u
                        && UUID_RE.is_match(u)
                        && !out.contains(u)
                    {
                        out.push(u.clone());
                    }
                }
            }
        }
    }
    out
}

fn time_ms(s: Option<f64>) -> Option<f64> {
    s.map(|x| x * 1000.0)
}

fn exec_script() -> String {
    format!(
        "import urllib.request as u,sys\ndef g(p):\n  try: return u.urlopen('http://127.0.0.1:{WORKER_METRICS_PORT}'+p,timeout=5).read().decode()\n  except Exception as e: return ''\nsys.stdout.write(g('/health')+'\\n---METRICS---\\n'+g('/metrics'))\n"
    )
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn ms(t0: Instant) -> i64 {
    i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// `s[-n:]` in code points.
fn tail(s: &str, n: usize) -> String {
    char_suffix_from(s, char_len(s).saturating_sub(n)).to_owned()
}

/// `(stdout + stderr).strip()` redacted, last n chars.
fn out_tail(r: &CommandResult, n: usize) -> String {
    tail(&redact(py_strip(&format!("{}{}", r.stdout, r.stderr))), n)
}

fn either_strip(r: &CommandResult) -> String {
    let s = if r.stderr.is_empty() {
        &r.stdout
    } else {
        &r.stderr
    };
    py_strip(s).to_owned()
}

/// One container that shares the worker's card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharer {
    pub name: String,
    pub image: String,
    pub project: String,
    pub service: String,
    pub workdir: String,
    pub config: String,
    pub all_gpus: bool,
    pub kind: &'static str,
}

fn describe(s: &Sharer) -> String {
    format!(
        "{} ({}{})",
        s.name,
        s.image,
        if s.all_gpus {
            ", requests ALL gpus"
        } else {
            ""
        }
    )
}

type Insp = HashMap<String, PyJson>;

/// (when, key = files + mtimes, uuids)
type UuidCache = (Instant, Vec<(String, i128)>, Vec<String>);

#[derive(Default)]
struct DynState {
    models: Vec<String>,
    card_uuids: Vec<String>,
    uuid_cache: Option<UuidCache>,
    last_files: Option<ComposeFiles>,
}

pub struct DynamoAdapter {
    pub ctx: AdapterContext,
    pub health_timeout: f64,
    pub poll_interval: f64,
    pub follow_interval: f64,
    /// The container replaces it with the one built from Settings.
    pub policy: CardPolicy,
    /// The container replaces it (addendum 9.11).
    pub names: DynamoNames,
    st: Mutex<DynState>,
}

/// Build the adapter (Python: registry `build_adapter` + the container's configure hook).
pub fn build(ctx: &AdapterContext, cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(DynamoAdapter::from_settings(ctx, cfg)))
}

fn st_get<'a>(d: Option<&'a PyJson>, key: &str) -> Option<&'a PyJson> {
    jpath(d, &["State", key])
}

/// `_state(d)`: absent | failed | running | stopped.
#[must_use]
pub fn container_state(d: Option<&PyJson>) -> &'static str {
    if d.is_none() {
        return "absent";
    }
    let status = jstr(st_get(d, "Status"));
    if pj_truthy(st_get(d, "Restarting")) || status.as_deref() == Some("restarting") {
        return "failed";
    }
    if pj_truthy(st_get(d, "Running")) {
        return "running";
    }
    let code_zero = match st_get(d, "ExitCode") {
        None | Some(PyJson::Null | PyJson::Int(0) | PyJson::Bool(false)) => true,
        Some(PyJson::Float(f)) => *f == 0.0,
        _ => false,
    };
    if status.as_deref() == Some("exited") && !code_zero {
        return "failed";
    }
    "stopped"
}

/// Running or restarting: the container is (trying to be) up, so a stopped Dynamo is not 'off'.
fn active(d: Option<&PyJson>) -> bool {
    pj_truthy(st_get(d, "Running"))
        || pj_truthy(st_get(d, "Restarting"))
        || jstr(st_get(d, "Status")).as_deref() == Some("restarting")
}

fn device_requests(d: Option<&PyJson>) -> Vec<&IndexMap<String, PyJson>> {
    match jpath(d, &["HostConfig", "DeviceRequests"]) {
        Some(PyJson::List(a)) => a
            .iter()
            .filter_map(|r| match r {
                PyJson::Dict(m) => Some(m),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn req_has(r: &IndexMap<String, PyJson>, u: &str) -> bool {
    match r.get("DeviceIDs") {
        Some(PyJson::List(a)) => a.iter().any(|x| matches!(x, PyJson::Str(s) if s == u)),
        Some(PyJson::Str(s)) => s.contains(u),
        _ => false,
    }
}

fn req_all(r: &IndexMap<String, PyJson>) -> bool {
    matches!(r.get("Count"), Some(PyJson::Int(-1)))
        || matches!(r.get("Count"), Some(PyJson::Float(f)) if *f == -1.0)
}

/// The container requests one of these GPUs (by UUID, or all GPUs). Unknown cards: any GPU request counts.
fn pinned(d: &PyJson, uuids: &[String]) -> bool {
    let reqs = device_requests(Some(d));
    if uuids.is_empty() {
        return !reqs.is_empty();
    }
    reqs.iter()
        .any(|r| uuids.iter().any(|u| req_has(r, u)) || req_all(r))
}

impl DynamoAdapter {
    /// `DynamoAdapter(ctx)` with the default health budget, intervals, policy and names.
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        DynamoAdapter {
            ctx,
            health_timeout: 600.0,
            poll_interval: 5.0,
            follow_interval: 2.0,
            policy: CardPolicy::default(),
            names: DynamoNames::default(),
            st: Mutex::new(DynState::default()),
        }
    }

    /// `build_adapter` + `container._configure_dynamo`.
    #[must_use]
    pub fn from_settings(ctx: &AdapterContext, cfg: &Settings) -> Self {
        let mut a = DynamoAdapter::new(ctx.clone());
        a.names = DynamoNames::from_settings(cfg);
        a.policy = CardPolicy::from_settings(cfg, &a.names);
        a
    }

    fn state(&self) -> std::sync::MutexGuard<'_, DynState> {
        match self.st.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    // ── plumbing ────────────────────────────────────────────────────────────────────────────
    #[must_use]
    pub fn base_url(&self) -> String {
        self.ctx
            .urls
            .get("dynamo")
            .map_or(DEFAULT_URL, String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn compose_path(&self) -> Option<PathBuf> {
        self.ctx.paths.get("dynamo_compose").cloned()
    }

    /// `<state>/dynamo/override.yml`.
    #[must_use]
    pub fn override_path(&self) -> PathBuf {
        self.ctx.state_dir.join("dynamo").join("override.yml")
    }

    fn override_active(&self) -> bool {
        let p = self.override_path();
        let is_link = p
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink());
        p.is_file()
            && !is_link
            && std::fs::read_to_string(&p).is_ok_and(|t| !py_strip(&t).is_empty())
    }

    /// docker compose with exactly the files in use (+ the console override on the plain base stack).
    pub fn compose_argv(
        &self,
        cf: &ComposeFiles,
        tail: &[&str],
        profile_mock: bool,
    ) -> Result<Vec<String>, AdapterError> {
        let Some(base) = self.compose_path() else {
            return Err(AdapterError::FileNotFound(
                "ctx.paths['dynamo_compose'] is not configured".into(),
            ));
        };
        if let Some(e) = &cf.error {
            return Err(AdapterError::failed("PermissionError", e.clone()));
        }
        let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut a = vec![
            "docker".to_owned(),
            "compose".to_owned(),
            "--project-directory".to_owned(),
            dir.display().to_string(),
        ];
        let files = if cf.files.is_empty() {
            vec![base]
        } else {
            cf.files.clone()
        };
        for f in files {
            a.push("-f".into());
            a.push(f.display().to_string());
        }
        if cf.overrides().is_empty() && self.override_active() {
            a.push("-f".into());
            a.push(self.override_path().display().to_string());
        }
        if profile_mock {
            a.push("--profile".into());
            a.push("mock".into());
        }
        a.extend(tail.iter().map(|s| (*s).to_owned()));
        Ok(a)
    }

    /// A container's config_files label -> resolved repo files, or an error (untrusted input).
    fn trusted_files(&self, raw: &str, workdir: Option<&PyJson>) -> (Vec<PathBuf>, Option<String>) {
        let Some(base) = self.compose_path() else {
            return (
                Vec::new(),
                Some("ctx.paths['dynamo_compose'] is not configured".into()),
            );
        };
        let base_dir = resolve_lenient(base.parent().unwrap_or(Path::new("/")));
        let own = resolve_lenient(&self.override_path());
        if let Some(PyJson::Str(w)) = workdir
            && !w.is_empty()
            && resolve_lenient(Path::new(w)) != base_dir
        {
            return (
                Vec::new(),
                Some(format!(
                    "the stack's project directory {} is not {}",
                    str_repr(char_prefix(w, 120)),
                    base_dir.display()
                )),
            );
        }
        let parts: Vec<String> = raw.split(',').map(|x| py_strip(x).to_owned()).collect();
        if parts.len() > MAX_LABEL_FILES {
            return (
                Vec::new(),
                Some(format!(
                    "more than {MAX_LABEL_FILES} compose files in the label"
                )),
            );
        }
        let mut out: Vec<PathBuf> = Vec::new();
        for part in &parts {
            if !part.starts_with('/') || part.contains('\0') || char_len(part) > 4096 {
                return (
                    Vec::new(),
                    Some(format!(
                        "compose file {} in the label is not an absolute path",
                        str_repr(char_prefix(part, 120))
                    )),
                );
            }
            let Ok(rp) = std::fs::canonicalize(part) else {
                return (
                    Vec::new(),
                    Some(format!(
                        "compose file {} from the label does not exist (any more)",
                        str_repr(char_prefix(&file_name(Path::new(part)), 120))
                    )),
                );
            };
            if rp == own {
                continue;
            }
            let ok = rp.parent() == Some(base_dir.as_path())
                && rp.is_file()
                && COMPOSE_FILE_RE.is_match(&file_name(&rp));
            if !ok {
                return (
                    Vec::new(),
                    Some(format!(
                        "compose file {} from the label is not a compose file inside {} (a symlink out of it counts as \
                         outside)",
                        str_repr(char_prefix(part, 160)),
                        base_dir.display()
                    )),
                );
            }
            if !out.contains(&rp) {
                out.push(rp);
            }
        }
        (out, None)
    }

    /// The files the live stack was started from; the configured base when no Dynamo container exists.
    pub async fn compose_files(&self, insp: Option<&Insp>) -> Result<ComposeFiles, AdapterError> {
        let Some(base) = self.compose_path() else {
            return Ok(ComposeFiles {
                files: Vec::new(),
                base: None,
                from_label: false,
                error: Some("ctx.paths['dynamo_compose'] is not configured".into()),
            });
        };
        let fetched;
        let insp = match insp {
            Some(i) => i,
            None => {
                fetched = self
                    .inspect(&[
                        self.names.worker.clone(),
                        self.names.frontend.clone(),
                        self.names.sv_frontend.clone(),
                        self.names.mocker.clone(),
                    ])
                    .await?;
                &fetched
            }
        };
        let mut seen: Vec<(Vec<PathBuf>, Vec<String>)> = Vec::new();
        for name in [
            &self.names.worker,
            &self.names.frontend,
            &self.names.sv_frontend,
            &self.names.mocker,
        ] {
            let d = insp.get(name.as_str());
            let lb = jpath(d, &["Config", "Labels"]);
            let lb = jd(lb);
            let raw = lb.and_then(|l| l.get(LABEL_FILES));
            let project = lb.and_then(|l| jstr(l.get("com.docker.compose.project")));
            let Some(PyJson::Str(raw)) = raw else {
                continue;
            };
            if d.is_none()
                || project.as_deref() != Some(self.names.project.as_str())
                || py_strip(raw).is_empty()
            {
                continue;
            }
            let (files, err) = self.trusted_files(raw, lb.and_then(|l| l.get(LABEL_WORKDIR)));
            if let Some(err) = err {
                let cf = ComposeFiles {
                    files: Vec::new(),
                    base: Some(base),
                    from_label: true,
                    error: Some(format!("{name}: {err}")),
                };
                self.state().last_files = Some(cf.clone());
                return Ok(cf);
            }
            match seen.iter_mut().find(|(f, _)| *f == files) {
                Some((_, names)) => names.push(name.clone()),
                None => seen.push((files, vec![name.clone()])),
            }
        }
        let cf = if seen.len() > 1 {
            let why: Vec<String> = seen
                .iter()
                .map(|(fs, n)| {
                    let names: Vec<String> = fs.iter().map(|f| file_name(f)).collect();
                    let from = if names.is_empty() {
                        "the console override only".to_owned()
                    } else {
                        names.join(", ")
                    };
                    format!("{} from {from}", n.join(", "))
                })
                .collect();
            ComposeFiles {
                files: Vec::new(),
                base: Some(base),
                from_label: true,
                error: Some(format!(
                    "the Dynamo containers were started from different compose files ({})",
                    why.join("; ")
                )),
            }
        } else if let Some((files, _)) = seen.first() {
            let files = if files.is_empty() {
                vec![resolve_lenient(&base)]
            } else {
                files.clone()
            };
            ComposeFiles {
                files,
                base: Some(base),
                from_label: true,
                error: None,
            }
        } else {
            ComposeFiles::new(vec![resolve_lenient(&base)], Some(base))
        };
        self.state().last_files = Some(cf.clone());
        Ok(cf)
    }

    /// Why `what` (Params / the console override) cannot apply to the stack in use, or None.
    #[must_use]
    pub fn refusal(&self, cf: &ComposeFiles, what: &str) -> Option<String> {
        if let Some(e) = &cf.error {
            return Some(format!(
                "refused: {what} needs the compose files in use, and they cannot be trusted: {e}"
            ));
        }
        if !cf.overrides().is_empty() {
            let base = cf
                .base
                .as_ref()
                .map_or_else(|| "the base compose".to_owned(), |b| file_name(b));
            return Some(format!(
                "refused: the live Dynamo stack runs from {}. {what} would write the worker command of {base} into \
                 the console override, which would replace that stack's worker. Stop it first (console action \
                 `down`), or change its compose files in git.",
                cf.names()
            ));
        }
        None
    }

    /// `docker inspect` (rc!=0 when some are absent, stdout still holds the rest). Verified by name.
    pub async fn inspect(&self, names: &[String]) -> Result<Insp, AdapterError> {
        let safe: Vec<String> = names
            .iter()
            .filter(|n| NAME_RE.is_match(n))
            .cloned()
            .collect();
        if safe.is_empty() {
            return Ok(HashMap::new());
        }
        let mut a = argv(&["docker", "inspect"]);
        a.extend(safe.iter().cloned());
        let res = self
            .ctx
            .runner
            .run(&a, RunOpts::timeout(15.0).max_bytes(2_000_000))
            .await?;
        let text = if res.stdout.is_empty() {
            "[]"
        } else {
            res.stdout.as_str()
        };
        let Some(data) = json_loads_bytes(text.as_bytes()) else {
            return Ok(HashMap::new());
        };
        let mut out = HashMap::new();
        if let PyJson::List(items) = data {
            for d in items {
                let PyJson::Dict(m) = &d else { continue };
                let name = m.get("Name").map_or_else(String::new, |n| pj_str(Some(n)));
                let name = name.trim_start_matches('/').to_owned();
                if safe.contains(&name) {
                    out.insert(name, d);
                }
            }
        }
        Ok(out)
    }

    fn project_ok(&self, d: &PyJson, service: &str) -> bool {
        let lb = jd(jpath(Some(d), &["Config", "Labels"]));
        lb.and_then(|l| jstr(l.get("com.docker.compose.project")))
            .as_deref()
            == Some(self.names.project.as_str())
            && lb
                .and_then(|l| jstr(l.get("com.docker.compose.service")))
                .as_deref()
                == Some(service)
    }

    /// Every GPU UUID the worker pins in the base compose file (empty when unreadable).
    fn base_yaml_uuids(&self) -> Vec<String> {
        let Some(base) = self.compose_path() else {
            return Vec::new();
        };
        let Ok(text) = std::fs::read_to_string(&base) else {
            return Vec::new();
        };
        let Ok(doc) = safe_load(&text) else {
            return Vec::new();
        };
        let mut cur = Some(&doc);
        for k in [
            "services",
            self.names.svc_worker.as_str(),
            "deploy",
            "resources",
            "reservations",
            "devices",
        ] {
            cur = cur.and_then(|c| c.get(k));
        }
        let Some(PyValue::List(devices)) = cur else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for dev in devices {
            let Some(ids) = dev.get("device_ids") else {
                if !dev.is_map() {
                    return Vec::new();
                }
                continue;
            };
            if let PyValue::List(ids) = ids {
                for u in ids {
                    let s = u.py_str();
                    if UUID_RE.is_match(&s) {
                        out.push(s);
                    }
                }
            }
        }
        out
    }

    /// The worker's GPUs: the MERGED config of the files in use (cached per file set and mtimes); else the base file
    /// itself (only without a repo override); else the worker container's own device requests.
    async fn worker_uuids(
        &self,
        cf: &ComposeFiles,
        wk: Option<&PyJson>,
    ) -> Result<Vec<String>, AdapterError> {
        if cf.error.is_none() && self.compose_path().is_some() {
            let mut files = cf.files.clone();
            if cf.overrides().is_empty() && self.override_active() {
                files.push(self.override_path());
            }
            let mut key: Vec<(String, i128)> = Vec::new();
            for f in &files {
                match std::fs::metadata(f) {
                    Ok(m) => {
                        let ns = std::os::unix::fs::MetadataExt::mtime(&m) as i128 * 1_000_000_000
                            + i128::from(std::os::unix::fs::MetadataExt::mtime_nsec(&m));
                        key.push((f.display().to_string(), ns));
                    }
                    Err(_) => {
                        key.clear();
                        break;
                    }
                }
            }
            if let Some((at, k, uuids)) = &self.state().uuid_cache
                && !key.is_empty()
                && *k == key
                && at.elapsed().as_secs_f64() < UUID_TTL
            {
                return Ok(uuids.clone());
            }
            let a = self.compose_argv(cf, &["config", "--format", "json"], false)?;
            let res = self
                .ctx
                .runner
                .run(&a, RunOpts::timeout(30.0).max_bytes(2_000_000))
                .await?;
            let uuids = if res.returncode == 0 {
                merged_worker_uuids(&res.stdout, &self.names.svc_worker)
            } else {
                Vec::new()
            };
            if !uuids.is_empty() {
                if !key.is_empty() {
                    self.state().uuid_cache = Some((Instant::now(), key, uuids.clone()));
                }
                return Ok(uuids);
            }
            if cf.overrides().is_empty() {
                let b = self.base_yaml_uuids();
                if !b.is_empty() {
                    return Ok(b);
                }
            }
        }
        let mut out: Vec<String> = Vec::new();
        for req in device_requests(wk) {
            if let Some(PyJson::List(ids)) = req.get("DeviceIDs") {
                for u in ids {
                    if let PyJson::Str(u) = u
                        && UUID_RE.is_match(u)
                        && !out.contains(u)
                    {
                        out.push(u.clone());
                    }
                }
            }
        }
        Ok(out)
    }

    async fn get_json(&self, path: &str) -> Option<PyJson> {
        let r = self
            .ctx
            .http
            .get(&format!("{}{path}", self.base_url()), 5.0)
            .await
            .ok()?;
        if r.status != 200 {
            return None;
        }
        json_loads_bytes(&r.body)
    }

    /// Worker /health + /metrics from INSIDE the container (host cannot reach the bus IP in rootless).
    async fn worker_scrape(
        &self,
    ) -> Result<(Option<IndexMap<String, PyJson>>, Vec<Sample>), AdapterError> {
        let script = exec_script();
        let a = argv(&[
            "docker",
            "exec",
            &self.names.worker,
            "python3",
            "-c",
            &script,
        ]);
        let res = self
            .ctx
            .runner
            .run(&a, RunOpts::timeout(20.0).max_bytes(2_000_000))
            .await?;
        if res.returncode != 0 || !res.stdout.contains("---METRICS---") {
            return Ok((None, Vec::new()));
        }
        let (head, body) = res.stdout.split_once("---METRICS---").unwrap_or(("", ""));
        let h = py_strip(head);
        let health = match json_loads_bytes(if h.is_empty() { b"null" } else { h.as_bytes() }) {
            Some(PyJson::Dict(d)) => Some(d),
            _ => None,
        };
        Ok((health, parse_prom(body)))
    }

    // ── observe ─────────────────────────────────────────────────────────────────────────────
    fn endpoints(&self) -> Vec<Endpoint> {
        let b = self.base_url();
        let e = |name: &str, url: String, role: &str| Endpoint {
            name: name.into(),
            url,
            role: role.into(),
            auth: "none".into(),
        };
        vec![
            e("frontend api", format!("{b}/v1"), "api"),
            e("frontend health", format!("{b}/health"), "health"),
            e("frontend metrics", format!("{b}/metrics"), "metrics"),
            e(
                "worker metrics (ai-lab bus, read via docker exec)",
                format!("http://dynamo-worker:{WORKER_METRICS_PORT}/metrics"),
                "metrics",
            ),
        ]
    }

    fn version_of(docs: &[Option<&PyJson>]) -> Option<String> {
        docs.iter().find_map(|d| image_version(*d))
    }

    fn off_status(
        &self,
        now: f64,
        fe: Option<&PyJson>,
        wk: Option<&PyJson>,
        mk: Option<&PyJson>,
        sv: Option<&PyJson>,
    ) -> SystemStatus {
        let mut comps = Vec::new();
        for (name, d) in [
            (&self.names.frontend, fe),
            (&self.names.sv_frontend, sv),
            (&self.names.worker, wk),
            (&self.names.mocker, mk),
        ] {
            if d.is_none() && (*name == self.names.mocker || *name == self.names.sv_frontend) {
                continue;
            }
            let code = st_get(d, "ExitCode");
            let zero = matches!(
                code,
                None | Some(PyJson::Null | PyJson::Int(0) | PyJson::Bool(false))
            ) || matches!(code, Some(PyJson::Float(f)) if *f == 0.0);
            comps.push(Component {
                kind: "container".into(),
                name: name.clone(),
                state: if d.is_none() { "absent" } else { "stopped" }.into(),
                image: jstr(jpath(d, &["Config", "Image"])),
                detail: if d.is_some() && !zero {
                    Some(format!("exited (code {})", pj_str(code)))
                } else {
                    None
                },
            });
        }
        let problem = Problem::new(
            Severity::Info,
            "dynamo.off",
            "Dynamo is opt-in and off: no frontend, worker or mocker container is running or restarting. Start it \
             with the fix: it frees the worker's card first (the console's `up` is refused while an exclusive engine \
             holds the card).",
            Some(self.policy.start.clone()),
        );
        SystemStatus {
            system: SystemId::Dynamo,
            health: Health::Off,
            version: Self::version_of(&[wk, sv, fe]),
            summary: "off (opt-in): no Dynamo container is running".into(),
            endpoints: self.endpoints(),
            components: comps,
            problems: vec![problem],
            checked_at: now,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn status_inner(&self, now: f64) -> Result<SystemStatus, AdapterError> {
        let pol = &self.policy;
        let excl = pol.exclusive_names();
        let mut names = vec![
            self.names.frontend.clone(),
            self.names.worker.clone(),
            self.names.mocker.clone(),
            self.names.sv_frontend.clone(),
        ];
        names.extend(excl.iter().cloned());
        let insp = self.inspect(&names).await?;
        let (fe, wk, mk, sv) = (
            insp.get(&self.names.frontend),
            insp.get(&self.names.worker),
            insp.get(&self.names.mocker),
            insp.get(&self.names.sv_frontend),
        );
        let (fe_state, wk_state, mk_state) = (
            container_state(fe),
            container_state(wk),
            container_state(mk),
        );
        if pol.opt_in && ![fe, wk, mk, sv].iter().any(|d| active(*d)) {
            return Ok(self.off_status(now, fe, wk, mk, sv));
        }
        let mut problems: Vec<Problem> = Vec::new();
        let mut comps: Vec<Component> = Vec::new();
        let cf = self.compose_files(Some(&insp)).await?;
        let uuids = self.worker_uuids(&cf, wk).await?;
        let excl_running: Vec<String> = excl
            .iter()
            .filter(|n| container_state(insp.get(*n)) == "running")
            .cloned()
            .collect();
        let excl_on_card: Vec<String> = excl_running
            .iter()
            .filter(|n| insp.get(*n).is_some_and(|d| pinned(d, &uuids)))
            .cloned()
            .collect();
        let overrides = !cf.overrides().is_empty();
        let start_fix = if overrides {
            format!(
                "run the console action `up` (it uses the files this stack runs from: {})",
                cf.names()
            )
        } else {
            pol.start.clone()
        };
        let (mut models_doc, mut health_doc) = (None, None);
        if fe_state == "running" {
            health_doc = self.get_json("/health").await;
            models_doc = self.get_json("/v1/models").await;
        }
        let models: Vec<String> = match jd(models_doc.as_ref()).and_then(|m| m.get("data")) {
            Some(PyJson::List(a)) => a
                .iter()
                .filter_map(|m| match m {
                    PyJson::Dict(m) if pj_truthy(m.get("id")) => Some(pj_str(m.get("id"))),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        if !models.is_empty() {
            self.state().models.clone_from(&models);
        }
        let fe_http = health_doc.is_some();
        let fe_health = jpath(fe, &["State", "Health", "Status"]);
        let gateway = sv.is_some() && fe.is_some() && !is_dynamo_image(fe);
        let mut fdetail = String::new();
        if gateway {
            fdetail.push_str(&format!("gateway to {}; ", self.names.svc_sv_frontend));
        }
        if pj_truthy(fe_health) {
            fdetail.push_str(&format!("docker health: {}; ", pj_str(fe_health)));
        }
        if fe_http {
            fdetail.push_str("HTTP ok");
        } else if fe_state == "running" {
            fdetail.push_str("HTTP unreachable");
        }
        comps.push(Component {
            kind: "container".into(),
            name: self.names.frontend.clone(),
            state: fe_state.into(),
            image: jstr(jpath(fe, &["Config", "Image"])),
            detail: Some(fdetail),
        });
        if sv.is_some() {
            let sv_health = jpath(sv, &["State", "Health", "Status"]);
            let mut d = "the Dynamo frontend, on an internal network behind the gateway".to_owned();
            if pj_truthy(sv_health) {
                d.push_str(&format!("; docker health: {}", pj_str(sv_health)));
            }
            comps.push(Component {
                kind: "container".into(),
                name: self.names.sv_frontend.clone(),
                state: container_state(sv).into(),
                image: jstr(jpath(sv, &["Config", "Image"])),
                detail: Some(d),
            });
        }
        let loading = wk_state == "running" && fe_http && models.is_empty();
        let wk_detail = if wk_state == "running" {
            if loading {
                Some(
                    "loading: model not registered with the frontend yet (cold start ~4 min)"
                        .to_owned(),
                )
            } else if models.is_empty() {
                None
            } else {
                Some(format!("serving {}", models.join(", ")))
            }
        } else if active(wk) {
            let rc = jpath(wk, &["RestartCount"]);
            #[allow(clippy::cast_possible_truncation)]
            let n = match rc {
                Some(PyJson::Int(i)) => *i,
                Some(PyJson::Float(f)) => *f as i128,
                Some(PyJson::Bool(b)) => i128::from(*b),
                _ => 0,
            };
            Some(format!("restarting (crash loop?), restart count {n}"))
        } else {
            None
        };
        comps.push(Component {
            kind: "container".into(),
            name: self.names.worker.clone(),
            state: wk_state.into(),
            image: jstr(jpath(wk, &["Config", "Image"])),
            detail: wk_detail,
        });
        if mk.is_some() {
            comps.push(Component {
                kind: "container".into(),
                name: self.names.mocker.clone(),
                state: mk_state.into(),
                image: jstr(jpath(mk, &["Config", "Image"])),
                detail: Some("CPU mocker (gibberish by design)".into()),
            });
        }
        // problems ------------------------------------------------------------------------------
        if let Some(e) = &cf.error {
            problems.push(Problem::new(
                Severity::Warn,
                "dynamo.compose.untrusted",
                format!("Lifecycle actions and Params are refused: {e}"),
                Some(format!(
                    "docker inspect --format '{{{{index .Config.Labels \"{LABEL_FILES}\"}}}}' {}",
                    self.names.worker
                )),
            ));
        } else if overrides {
            problems.push(Problem::new(
                Severity::Info,
                "dynamo.compose.override",
                format!(
                    "The stack runs from {} (its compose labels). up, down and restart use exactly these files; Params \
                     and the console override are refused while it does, because they restate the base worker command.",
                    cf.names()
                ),
                None,
            ));
        }
        if fe_state != "running" {
            problems.push(Problem::new(
                Severity::Error,
                "dynamo.frontend.down",
                format!("frontend container {} is {fe_state}", self.names.frontend),
                Some(start_fix.clone()),
            ));
        } else if sv.is_some() && container_state(sv) != "running" {
            problems.push(Problem::new(
                Severity::Error,
                "dynamo.frontend.down",
                format!(
                    "frontend container {} (behind the gateway) is {}",
                    self.names.sv_frontend,
                    container_state(sv)
                ),
                Some(start_fix.clone()),
            ));
        } else if !fe_http {
            problems.push(Problem::new(
                Severity::Error,
                "dynamo.frontend.unreachable",
                format!(
                    "{} runs but {}/health did not answer",
                    self.names.frontend,
                    self.base_url()
                ),
                Some(format!("docker logs --tail 50 {}", self.names.frontend)),
            ));
        }
        if wk_state == "absent" && mk_state != "running" {
            problems.push(Problem::new(
                if fe_state == "running" {
                    Severity::Warn
                } else {
                    Severity::Info
                },
                "dynamo.worker.absent",
                "no Dynamo worker container: the frontend has nothing to route to",
                Some(start_fix.clone()),
            ));
        } else if wk_state == "stopped" || wk_state == "failed" {
            problems.push(Problem::new(
                Severity::Error,
                "dynamo.worker.stopped",
                format!(
                    "worker {} is {}",
                    self.names.worker,
                    if active(wk) {
                        "restarting (crash loop)"
                    } else {
                        wk_state
                    }
                ),
                Some(format!("docker logs --tail 80 {}", self.names.worker)),
            ));
        }
        if active(wk) && !excl_on_card.is_empty() {
            let keep = if overrides {
                "To keep this Dynamo stack, stop that engine instead".to_owned()
            } else {
                format!("To run Dynamo instead, switch with: {}", pol.start)
            };
            problems.push(Problem::new(
                Severity::Error,
                "dynamo.card.contended",
                format!(
                    "{} and the Dynamo worker are both on the worker's card; they cannot share its VRAM (the worker \
                     crash-loops or the engine fails). {keep}",
                    excl_on_card.join(", ")
                ),
                Some("run the console action `down` (stops only Dynamo; the other engine keeps running)".into()),
            ));
        } else if wk_state == "running" && excl_running.is_empty() && !pol.dark.is_empty() {
            problems.push(Problem::new(
                Severity::Info,
                "dynamo.holds_card",
                format!(
                    "Dynamo holds the worker's card(s) and no exclusive engine is running, so these are dark: {}",
                    pol.dark_text()
                ),
                Some(pol.restore.clone()),
            ));
        }
        if mk_state == "running" {
            problems.push(Problem::new(
                Severity::Info,
                "dynamo.mocker.running",
                "the CPU mocker is registered as the model; its text is gibberish by design",
                Some("run the console action `down` to remove it".into()),
            ));
        }
        if wk_state == "running" && wk.is_some() && !overrides {
            let cmd = cmd_of(wk);
            if !cmd.iter().any(|c| c == STRUCTURAL) {
                problems.push(Problem::new(
                    Severity::Warn,
                    "dynamo.structural_tag.off",
                    "worker runs without --dyn-enable-structural-tag: tool_choice=\"required\" returns no tool call on \
                     this model",
                    Some("Params: set enable_structural_tag=true and apply".into()),
                ));
            }
            if cmd
                .iter()
                .any(|c| c == "--tool-call-parser" || c == "--reasoning-parser")
            {
                problems.push(Problem::new(
                    Severity::Warn,
                    "dynamo.parser.dialect",
                    "worker was started with vLLM's --tool-call-parser/--reasoning-parser; Dynamo parses in the Rust \
                     frontend via --dyn-* flags, so these do nothing useful",
                    Some("Params: set tool_call_parser / reasoning_parser (renders --dyn-*)".into()),
                ));
            }
        }
        if fe_state == "running" && fe.is_some() && !overrides {
            let fcmd = cmd_of(fe);
            if let Some(i) = fcmd.iter().position(|c| c == "--router-mode")
                && fcmd.get(i + 1).map(String::as_str) == Some("kv")
            {
                problems.push(Problem::new(
                    Severity::Warn,
                    "dynamo.router.kv",
                    "frontend runs --router-mode kv with a single worker and no NATS",
                    Some("Params: router_mode=round-robin and apply".into()),
                ));
            }
        }
        // verdict -------------------------------------------------------------------------------
        let (health, summary) = if fe_state != "running" || !fe_http {
            if fe_state != "running" {
                (Health::Down, "down: frontend not running".to_owned())
            } else {
                (Health::Degraded, "frontend unreachable".to_owned())
            }
        } else if wk_state != "running" && mk_state != "running" {
            (
                Health::Degraded,
                "frontend up, no worker (nothing to serve)".to_owned(),
            )
        } else if loading || models.is_empty() {
            (
                Health::Degraded,
                "loading: worker up, model not yet registered (cold start ~4 min)".to_owned(),
            )
        } else {
            let more = if models.len() > 1 {
                format!(" (+{})", models.len() - 1)
            } else {
                String::new()
            };
            (Health::Up, format!("serving {}{more}", models[0]))
        };
        Ok(SystemStatus {
            system: SystemId::Dynamo,
            health,
            version: Self::version_of(&[wk, sv, fe]),
            summary,
            endpoints: self.endpoints(),
            components: comps,
            problems,
            checked_at: now,
        })
    }

    async fn logs(
        &self,
        name: &str,
        component: &str,
        tail_n: Option<usize>,
        since: Option<f64>,
    ) -> Result<Vec<SystemEvent>, AdapterError> {
        let insp = self.inspect(&[name.to_owned()]).await?;
        let Some(d) = insp.get(name) else {
            return Ok(Vec::new());
        };
        let svcs = [
            &self.names.svc_frontend,
            &self.names.svc_worker,
            &self.names.svc_mocker,
            &self.names.svc_sv_frontend,
        ];
        if !svcs.iter().any(|s| self.project_ok(d, s)) {
            return Ok(Vec::new());
        }
        let component = if name == self.names.frontend && !is_dynamo_image(Some(d)) {
            "gateway" // the Super VL variant's nginx at the dynamo-frontend service name
        } else {
            component
        };
        let mut a = argv(&["docker", "logs", "--timestamps"]);
        if let Some(t) = tail_n {
            a.push("--tail".into());
            a.push(t.clamp(1, 1000).to_string());
        }
        if let Some(s) = since {
            a.push("--since".into());
            a.push(format!("{s:.3}"));
        }
        a.push(name.to_owned());
        let res = self
            .ctx
            .runner
            .run(&a, RunOpts::timeout(20.0).max_bytes(512_000))
            .await?;
        let all = format!("{}\n{}", res.stdout, res.stderr);
        Ok(py_splitlines(&all)
            .into_iter()
            .filter_map(|l| parse_log_line(l, component))
            .collect())
    }

    // ── configure ───────────────────────────────────────────────────────────────────────────
    fn display(&self, p: &Path) -> String {
        let s = p.display().to_string();
        let home = self
            .ctx
            .environ
            .get("HOME")
            .unwrap_or_else(|| crate::config::home_dir().display().to_string());
        let home = home.trim_end_matches('/');
        match s.strip_prefix(&format!("{home}/")) {
            Some(rest) => format!("~/{rest}"),
            None => s,
        }
    }

    fn specs(&self) -> Vec<ConfigFileSpec> {
        vec![
            ConfigFileSpec {
                id: "compose".into(),
                label: "Dynamo compose (repo)".into(),
                description: "Frontend + worker (+ CPU mocker profile) for NVIDIA Dynamo 1.5.0. Source of truth in git."
                    .into(),
                display_path: "inference/dynamo/docker-compose.yml".into(),
                format: "yaml".into(),
                editable: false,
                why_readonly: Some(
                    "The console changes Dynamo tunables through a generated compose override in its own state dir \
                     (ADR 8) and never edits the reviewed repo compose; edit it via git."
                        .into(),
                ),
                validators: vec!["docker compose config".into()],
                authoritative: true,
                apply_effect: "n/a (read-only)".into(),
            },
            ConfigFileSpec {
                id: "override".into(),
                label: "Dynamo compose override (console-generated)".into(),
                description: "Restates the worker (and optionally frontend) `command` with the console-managed \
                              tunables. Delete it to fall back to the repo compose."
                    .into(),
                display_path: self.display(&self.override_path()),
                format: "yaml".into(),
                editable: true,
                why_readonly: None,
                validators: vec![
                    "allowlist (services/flags/env)".into(),
                    "docker compose config -q".into(),
                    "refused while the live stack runs from repo override files".into(),
                ],
                authoritative: true,
                apply_effect: "recreates dynamo-worker (~4 min cold start); the router aliases Dynamo serves and the \
                               Dynamo playground are unavailable meanwhile"
                    .into(),
            },
        ]
    }

    /// Per service: (base command tokens, base environment keys). Empty when the base is unreadable, which makes
    /// every `$`, `--model` and env key in a candidate fail closed.
    fn base_facts(&self) -> HashMap<String, (Vec<String>, HashSet<String>)> {
        let mut facts = HashMap::new();
        let Some(base) = self.compose_path() else {
            return HashMap::new();
        };
        let Ok(text) = std::fs::read_to_string(&base) else {
            return HashMap::new();
        };
        let Ok(doc) = safe_load(&text) else {
            return HashMap::new();
        };
        for svc in [&self.names.svc_worker, &self.names.svc_frontend] {
            let Some(body) = doc.get("services").and_then(|s| s.get(svc)) else {
                return HashMap::new();
            };
            if !body.is_map() {
                return HashMap::new();
            }
            let toks: Vec<String> = match body.get("command") {
                Some(PyValue::Str(s)) => match shlex::split(s) {
                    Ok(t) => t,
                    Err(_) => return HashMap::new(),
                },
                None | Some(PyValue::None) => Vec::new(),
                Some(PyValue::List(a)) => a.iter().map(PyValue::py_str).collect(),
                Some(PyValue::Dict(m)) => m.iter().map(|(k, _)| k.py_str()).collect(),
                Some(v) if !v.truthy() => Vec::new(),
                Some(_) => return HashMap::new(),
            };
            let env = body.get("environment").cloned().unwrap_or(PyValue::None);
            let keys: HashSet<String> = match &env {
                v if !v.truthy() => HashSet::new(),
                PyValue::Dict(m) => m.iter().map(|(k, _)| k.py_str()).collect(),
                PyValue::List(a) => a
                    .iter()
                    .map(|e| e.py_str().split('=').next().unwrap_or("").to_owned())
                    .collect(),
                PyValue::Str(s) => s.chars().map(|c| c.to_string()).collect(),
                _ => return HashMap::new(),
            };
            facts.insert(svc.clone(), (toks, keys));
        }
        facts
    }

    #[allow(clippy::too_many_lines)]
    fn check_override_doc(&self, content: &str) -> Vec<Diagnostic> {
        let mut d: Vec<Diagnostic> = Vec::new();
        let err = |d: &mut Vec<Diagnostic>, m: String| {
            d.push(Diagnostic {
                severity: Severity::Error,
                message: m,
                line: None,
            });
        };
        if py_strip(content).is_empty() {
            return vec![Diagnostic {
                severity: Severity::Info,
                message: "empty override: the repo compose applies unchanged".into(),
                line: None,
            }];
        }
        let doc = match safe_load(content) {
            Ok(doc) => doc,
            Err(e) => {
                return vec![Diagnostic {
                    severity: Severity::Error,
                    // Python: f"YAML: {getattr(e, 'problem', e)}" (the real `problem` attribute, not str(e))
                    message: format!("YAML: {}", e.problem_or_str()),
                    line: e
                        .problem_mark()
                        .map(|m| i64::try_from(m.line).unwrap_or(0) + 1),
                }];
            }
        };
        let only = || {
            vec![Diagnostic {
                severity: Severity::Error,
                message: "only a top-level `services:` mapping is allowed".into(),
                line: None,
            }]
        };
        let PyValue::Dict(top) = &doc else {
            return only();
        };
        if top
            .iter()
            .any(|(k, _)| !matches!(k, PyValue::Str(s) if s == "services"))
        {
            return only();
        }
        let Some(PyValue::Dict(services)) = doc.get("services") else {
            return only();
        };
        let base = self.base_facts();
        let (wsvc, fsvc) = (&self.names.svc_worker, &self.names.svc_frontend);
        let empty = (Vec::new(), HashSet::new());
        for (svc_v, body) in services {
            let svc = svc_v.py_str();
            let is_ours = matches!(svc_v, PyValue::Str(s) if s == wsvc || s == fsvc);
            if !is_ours {
                err(
                    &mut d,
                    format!(
                        "service {} is not allowed (only {wsvc}, {fsvc})",
                        svc_v.repr()
                    ),
                );
                continue;
            }
            let PyValue::Dict(bm) = body else {
                err(
                    &mut d,
                    format!("{svc}: only `command` and `environment` may be overridden"),
                );
                continue;
            };
            if bm
                .iter()
                .any(|(k, _)| !matches!(k, PyValue::Str(s) if s == "command" || s == "environment"))
            {
                err(
                    &mut d,
                    format!("{svc}: only `command` and `environment` may be overridden"),
                );
                continue;
            }
            let cmd = body.get("command");
            if let Some(cmd) = cmd.filter(|c| !matches!(c, PyValue::None)) {
                let toks: Vec<String> = match cmd {
                    PyValue::List(a) if a.iter().all(|t| matches!(t, PyValue::Str(_))) => {
                        a.iter().map(PyValue::py_str).collect()
                    }
                    _ => {
                        err(
                            &mut d,
                            format!(
                                "{svc}.command must be a list of strings (a folded string is re-split by compose)"
                            ),
                        );
                        continue;
                    }
                };
                let want: Vec<String> = vec![
                    "python3".into(),
                    "-m".into(),
                    if svc == *wsvc {
                        "dynamo.vllm"
                    } else {
                        "dynamo.frontend"
                    }
                    .into(),
                ];
                if toks.iter().take(3).cloned().collect::<Vec<_>>() != want {
                    err(
                        &mut d,
                        format!("{svc}.command must start with {}", want.join(" ")),
                    );
                    continue;
                }
                let allowed = if svc == *wsvc {
                    worker_flags()
                } else {
                    frontend_flags()
                };
                let allowed_norm: HashSet<String> = allowed.iter().map(|a| norm_flag(a)).collect();
                let (b_tokens, _b_env) = base.get(&svc).unwrap_or(&empty);
                let b_dollar: HashSet<&String> =
                    b_tokens.iter().filter(|t| t.contains('$')).collect();
                let (b_flags, _) = parse_flags(b_tokens.get(3..).unwrap_or(&[]));
                let (c_flags, stray) = parse_flags(&toks[3..]);
                for tok in &stray {
                    err(
                        &mut d,
                        format!(
                            "{svc}: positional token {} is not allowed (every value must follow a flag)",
                            str_repr(tok)
                        ),
                    );
                }
                let mut seen: HashSet<String> = HashSet::new();
                for (flag, vals, raw) in &c_flags {
                    if !allowed.contains(raw) && !allowed_norm.contains(flag) {
                        err(
                            &mut d,
                            format!("{svc}: flag {raw} is not on the console allowlist"),
                        );
                    }
                    let key = dup_key(flag);
                    if seen.contains(&key) {
                        err(
                            &mut d,
                            format!(
                                "{svc}: flag {flag} appears more than once (argparse takes the LAST occurrence, so a \
                                 duplicate can override a pinned value); repeat nothing"
                            ),
                        );
                    }
                    seen.insert(key);
                    let base_vals = b_flags
                        .iter()
                        .find(|(bf, _, _)| bf == flag)
                        .map(|(_, bv, _)| bv);
                    if flag == "--speculative-config" {
                        if !spec_ok(vals) {
                            err(
                                &mut d,
                                format!(
                                    "{svc}: --speculative-config must be exactly \
                                     {{\"method\":\"mtp\",\"num_speculative_tokens\":<int 1..8>}}"
                                ),
                            );
                        }
                        continue;
                    }
                    if flag == "--model" || flag == "--served-model-name" {
                        if base_vals != Some(vals) {
                            err(
                                &mut d,
                                format!(
                                    "{svc}: {flag} must equal the base compose's value (it is not tunable)"
                                ),
                            );
                        }
                        continue;
                    }
                    for v in vals {
                        if (v.starts_with('{')
                            || v.starts_with('[')
                            || v.contains('/')
                            || v.contains('\\'))
                            && !v.contains('$')
                            && base_vals != Some(vals)
                        {
                            err(
                                &mut d,
                                format!(
                                    "{svc}: {flag} value {} is a path/JSON and must equal the base compose's value",
                                    str_repr(v)
                                ),
                            );
                        }
                    }
                }
                for tok in &toks[3..] {
                    if tok.contains('$') && !b_dollar.contains(tok) {
                        err(
                            &mut d,
                            format!(
                                "{svc}: `$` in {} is refused (compose would interpolate host env/.env into argv); \
                                 only `${{VAR:-x}}` tokens byte-identical to the base compose may stay",
                                str_repr(tok)
                            ),
                        );
                    }
                    if OVERRIDE_SECRET.is_match(tok) {
                        err(
                            &mut d,
                            format!("{svc}: a value looks like a secret; secrets never go in argv"),
                        );
                    }
                }
                if svc == *wsvc && !toks.iter().any(|t| t == STRUCTURAL) {
                    d.push(Diagnostic {
                        severity: Severity::Warn,
                        message: "--dyn-enable-structural-tag is absent: tool_choice=\"required\" will not produce tool \
                                  calls"
                            .into(),
                        line: None,
                    });
                }
                if svc == *fsvc
                    && let Some(i) = toks.iter().position(|t| t == "--router-mode")
                    && toks.get(i + 1).map(String::as_str) == Some("kv")
                {
                    d.push(Diagnostic {
                        severity: Severity::Warn,
                        message: "router-mode kv needs >1 worker and NATS".into(),
                        line: None,
                    });
                }
            }
            if let Some(env) = body
                .get("environment")
                .filter(|e| !matches!(e, PyValue::None))
            {
                let PyValue::Dict(env) = env else {
                    err(&mut d, format!("{svc}.environment must be a mapping"));
                    continue;
                };
                let b_env = base.get(&svc).map(|x| &x.1);
                for (k, val) in env {
                    let ks = k.py_str();
                    let known =
                        matches!(k, PyValue::Str(s) if b_env.is_some_and(|e| e.contains(s)));
                    let vs = val.py_str();
                    if !known {
                        err(
                            &mut d,
                            format!(
                                "{svc}: environment {ks} is not defined in the base compose for this service"
                            ),
                        );
                    } else if vs.contains('$') {
                        err(
                            &mut d,
                            format!(
                                "{svc}: environment {ks} contains `$` (interpolation of host env/.env is refused)"
                            ),
                        );
                    } else if OVERRIDE_SECRET.is_match(&vs) {
                        err(
                            &mut d,
                            format!("{svc}: environment {ks} looks like a secret"),
                        );
                    }
                }
            }
        }
        d
    }

    /// Values saved in the override header (empty when no override).
    #[must_use]
    pub fn current_values(&self) -> Map<String, Value> {
        std::fs::read_to_string(self.override_path())
            .map(|t| parse_values_header(&t))
            .unwrap_or_default()
    }

    fn base_commands(&self) -> Result<(Vec<String>, Vec<String>), AdapterError> {
        let Some(base) = self.compose_path() else {
            return Err(param_error(
                "repo compose path is not configured (ctx.paths['dynamo_compose'])".into(),
            ));
        };
        let text = std::fs::read_to_string(&base).map_err(|e| {
            let class = match e.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                std::io::ErrorKind::IsADirectory => "IsADirectoryError",
                _ => "OSError",
            };
            param_error(format!("cannot read the repo compose: {class}"))
        })?;
        load_base_commands(&text, &self.names.svc_worker, &self.names.svc_frontend).map_err(|e| {
            match &e {
                AdapterError::Failed { class, .. }
                    if matches!(
                        class.as_str(),
                        "ScannerError" | "ParserError" | "ComposerError" | "ConstructorError"
                    ) =>
                {
                    param_error(format!("cannot read the repo compose: {class}"))
                }
                _ => e,
            }
        })
    }

    /// Write the console override (0600 file in a 0700 dir, atomic).
    pub fn write_override(&self, text: &str) -> Result<(), AdapterError> {
        let p = self.override_path();
        if let Some(d) = p.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(d)
                .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        }
        atomic_write(&p, text.as_bytes(), 0o600)
            .map_err(|e| AdapterError::failed("OSError", e.to_string()))
    }

    fn restore_override(&self, previous: Option<&str>) {
        match previous {
            None => {
                let _ = std::fs::remove_file(self.override_path());
            }
            Some(t) => {
                let _ = self.write_override(t);
            }
        }
    }

    async fn wait_healthy(
        &self,
        prev_started: Option<&PyJson>,
    ) -> Result<(bool, String), AdapterError> {
        let deadline = Instant::now() + Duration::from_secs_f64(self.health_timeout.max(0.0));
        loop {
            let insp = self
                .inspect(std::slice::from_ref(&self.names.worker))
                .await?;
            let wk = insp.get(&self.names.worker);
            let last = if wk.is_none() || container_state(wk) != "running" {
                format!("worker {}", container_state(wk))
            } else if prev_started.is_some() && st_get(wk, "StartedAt") == prev_started {
                "worker not recreated yet".to_owned()
            } else {
                let doc = self.get_json("/v1/models").await;
                let data = jd(doc.as_ref()).and_then(|m| m.get("data"));
                if let Some(PyJson::List(models)) = data.filter(|x| pj_truthy(Some(x))) {
                    let id = match models.first() {
                        Some(PyJson::Dict(m)) => pj_str(m.get("id")),
                        _ => "None".into(),
                    };
                    return Ok((true, format!("serving {id}")));
                }
                "worker running, /v1/models empty (loading)".to_owned()
            };
            if Instant::now() >= deadline {
                return Ok((
                    false,
                    format!("timed out after {:.0}s: {last}", self.health_timeout),
                ));
            }
            tokio::time::sleep(Duration::from_secs_f64(self.poll_interval.max(0.0))).await;
        }
    }

    async fn recreate_worker_and_check(&self, why: &str) -> Result<ApplyResult, AdapterError> {
        let cf = self.compose_files(None).await?;
        if let Some(refused) = self.refusal(&cf, "Applying the console override") {
            return Ok(ApplyResult::new(false, format!("{why}; {refused}")));
        }
        let prev = self
            .inspect(std::slice::from_ref(&self.names.worker))
            .await?;
        let prev_started = st_get(prev.get(&self.names.worker), "StartedAt").cloned();
        let a = match self.compose_argv(
            &cf,
            &["up", "-d", &self.names.svc_frontend, &self.names.svc_worker],
            false,
        ) {
            Ok(a) => a,
            Err(e) => return Ok(ApplyResult::new(false, e.to_string())),
        };
        let res = self.ctx.runner.run(&a, RunOpts::timeout(600.0)).await?;
        if res.returncode != 0 {
            let mut r = ApplyResult::new(
                false,
                format!(
                    "{why}; compose up failed: {}",
                    tail(&redact(&either_strip(&res)), 400)
                ),
            );
            r.post_check = PostCheck::Unhealthy;
            return Ok(r);
        }
        let (ok, detail) = self.wait_healthy(prev_started.as_ref()).await?;
        let mut r = ApplyResult::new(ok, format!("{why}; {detail}"));
        r.restarted = true;
        r.post_check = if ok {
            PostCheck::Healthy
        } else {
            PostCheck::Unhealthy
        };
        Ok(r)
    }

    /// Running containers (other than Dynamo's) pinned to ANY of the worker's GPUs -> (sharers, error).
    async fn sharers(
        &self,
        cf: &ComposeFiles,
    ) -> Result<(Vec<Sharer>, Option<String>), AdapterError> {
        let wk_insp = self
            .inspect(std::slice::from_ref(&self.names.worker))
            .await?;
        let uuids = self
            .worker_uuids(cf, wk_insp.get(&self.names.worker))
            .await?;
        self.state().card_uuids.clone_from(&uuids);
        if uuids.is_empty() {
            return Ok((
                Vec::new(),
                Some("cannot determine the worker's GPU UUID(s) from the compose files in use or the container".into()),
            ));
        }
        let ps = self
            .ctx
            .runner
            .run(
                &argv(&["docker", "ps", "--format", "{{.Names}}"]),
                RunOpts::timeout(15.0),
            )
            .await?;
        let own = self.names.all();
        let names: Vec<String> = py_split_ws(&ps.stdout)
            .into_iter()
            .filter(|n| NAME_RE.is_match(n) && !own.iter().any(|o| o == n))
            .map(str::to_owned)
            .collect();
        if names.is_empty() {
            return Ok((Vec::new(), None));
        }
        let found = self.inspect(&names).await?;
        let mut out: Vec<Sharer> = Vec::new();
        for (n, d) in &found {
            if container_state(Some(d)) != "running" {
                continue;
            }
            for req in device_requests(Some(d)) {
                if uuids.iter().any(|u| req_has(req, u)) || req_all(req) {
                    let lb = jd(jpath(Some(d), &["Config", "Labels"]));
                    let l = |k: &str| {
                        lb.and_then(|l| l.get(k))
                            .map_or_else(String::new, |v| pj_str(Some(v)))
                    };
                    let img = jpath(Some(d), &["Config", "Image"])
                        .map_or_else(String::new, |v| pj_str(Some(v)));
                    out.push(Sharer {
                        name: n.clone(),
                        image: img,
                        project: l("com.docker.compose.project"),
                        service: l("com.docker.compose.service"),
                        workdir: l("com.docker.compose.project.working_dir"),
                        config: l("com.docker.compose.project.config_files"),
                        all_gpus: req_all(req),
                        kind: self.policy.kind(n),
                    });
                    break;
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((out, None))
    }

    /// (exclusive that may NOT be stopped here, the stoppable 27B if exclusive, unknown, coexisting, ignored).
    fn split(sharers: &[Sharer]) -> [Vec<Sharer>; 5] {
        let excl: Vec<&Sharer> = sharers.iter().filter(|s| s.kind == "exclusive").collect();
        let of = |k: &str| {
            sharers
                .iter()
                .filter(|s| s.kind == k)
                .cloned()
                .collect::<Vec<_>>()
        };
        [
            excl.iter()
                .filter(|s| s.name != STOPPABLE_ENGINE)
                .map(|s| (*s).clone())
                .collect(),
            excl.iter()
                .filter(|s| s.name == STOPPABLE_ENGINE)
                .map(|s| (*s).clone())
                .collect(),
            of("unknown"),
            of("coexisting"),
            of("ignored"),
        ]
    }

    async fn preflight(&self, cf: &ComposeFiles) -> Result<ActionResult, AdapterError> {
        let pol = &self.policy;
        let (sharers, err) = self.sharers(cf).await?;
        if let Some(err) = err {
            return Ok(ActionResult::new(false, err));
        }
        let [blocking, stoppable, unknown, coexisting, ignored] = Self::split(&sharers);
        let cards = self.state().card_uuids.join(", ");
        let overrides = !cf.overrides().is_empty();
        let mut lines = vec![format!(
            "Dynamo worker GPU(s): {cards} (from the compose files in use: {}{}). up/down of Dynamo touches no other \
             container and never calls the model-switch script.",
            cf.names(),
            cf.error.as_ref().map_or_else(String::new, |e| format!(
                "; refused for lifecycle actions: {e}"
            ))
        )];
        let switch = if overrides {
            format!(
                "Stop them first (they are exclusive with this stack: {})",
                cf.names()
            )
        } else {
            format!(
                "Switch to Dynamo with `{}` (it stops them in the right order)",
                pol.start
            )
        };
        let desc = |v: &[Sharer]| v.iter().map(describe).collect::<Vec<_>>().join("; ");
        if !blocking.is_empty() {
            lines.push(format!(
                "Exclusive engines running on the card: {}. `up` is refused while they run. {switch}; the console never \
                 stops them.",
                desc(&blocking)
            ));
        }
        if !stoppable.is_empty() {
            lines.push(format!(
                "{STOPPABLE_ENGINE} (exclusive) is running on the card. `up` is refused until it stops; \
                 stop_sharing_engine=true stops only {STOPPABLE_ENGINE}."
            ));
        }
        if !unknown.is_empty() {
            lines.push(format!(
                "Containers on the card the console does not know: {}. `up` is refused while they run; find out what \
                 they are first (the console never stops them). If one fits beside the worker, add it to the \
                 dynamo_coexisting_engines setting.",
                desc(&unknown)
            ));
        }
        if !coexisting.is_empty() {
            lines.push(format!(
                "Allowed beside the worker (coexisting): {}.",
                desc(&coexisting)
            ));
        }
        if !ignored.is_empty() {
            lines.push(format!(
                "Ignored (monitoring): {}.",
                ignored
                    .iter()
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if sharers.is_empty() {
            lines.push("No other container is running on the Dynamo card.".into());
        }
        lines.push(format!(
            "While Dynamo holds the card these are dark ({}). To switch to another engine: `{}`.",
            pol.dark_text(),
            pol.restore
        ));
        let mut probs: Vec<Problem> = blocking
            .iter()
            .map(|s| {
                Problem::new(
                    Severity::Warn,
                    "dynamo.card.shared",
                    describe(s),
                    Some(if overrides {
                        format!("stop {} first, then run the console action `up`", s.name)
                    } else {
                        pol.start.clone()
                    }),
                )
            })
            .collect();
        probs.extend(stoppable.iter().map(|s| {
            Problem::new(
                Severity::Warn,
                "dynamo.card.shared",
                describe(s),
                Some("run `up` with stop_sharing_engine=true".into()),
            )
        }));
        probs.extend(unknown.iter().map(|s| {
            Problem::new(
                Severity::Warn,
                "dynamo.card.unknown",
                format!(
                    "{} is on the worker's card and is not in the exclusive, coexisting or ignored lists",
                    describe(s)
                ),
                Some(format!(
                    "docker inspect --format '{{{{.Config.Image}}}}' {}   # identify it before stopping anything",
                    s.name
                )),
            )
        }));
        lines.extend(self.machine_lines(&sharers).await?);
        let mut r = ActionResult::new(true, lines.join("\n"));
        r.problems = probs;
        Ok(r)
    }

    /// Machine-readable tail of the preflight output (the UI parses these keys).
    async fn machine_lines(&self, sharers: &[Sharer]) -> Result<Vec<String>, AdapterError> {
        let excl = self.policy.exclusive_names();
        let mut names: IndexMap<String, &'static str> = sharers
            .iter()
            .filter(|s| s.kind == "exclusive" || s.kind == "unknown")
            .map(|s| (s.name.clone(), "running"))
            .collect();
        let mut want = excl.clone();
        want.push(self.names.worker.clone());
        let cur = self.inspect(&want).await?;
        let card = self.state().card_uuids.clone();
        for n in &excl {
            if let Some(d) = cur.get(n)
                && !names.contains_key(n)
                && !card.is_empty()
                && pinned(d, &card)
            {
                names.insert(
                    n.clone(),
                    if container_state(Some(d)) == "running" {
                        "running"
                    } else {
                        "stopped"
                    },
                );
            }
        }
        let mut sorted: Vec<(String, &str)> = names.into_iter().collect();
        sorted.sort();
        let mut out: Vec<String> = sorted
            .iter()
            .map(|(n, st)| format!("shares_card: {n} ({st})"))
            .collect();
        if out.is_empty() {
            out.push("shares_card: none".into());
        }
        let stop = sharers
            .iter()
            .any(|s| s.name == STOPPABLE_ENGINE && s.kind == "exclusive");
        out.push(if stop {
            format!("stoppable: {STOPPABLE_ENGINE} (running)")
        } else {
            "stoppable: none".into()
        });
        let excl_running = excl
            .iter()
            .any(|n| container_state(cur.get(n)) == "running");
        let dark = cur.contains_key(&self.names.worker)
            && container_state(cur.get(&self.names.worker)) == "running"
            && !excl_running;
        out.push(if dark && !self.policy.dark.is_empty() {
            format!("router_aliases_dark: {}", self.policy.dark.join(", "))
        } else {
            "router_aliases_dark: none".into()
        });
        Ok(out)
    }

    async fn up_mock(&self, cf: &ComposeFiles) -> Result<ActionResult, AdapterError> {
        if !cf.overrides().is_empty() {
            let mut r = ActionResult::new(
                false,
                format!(
                    "refused: the stack runs from {}; the CPU mocker belongs to the base compose",
                    cf.names()
                ),
            );
            r.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.mock.override_in_use",
                format!("the live Dynamo stack runs from {}", cf.names()),
                Some("run the console action `down` first".into()),
            )];
            return Ok(r);
        }
        let cur = self
            .inspect(std::slice::from_ref(&self.names.worker))
            .await?;
        if cur.contains_key(&self.names.worker)
            && container_state(cur.get(&self.names.worker)) == "running"
        {
            let mut r = ActionResult::new(
                false,
                "refused: the real worker is running (same model name as the mocker)",
            );
            r.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.mock.worker_running",
                "stop the worker first",
                Some("run the console action `down`".into()),
            )];
            return Ok(r);
        }
        let a = self.compose_argv(
            cf,
            &["up", "-d", &self.names.svc_frontend, &self.names.svc_mocker],
            true,
        )?;
        let r = self.ctx.runner.run(&a, RunOpts::timeout(300.0)).await?;
        Ok(ActionResult::new(r.returncode == 0, out_tail(&r, 2000)))
    }

    #[allow(clippy::too_many_lines)]
    async fn up_or_restart(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
        cf: &ComposeFiles,
    ) -> Result<ActionResult, AdapterError> {
        let pol = &self.policy;
        let cur = self
            .inspect(&[self.names.worker.clone(), self.names.mocker.clone()])
            .await?;
        let worker_running = cur.contains_key(&self.names.worker)
            && container_state(cur.get(&self.names.worker)) == "running";
        let mut out: Vec<String> = Vec::new();
        if cur.contains_key(&self.names.mocker)
            && container_state(cur.get(&self.names.mocker)) == "running"
        {
            let mut r = ActionResult::new(
                false,
                "refused: the CPU mocker is running (same model name)",
            );
            r.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.up.mocker_running",
                "the mocker must not run alongside the real worker",
                Some("run the console action `down`, then `up`".into()),
            )];
            return Ok(r);
        }
        let overrides = !cf.overrides().is_empty();
        if !worker_running {
            let (sharers, err) = self.sharers(cf).await?;
            if let Some(err) = err {
                let mut r = ActionResult::new(false, err.clone());
                r.problems = vec![Problem::new(
                    Severity::Error,
                    "dynamo.preflight.failed",
                    err,
                    None,
                )];
                return Ok(r);
            }
            let [blocking, stoppable, unknown, _co, _ig] = Self::split(&sharers);
            if !blocking.is_empty() {
                let why = blocking.iter().map(describe).collect::<Vec<_>>().join("; ");
                let fix = if overrides {
                    format!(
                        "stop {} first (exclusive with this stack), then run the console action `up`",
                        blocking
                            .iter()
                            .map(|s| s.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    pol.start.clone()
                };
                let mut r = ActionResult::new(
                    false,
                    format!(
                        "refused: exclusive engine(s) on the worker's card: {why}. The console never stops them; {}{fix}",
                        if overrides {
                            ""
                        } else {
                            "switch to Dynamo with: "
                        }
                    ),
                );
                r.problems = vec![Problem::new(
                    Severity::Error,
                    "dynamo.up.exclusive_engine",
                    format!("card held by {why}"),
                    Some(fix),
                )];
                return Ok(r);
            }
            if !unknown.is_empty() {
                let why = unknown.iter().map(describe).collect::<Vec<_>>().join("; ");
                let first = &unknown[0].name;
                let mut r = ActionResult::new(
                    false,
                    format!(
                        "refused: the worker's card is in use by {why}, which the console does not know (not in the \
                         exclusive, coexisting or ignored lists). It is never stopped by the console; find out what it \
                         is first. If it fits beside the worker, add it to the dynamo_coexisting_engines setting."
                    ),
                );
                r.problems = vec![Problem::new(
                    Severity::Error,
                    "dynamo.up.card_in_use",
                    format!("card in use by {why} (unknown to the console)"),
                    Some(format!(
                        "docker inspect --format '{{{{.Config.Image}}}}' {first}   # identify it before stopping anything"
                    )),
                )];
                return Ok(r);
            }
            if !stoppable.is_empty() {
                if params.get("stop_sharing_engine") != Some(&Value::Bool(true)) {
                    let why = stoppable
                        .iter()
                        .map(describe)
                        .collect::<Vec<_>>()
                        .join("; ");
                    let mut r = ActionResult::new(
                        false,
                        format!(
                            "refused: the worker's card is in use by {why}. Re-run with stop_sharing_engine=true to stop \
                             exactly {STOPPABLE_ENGINE}."
                        ),
                    );
                    r.problems = vec![Problem::new(
                        Severity::Error,
                        "dynamo.up.card_in_use",
                        format!("card in use by {why}"),
                        Some("run `up` with stop_sharing_engine=true".into()),
                    )];
                    return Ok(r);
                }
                let stopped = self.stop_stoppable(&stoppable[0]).await?;
                out.push(stopped.output.clone());
                if !stopped.ok {
                    let mut r = ActionResult::new(false, out.join("\n"));
                    r.problems = stopped.problems;
                    return Ok(r);
                }
            }
        }
        let mut t: Vec<&str> = vec!["up", "-d"];
        if action_id == "restart" {
            t.push("--force-recreate");
        }
        t.push(&self.names.svc_frontend);
        t.push(&self.names.svc_worker);
        let prev_started = st_get(cur.get(&self.names.worker), "StartedAt").cloned();
        let a = self.compose_argv(cf, &t, false)?;
        let r = self.ctx.runner.run(&a, RunOpts::timeout(600.0)).await?;
        out.push(out_tail(&r, 1500));
        if r.returncode != 0 {
            let mut res = ActionResult::new(false, out.join("\n"));
            res.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.up.failed",
                "docker compose up failed",
                None,
            )];
            return Ok(res);
        }
        let (ok, detail) = self
            .wait_healthy(if action_id == "restart" {
                prev_started.as_ref()
            } else {
                None
            })
            .await?;
        out.push(detail.clone());
        let mut res = ActionResult::new(ok, out.join("\n"));
        if !ok {
            res.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.up.unhealthy",
                detail,
                Some(format!("docker logs --tail 80 {}", self.names.worker)),
            )];
        }
        Ok(res)
    }

    /// Stops exactly STOPPABLE_ENGINE: name + compose labels verified, a compose `down` in ITS project, volumes kept.
    async fn stop_stoppable(&self, s: &Sharer) -> Result<ActionResult, AdapterError> {
        let cfg = py_strip(s.config.split(',').next().unwrap_or("")).to_owned();
        let workdir = s.workdir.clone();
        let parent = Path::new(&cfg)
            .parent()
            .map_or_else(String::new, |p| p.display().to_string());
        let bad = s.name != STOPPABLE_ENGINE
            || s.project != STOPPABLE_PROJECT
            || s.service != STOPPABLE_SERVICE
            || !cfg.starts_with('/')
            || !COMPOSE_NAMES.contains(&file_name(Path::new(&cfg)).as_str())
            || parent != workdir;
        if bad {
            let mut r = ActionResult::new(
                false,
                "refused: container labels do not match the expected engine",
            );
            r.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.stop.unverified",
                format!("{} failed name/label verification", s.name),
                Some(format!("stop it by hand: docker stop {}", s.name)),
            )];
            return Ok(r);
        }
        let r = self
            .ctx
            .runner
            .run(
                &argv(&["docker", "compose", "-f", &cfg, "down"]),
                RunOpts::timeout(180.0).cwd(PathBuf::from(&workdir)),
            )
            .await?;
        let again = self.inspect(&[STOPPABLE_ENGINE.to_owned()]).await?;
        let ok = r.returncode == 0 && container_state(again.get(STOPPABLE_ENGINE)) != "running";
        let mut res = ActionResult::new(
            ok,
            format!("stopped {STOPPABLE_ENGINE}: {}", out_tail(&r, 600)),
        );
        if !ok {
            res.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.stop.failed",
                format!("{STOPPABLE_ENGINE} is still running"),
                None,
            )];
        }
        Ok(res)
    }

    async fn dispatch(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        if self.compose_path().is_none() {
            let mut r = ActionResult::new(false, "");
            r.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.compose.unset",
                "repo compose path is not configured",
                Some("set ctx.paths['dynamo_compose']".into()),
            )];
            return Ok(r);
        }
        let cf = self.compose_files(None).await?;
        if action_id == "preflight" {
            return self.preflight(&cf).await;
        }
        if let Some(e) = &cf.error {
            let mut r = ActionResult::new(false, format!("refused: {e}"));
            r.problems = vec![Problem::new(
                Severity::Error,
                "dynamo.compose.untrusted",
                format!("the compose files of the running Dynamo stack cannot be trusted: {e}"),
                Some(format!(
                    "docker inspect --format '{{{{index .Config.Labels \"{LABEL_FILES}\"}}}}' {}",
                    self.names.worker
                )),
            )];
            return Ok(r);
        }
        match action_id {
            "up_mock" => self.up_mock(&cf).await,
            "down" => {
                let a = self.compose_argv(&cf, &["down"], true)?;
                let r = self.ctx.runner.run(&a, RunOpts::timeout(180.0)).await?;
                Ok(ActionResult::new(r.returncode == 0, out_tail(&r, 2000)))
            }
            _ => self.up_or_restart(action_id, params, &cf).await,
        }
    }

    fn playground_models(&self) -> Vec<String> {
        let mut models = self.state().models.clone();
        if models.is_empty()
            && let Some(base) = self.compose_path()
            && let Ok(text) = std::fs::read_to_string(&base)
            && let Ok((w, _)) =
                load_base_commands(&text, &self.names.svc_worker, &self.names.svc_frontend)
            && let Some(m) = find(&split_command(&w).1, "--served-model-name")
            && let Some(first) = m.first()
        {
            models = vec![first.clone()];
        }
        if models.is_empty() {
            models = vec![DEFAULT_MODEL.to_owned()];
        }
        models
    }
}

fn cmd_of(d: Option<&PyJson>) -> Vec<String> {
    match jpath(d, &["Config", "Cmd"]) {
        Some(PyJson::List(a)) => a.iter().map(|c| pj_str(Some(c))).collect(),
        _ => Vec::new(),
    }
}

#[async_trait]
impl SystemAdapter for DynamoAdapter {
    fn id(&self) -> SystemId {
        SystemId::Dynamo
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let now = now_ts();
        match self.status_inner(now).await {
            Ok(s) => Ok(s),
            Err(e) => {
                let msg = match &e {
                    AdapterError::Failed { message, .. } => message.clone(),
                    other => other.to_string(),
                };
                Ok(SystemStatus {
                    system: SystemId::Dynamo,
                    health: Health::Unknown,
                    version: None,
                    summary: format!("status probe failed: {}", e.py_class()),
                    endpoints: Vec::new(),
                    components: Vec::new(),
                    problems: vec![Problem::new(
                        Severity::Error,
                        "dynamo.probe.failed",
                        char_prefix(&redact(&msg), 300),
                        Some("check the docker runner/socket".into()),
                    )],
                    checked_at: now,
                })
            }
        }
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let ts = now_ts();
        let fe = match self
            .ctx
            .http
            .get(&format!("{}/metrics", self.base_url()), 5.0)
            .await
        {
            Ok(r) if (200..300).contains(&r.status) => parse_prom(&r.text()),
            Ok(_) => {
                return Ok(MetricsSnapshot {
                    system: SystemId::Dynamo,
                    ts,
                    available: false,
                    points: Vec::new(),
                    error: Some("frontend /metrics unreachable: HTTPStatusError".into()),
                });
            }
            Err(e) => {
                return Ok(MetricsSnapshot {
                    system: SystemId::Dynamo,
                    ts,
                    available: false,
                    points: Vec::new(),
                    error: Some(format!("frontend /metrics unreachable: {}", e.class)),
                });
            }
        };
        let mut pts = vec![{
            let mut p = MetricPoint::new("dyn.up", 1.0);
            p.unit = "1".into();
            p
        }];
        let add = |pts: &mut Vec<MetricPoint>,
                   key: &str,
                   val: Option<f64>,
                   unit: &str,
                   labels: &[(&str, &str)]| {
            if let Some(v) = val {
                pts.push(MetricPoint {
                    key: key.into(),
                    value: v,
                    unit: unit.into(),
                    labels: labels
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                });
            }
        };
        let ready: Vec<f64> = fe
            .iter()
            .filter(|(n, _, _)| n == "dynamo_frontend_model_ready")
            .map(|s| s.2)
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let models = if ready.is_empty() {
            None
        } else {
            Some(ready.iter().filter(|v| **v >= 1.0).count() as f64)
        };
        add(&mut pts, "dyn.models", models, "1", &[]);
        let mut by_status: IndexMap<String, f64> = IndexMap::new();
        for (n, lb, v) in &fe {
            if n == "dynamo_frontend_requests_total" {
                let st = lb
                    .get("status")
                    .cloned()
                    .unwrap_or_else(|| "unknown".into());
                *by_status.entry(st).or_insert(0.0) += v;
            }
        }
        let mut statuses: Vec<(String, f64)> = by_status.into_iter().collect();
        statuses.sort_by(|a, b| a.0.cmp(&b.0));
        for (st, v) in statuses {
            add(
                &mut pts,
                "dyn.requests_total",
                Some(v),
                "1",
                &[("status", &st)],
            );
        }
        add(
            &mut pts,
            "dyn.in_flight",
            sum(&fe, "dynamo_frontend_inflight_requests"),
            "1",
            &[],
        );
        for (qn, q) in [("p50", 0.5), ("p95", 0.95)] {
            add(
                &mut pts,
                "dyn.ttft_ms",
                time_ms(histogram_quantile(
                    &fe,
                    "dynamo_frontend_time_to_first_token_seconds",
                    q,
                )),
                "ms",
                &[("q", qn)],
            );
            add(
                &mut pts,
                "dyn.itl_ms",
                time_ms(histogram_quantile(
                    &fe,
                    "dynamo_frontend_inter_token_latency_seconds",
                    q,
                )),
                "ms",
                &[("q", qn)],
            );
        }
        add(
            &mut pts,
            "dyn.tokens",
            sum(&fe, "dynamo_frontend_input_sequence_tokens_sum"),
            "tok",
            &[("kind", "prompt")],
        );
        add(
            &mut pts,
            "dyn.tokens",
            sum(&fe, "dynamo_frontend_output_tokens_total"),
            "tok",
            &[("kind", "completion")],
        );
        add(
            &mut pts,
            "dyn.tokens",
            sum(&fe, "dynamo_frontend_cached_tokens_sum"),
            "tok",
            &[("kind", "cached")],
        );
        let insp = self
            .inspect(std::slice::from_ref(&self.names.worker))
            .await?;
        let mut worker_up = 0.0;
        if let Some(wk) = insp.get(&self.names.worker)
            && container_state(Some(wk)) == "running"
            && self.project_ok(wk, &self.names.svc_worker)
        {
            let (health, wm) = self.worker_scrape().await?;
            if let Some(h) = &health
                && pj_str(h.get("status")) == "ready"
            {
                worker_up = 1.0;
            }
            add(
                &mut pts,
                "dyn.kv_cache_usage",
                sum(&wm, "vllm:kv_cache_usage_perc"),
                "ratio",
                &[],
            );
            add(
                &mut pts,
                "dyn.requests_running",
                sum(&wm, "vllm:num_requests_running"),
                "1",
                &[],
            );
            add(
                &mut pts,
                "dyn.requests_waiting",
                sum(&wm, "vllm:num_requests_waiting"),
                "1",
                &[],
            );
            let drafted = sum(&wm, "vllm:spec_decode_num_draft_tokens_total");
            let accepted = sum(&wm, "vllm:spec_decode_num_accepted_tokens_total");
            if let (Some(d), Some(a)) = (drafted, accepted)
                && d != 0.0
            {
                add(&mut pts, "dyn.spec_accept_ratio", Some(a / d), "ratio", &[]);
            }
            let pq = sum(&wm, "vllm:prefix_cache_queries_total");
            let ph = sum(&wm, "vllm:prefix_cache_hits_total");
            if let (Some(q), Some(h)) = (pq, ph)
                && q != 0.0
            {
                add(
                    &mut pts,
                    "dyn.prefix_cache_hit_ratio",
                    Some(h / q),
                    "ratio",
                    &[],
                );
            }
        }
        add(&mut pts, "dyn.worker_up", Some(worker_up), "1", &[]);
        Ok(MetricsSnapshot {
            system: SystemId::Dynamo,
            ts,
            available: true,
            points: pts,
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let limit = limit.clamp(1, 500);
        let mut evs: Vec<SystemEvent> = Vec::new();
        for (name, comp) in self.names.log_sources() {
            evs.extend(
                self.logs(&name, comp, Some((limit * 3).min(1000)), None)
                    .await?,
            );
        }
        evs.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        let skip = evs.len().saturating_sub(limit);
        Ok(evs.into_iter().skip(skip).collect())
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        Box::pin(async_stream::stream! {
            let mut cursor = since_ts.unwrap_or_else(now_ts);
            let mut seen: IndexMap<(u64, String), ()> = IndexMap::new();
            loop {
                let mut batch: Vec<SystemEvent> = Vec::new();
                for (name, comp) in self.names.log_sources() {
                    match self.logs(&name, comp, Some(200), Some(cursor)).await {
                        Ok(v) => batch.extend(v),
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
                batch.sort_by(|a, b| a.ts.total_cmp(&b.ts));
                for ev in batch {
                    let key = (ev.ts.to_bits(), ev.summary.clone());
                    if seen.contains_key(&key) || ev.ts < cursor {
                        continue;
                    }
                    seen.insert(key, ());
                    while seen.len() > 2000 {
                        seen.shift_remove_index(0);
                    }
                    cursor = cursor.max(ev.ts);
                    yield Ok(ev);
                }
                tokio::time::sleep(Duration::from_secs_f64(self.follow_interval.max(0.0))).await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        Ok(self.specs())
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        match file_id {
            "compose" => self.compose_path().ok_or_else(|| {
                AdapterError::UnknownId(
                    "compose (ctx.paths['dynamo_compose'] not configured)".into(),
                )
            }),
            "override" => Ok(self.override_path()),
            other => Err(AdapterError::UnknownId(other.to_owned())),
        }
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let Some(spec) = self.specs().into_iter().find(|s| s.id == file_id) else {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        };
        let p = self.config_path(file_id)?;
        if p.symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(AdapterError::failed(
                "PermissionError",
                format!("{file_id}: refusing to read through a symlink"),
            ));
        }
        if !p.exists() {
            if file_id == "override" {
                return Ok(ConfigFile {
                    spec,
                    content: String::new(),
                    sha256: sha256_hex(b""),
                    mtime: 0.0,
                });
            }
            return Err(AdapterError::FileNotFound(p.display().to_string()));
        }
        let mut raw = Vec::new();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_CLOEXEC)
            .open(&p)
            .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        std::io::Read::read_to_end(
            &mut std::io::Read::take(f, MAX_CONFIG_BYTES as u64 + 1),
            &mut raw,
        )
        .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        if raw.len() > MAX_CONFIG_BYTES {
            return Err(AdapterError::Invalid(format!(
                "{file_id}: larger than {MAX_CONFIG_BYTES} bytes"
            )));
        }
        let mtime = std::fs::metadata(&p)
            .ok()
            .and_then(|m| mtime_f64(&m))
            .unwrap_or(0.0);
        Ok(ConfigFile {
            spec,
            content: String::from_utf8_lossy(&raw).into_owned(),
            sha256: sha256_hex(&raw),
            mtime,
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        if file_id != "override" {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        let mut diags = self.check_override_doc(content);
        let nonempty = !py_strip(content).is_empty();
        if nonempty {
            let cf = self.compose_files(None).await?;
            if let Some(why) = self.refusal(&cf, "Applying the console override") {
                diags.push(Diagnostic {
                    severity: Severity::Error,
                    message: why,
                    line: None,
                });
            }
        }
        let mut ok = !diags.iter().any(|x| x.severity == Severity::Error);
        let mut note = None;
        let mut authoritative = false;
        let base = self.compose_path();
        if ok
            && nonempty
            && let Some(base) = &base
        {
            let tmpdir = self.ctx.state_dir.join("dynamo").join("tmp");
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&tmpdir)
                .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
            let ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let tmp = tmpdir.join(format!("candidate-{}-{ns}.yml", std::process::id()));
            {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&tmp)
                    .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
                f.write_all(content.as_bytes())
                    .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
            }
            let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
            let a = vec![
                "docker".to_owned(),
                "compose".to_owned(),
                "--project-directory".to_owned(),
                dir.display().to_string(),
                "-f".to_owned(),
                base.display().to_string(),
                "-f".to_owned(),
                tmp.display().to_string(),
                "config".to_owned(),
                "-q".to_owned(),
            ];
            let res = self.ctx.runner.run(&a, RunOpts::timeout(30.0)).await;
            let _ = std::fs::remove_file(&tmp);
            let res = res?;
            if res.returncode == 0 {
                authoritative = true;
            } else {
                ok = false;
                diags.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!(
                        "docker compose config: {}",
                        char_prefix(&redact(&either_strip(&res)), 400)
                    ),
                    line: None,
                });
            }
        } else if ok && nonempty {
            note = Some("docker compose config was not run (repo compose path not configured); allowlist checks only".into());
        }
        Ok(ValidationResult {
            ok,
            validator: "allowlist + docker compose config -q".into(),
            authoritative,
            diagnostics: diags,
            note,
        })
    }

    async fn post_apply(&self, file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError> {
        if file_id != "override" {
            return Ok(ApplyResult::new(false, format!("{file_id} is read-only")));
        }
        if !restart {
            return Ok(ApplyResult::new(
                true,
                "override saved; takes effect on the next up/restart",
            ));
        }
        self.recreate_worker_and_check("override written").await
    }

    fn param_catalog(&self) -> Result<Vec<ParamSpec>, AdapterError> {
        Ok(catalog())
    }

    async fn preview_params(&self, values: &Map<String, Value>) -> Result<String, AdapterError> {
        let (w, f) = self.base_commands()?;
        let (text, _) = render_override(
            values,
            &w,
            Some(&f),
            &self.names.svc_worker,
            &self.names.svc_frontend,
        )?;
        Ok(text)
    }

    async fn apply_params(&self, values: &Map<String, Value>) -> Result<ApplyResult, AdapterError> {
        let text = match self.preview_params(values).await {
            Ok(t) => t,
            Err(AdapterError::Invalid(m)) => return Ok(ApplyResult::new(false, m)),
            Err(e) => return Err(e),
        };
        let cf = self.compose_files(None).await?;
        if let Some(why) = self.refusal(&cf, "Applying Params") {
            return Ok(ApplyResult::new(false, why)); // nothing written: the live stack is not the base stack
        }
        let previous = std::fs::read_to_string(self.override_path()).ok();
        self.write_override(&text)?;
        let a = match self.compose_argv(&cf, &["config", "-q"], false) {
            Ok(a) => a,
            Err(e) => {
                self.restore_override(previous.as_deref());
                return Ok(ApplyResult::new(false, e.to_string()));
            }
        };
        let chk = self.ctx.runner.run(&a, RunOpts::timeout(30.0)).await?;
        if chk.returncode != 0 {
            self.restore_override(previous.as_deref());
            return Ok(ApplyResult::new(
                false,
                format!(
                    "docker compose config rejected the override: {}",
                    char_prefix(&redact(&either_strip(&chk)), 400)
                ),
            ));
        }
        let sha = sha256_hex(text.as_bytes());
        let insp = self
            .inspect(std::slice::from_ref(&self.names.worker))
            .await?;
        if !insp.contains_key(&self.names.worker) {
            let mut r = ApplyResult::new(
                true,
                "override written (0600); no worker container exists, so nothing was recreated. It applies on the next \
                 `up`.",
            );
            r.new_sha256 = Some(sha);
            return Ok(r);
        }
        let mut res = self.recreate_worker_and_check("params applied").await?;
        if res.post_check == PostCheck::Unhealthy {
            self.restore_override(previous.as_deref());
            let back = self.recreate_worker_and_check("rollback").await?;
            let pc = match back.post_check {
                PostCheck::Healthy => "healthy",
                PostCheck::Unhealthy => "unhealthy",
                PostCheck::Skipped => "skipped",
            };
            let mut r = ApplyResult::new(
                false,
                format!(
                    "new override unhealthy ({}); previous override restored -> {pc}",
                    res.detail
                ),
            );
            r.new_sha256 = Some(sha);
            r.restarted = true;
            r.post_check = PostCheck::Unhealthy;
            r.rolled_back = true;
            return Ok(r);
        }
        res.new_sha256 = Some(sha);
        Ok(res)
    }

    fn param_warnings(
        &self,
        values: &Map<String, Value>,
    ) -> Option<Result<Vec<String>, AdapterError>> {
        let clean = match validate_values(values) {
            Ok(c) => c,
            Err(e) => return Some(Err(e)),
        };
        let mut w = param_warnings(&clean);
        let last = self.state().last_files.clone();
        if let Some(last) = last
            && let Some(why) = self.refusal(&last, "Applying these Params")
        {
            w.insert(0, why);
        }
        Some(Ok(w))
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let pol = &self.policy;
        let excl = if pol.exclusive.is_empty() {
            "none configured".to_owned()
        } else {
            pol.exclusive.join(", ")
        };
        let mut up = ActionSpec::new(
            "up",
            "Up (frontend + worker)",
            &format!(
                "docker compose up -d for the frontend and the vLLM worker (cold start ~4 min), with the compose files \
                 the existing Dynamo containers were started from (with none, the configured base compose). Refused \
                 while an exclusive engine runs on the worker's card ({excl}): switch to Dynamo with `{}` instead, \
                 which stops them in the right order. stop_sharing_engine=true stops only {STOPPABLE_ENGINE}, never \
                 another engine. A container on the card that the console does not know also refuses `up`. While \
                 Dynamo holds the card these are dark: {}.",
                pol.start,
                pol.dark_text()
            ),
            Danger::Disruptive,
        );
        let mut p = ParamSpec::new(
            "stop_sharing_engine",
            &format!("Stop {STOPPABLE_ENGINE} first"),
            &format!(
                "Stops only {STOPPABLE_ENGINE} (docker compose down in its own project, volumes kept: the same down the \
                 model-switch script does). Never any other engine: with another exclusive engine on the card `up` \
                 stays refused."
            ),
            ParamType::Bool,
        );
        p.default = Value::Bool(false);
        up.params = vec![p];
        Ok(vec![
            ActionSpec::new(
                "preflight",
                "Preflight (what shares the card)",
                "Lists the running containers pinned to any of the Dynamo worker's GPUs, sorted into exclusive (block \
                 `up`), coexisting (allowed beside the worker), ignored (monitoring) and unknown (block `up`, never \
                 stopped). Dynamo up/down touches no other container and never runs the model-switch script.",
                Danger::Safe,
            ),
            up,
            ActionSpec::new(
                "down",
                "Down (Dynamo only)",
                &format!(
                    "docker compose down for the Dynamo project (frontend, worker, mocker, and the services of the \
                     override files the stack runs from); volumes are kept. Frees the worker's card(s); touches no \
                     other container and starts no other engine (to switch to one: `{}`).",
                    pol.restore
                ),
                Danger::Disruptive,
            ),
            ActionSpec::new(
                "restart",
                "Restart (recreate)",
                "up -d --force-recreate of frontend and worker with the compose files in use, picking up the console \
                 override (base stack only).",
                Danger::Disruptive,
            ),
            ActionSpec::new(
                "up_mock",
                "Up mock (CPU, no VRAM)",
                "Starts the frontend and the CPU mocker (--profile mock) of the base compose to test plumbing. Refused \
                 while the real worker runs (same model name) and while the stack runs from override files.",
                Danger::Safe,
            ),
        ])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        if !self.actions()?.iter().any(|a| a.id == action_id) {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        let t0 = Instant::now();
        let mut res = self.dispatch(action_id, params).await?;
        res.duration_ms = ms(t0);
        Ok(res)
    }

    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        let models = self.playground_models();
        Ok(Some(PlaygroundSpec {
            wire: "openai_chat".into(),
            default_model: models[0].clone(),
            models,
            note: "Direct to the Dynamo frontend (no router, no Presidio masking, no auth). Model list refreshes on \
                   each status() call."
                .into(),
        }))
    }

    fn playground_url(&self) -> Option<String> {
        Some(format!("{}/v1/chat/completions", self.base_url()))
    }
}
