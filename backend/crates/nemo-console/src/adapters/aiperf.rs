//! AIPerf adapter: NVIDIA AIPerf (a load / latency benchmark CLI, not a service) as a managed system. OWNER: workstream B.
//! Port of `backend/src/nemo_console/adapters/aiperf.py`.
//!
//! Facts this adapter is built on (evidence in tests/fixtures/aiperf, captured live 2026-10-24):
//!   * `aiperf-local` is the lab wrapper (AIPerf 0.13.0); `--version` prints "0.13.0". The wrapper owns the target
//!     (router, loopback only), the router key (its own 0600 file) and the offline tokenizer; the console passes no
//!     env and never sees a key.
//!   * `aiperf-local profile ... --artifact-dir DIR` writes DIR/profile_export_aiperf.json (metric objects with
//!     p50/p99/avg) and, with `--export-level records`, DIR/profile_export.jsonl (per-request errors).
//!   * A run that completes no request exits 1 with an error panel and writes no summary.
//!   * profiles.yaml is this console's own schema: validated here, authoritative.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, RunOpts, SystemAdapter,
};
use crate::config::Settings;
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, PostCheck,
    Problem, Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::pyfmt::str_repr;
use crate::services::pyyaml::{self, NodeKind, PyValue as Yv, YamlError};
use crate::services::{plumbing as wsb, pystr};

/// `AiPerfAdapter.label` in Python.
pub const LABEL: &str = "AIPerf";
pub const LAUNCHER: &str = "aiperf-local";
/// = runner.AIPERF_PROFILE; the runner caps `aiperf-local profile` at this.
pub const PROFILE_TIMEOUT: f64 = 3600.0;
pub const PROFILE_MAX_BYTES: usize = 1_048_576;
pub const VERSION_TTL: f64 = 300.0;
pub const KEEP_RUNS: usize = 100;
pub const EVENT_KIND: &str = "aiperf.run";
pub const VERDICTS: [&str; 4] = ["succeeded", "failed", "error", "interrupted"];
pub const SUMMARY_FILE: &str = "profile_export_aiperf.json";
pub const RECORDS_FILE: &str = "profile_export.jsonl";
pub const MAX_RECORD_LINES: usize = 500;
/// The most bytes [`request_errors`] reads from profile_export.jsonl (PORTING D-27).
pub const MAX_EXPORT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PROFILES_BYTES: usize = 256 * 1024;
pub const MAX_PROFILES: usize = 32;
pub const PROFILES_ID: &str = "profiles";
pub const ENDPOINT_TYPES: [&str; 2] = ["chat", "completions"];
pub const LAB_INSTALLER: &str = "clients/aiperf/install.sh";
pub const LAB_MINT_KEY: &str = "clients/aiperf/mint-key.sh";
pub const PINNED_VERSION: &str = "0.13.0";

static RUN_ID_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A\d{8}T\d{6}-\d{3}-[a-z0-9][a-z0-9-]{0,39}\z").ok());
static PRESET_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9-]{0,39}\z").ok());
static MODEL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._:/@+\-]{0,127}\z").ok());
static ANSI_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").ok());
static BOX_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[│╭╮╰╯─┃┏┓┗┛━┡┩╇┳┻╋]+").ok());
static LOG_LINE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A\d\d:\d\d:\d\d\.\d+ (?:INFO|WARNING|NOTICE|DEBUG)\b").ok());
static KEEP_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?i)error|reason|failed|refused|missing|not answering|prefetch").ok()
});

fn is(re: &LazyLock<Option<Regex>>, s: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(s))
}

fn sub(re: &LazyLock<Option<Regex>>, s: &str, rep: &str) -> String {
    match re.as_ref() {
        Some(r) => r.replace_all(s, regex::NoExpand(rep)).into_owned(),
        None => s.to_owned(),
    }
}

/// profiles.yaml schema: integer key -> (min, max, required).
pub const INT_KEYS: [(&str, i128, i128, bool); 7] = [
    ("concurrency", 1, 64, true),
    ("requests", 1, 2000, true),
    ("isl", 1, 32768, true),
    ("osl", 1, 4096, true),
    ("warmup_requests", 0, 100, false),
    ("timeout_s", 1, 600, false),
    ("seed", 0, 2_147_483_647, false),
];

fn profile_keys() -> Vec<&'static str> {
    let mut k = vec!["description", "model", "endpoint_type", "streaming"];
    k.extend(INT_KEYS.iter().map(|(n, ..)| *n));
    k
}

/// One validated preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub model: String,
    pub endpoint_type: String,
    pub streaming: bool,
    pub concurrency: i64,
    pub requests: i64,
    pub isl: i64,
    pub osl: i64,
    pub warmup_requests: i64,
    pub timeout_s: i64,
    pub seed: i64,
    pub description: String,
}

/// (path...) -> 1-based line, for top-level keys, profile names and profile fields. Never fails.
fn line_map(text: &str) -> HashMap<Vec<String>, usize> {
    let mut out = HashMap::new();
    let Ok(doc) = pyyaml::compose(text) else {
        return out;
    };
    let Some(root) = doc.root.and_then(|r| doc.node(r)) else {
        return out;
    };
    let NodeKind::Map(items) = &root.kind else {
        return out;
    };
    for (k, v) in items {
        let (Some(kn), Some(vn)) = (doc.node(*k), doc.node(*v)) else {
            continue;
        };
        let NodeKind::Scalar(kv) = &kn.kind else {
            continue;
        };
        out.insert(vec![kv.clone()], kn.start.line + 1);
        if kv == "profiles"
            && let NodeKind::Map(profiles) = &vn.kind
        {
            for (pk, pv) in profiles {
                let (Some(pkn), Some(pvn)) = (doc.node(*pk), doc.node(*pv)) else {
                    continue;
                };
                let NodeKind::Scalar(pname) = &pkn.kind else {
                    continue;
                };
                out.insert(
                    vec!["profiles".to_owned(), pname.clone()],
                    pkn.start.line + 1,
                );
                if let NodeKind::Map(fields) = &pvn.kind {
                    for (fk, _) in fields {
                        if let Some(fkn) = doc.node(*fk)
                            && let NodeKind::Scalar(fname) = &fkn.kind
                        {
                            out.insert(
                                vec!["profiles".to_owned(), pname.clone(), fname.clone()],
                                fkn.start.line + 1,
                            );
                        }
                    }
                }
            }
        }
    }
    out
}

/// A 'did you mean' hint: the option that shares the longest common prefix (>= 3 chars) with a typo.
fn close(word: &str, options: &[&str]) -> String {
    let mut sorted: Vec<&str> = options.to_vec();
    sorted.sort_unstable();
    let (mut best, mut best_n) = ("", 0usize);
    for o in sorted {
        let n = word
            .chars()
            .zip(o.chars())
            .take_while(|(a, b)| a == b)
            .count();
        if n > best_n {
            best = o;
            best_n = n;
        }
    }
    if best_n >= 3 {
        format!(" (did you mean '{best}'?)")
    } else {
        String::new()
    }
}

fn diag(severity: Severity, message: impl Into<String>, line: Option<usize>) -> Diagnostic {
    Diagnostic {
        severity,
        message: message.into(),
        line: line.and_then(|l| i64::try_from(l).ok()),
    }
}

fn load_error(e: YamlError) -> AdapterError {
    match e {
        YamlError::Py(p) => AdapterError::failed(p.class, p.message),
        y => AdapterError::failed(y.class(), y.to_string()),
    }
}

/// `parse_profiles(text)`: (valid presets, diagnostics); a preset with an error is dropped. A YAML error is a
/// diagnostic; a non-YAML exception PyYAML lets through (ValueError from `!!int x`) is returned as an error, as
/// Python raises it.
pub fn parse_profiles(
    text: &str,
) -> Result<(IndexMap<String, Preset>, Vec<Diagnostic>), AdapterError> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut presets: IndexMap<String, Preset> = IndexMap::new();
    if text.len() > MAX_PROFILES_BYTES {
        return Ok((
            presets,
            vec![diag(
                Severity::Error,
                format!("profiles.yaml is larger than {MAX_PROFILES_BYTES} bytes"),
                None,
            )],
        ));
    }
    let doc = match pyyaml::safe_load_value(text) {
        Ok(d) => d,
        Err(e) if e.is_yaml() => {
            let msg = format!("YAML syntax: {}", pystr::head(&e.problem_or_str(), 200));
            return Ok((
                presets,
                vec![diag(
                    Severity::Error,
                    msg,
                    e.problem_mark().map(|m| m.line + 1),
                )],
            ));
        }
        Err(e) => return Err(load_error(e)),
    };
    let lines = line_map(text);
    let line = |path: &[&str]| {
        lines
            .get(&path.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
            .copied()
    };
    let Yv::Dict(top) = &doc else {
        return Ok((
            presets,
            vec![diag(
                Severity::Error,
                "the document must be a mapping with 'version' and 'profiles'",
                None,
            )],
        ));
    };
    for (k, _) in top {
        if !matches!(k, Yv::Str(s) if s == "version" || s == "profiles") {
            let ks = k.py_str();
            diags.push(diag(
                Severity::Error,
                format!(
                    "unknown top-level key {}{}",
                    k.repr(),
                    close(&ks, &["version", "profiles"])
                ),
                line(&[&ks]),
            ));
        }
    }
    let version = doc.get("version");
    let version_ok =
        matches!(version, Some(Yv::Int(1))) || matches!(version, Some(Yv::Float(f)) if *f == 1.0);
    if !version_ok {
        diags.push(diag(
            Severity::Error,
            "'version' must be 1",
            line(&["version"]),
        ));
    }
    let profiles = match doc.get("profiles") {
        Some(Yv::Dict(p)) if !p.is_empty() => p,
        _ => {
            diags.push(diag(
                Severity::Error,
                "'profiles' must be a mapping with at least one preset",
                line(&["profiles"]),
            ));
            return Ok((presets, diags));
        }
    };
    if profiles.len() > MAX_PROFILES {
        diags.push(diag(
            Severity::Error,
            format!(
                "at most {MAX_PROFILES} presets are supported (found {})",
                profiles.len()
            ),
            line(&["profiles"]),
        ));
    }
    let keys = profile_keys();
    for (name_v, body) in profiles.iter().take(MAX_PROFILES) {
        let pname = name_v.py_str();
        let name = match name_v {
            Yv::Str(s) if is(&PRESET_RE, s) => s.clone(),
            _ => {
                diags.push(diag(
                    Severity::Error,
                    format!(
                        "preset name {} must match [a-z0-9][a-z0-9-]{{0,39}}",
                        str_repr(&pname)
                    ),
                    line(&["profiles", &pname]),
                ));
                continue;
            }
        };
        let Yv::Dict(fields) = body else {
            diags.push(diag(
                Severity::Error,
                format!("preset {} must be a mapping", str_repr(&name)),
                line(&["profiles", &name]),
            ));
            continue;
        };
        let before = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        for (k, _) in fields {
            if !matches!(k, Yv::Str(s) if keys.contains(&s.as_str())) {
                let ks = k.py_str();
                diags.push(diag(
                    Severity::Error,
                    format!("{name}: unknown key {}{}", k.repr(), close(&ks, &keys)),
                    line(&["profiles", &name, &ks]),
                ));
            }
        }
        let model = body.get("model");
        let model_ok = matches!(model, Some(Yv::Str(m)) if is(&MODEL_RE, m));
        if !model_ok {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'model' is required: a router model alias (letters, digits and . _ : / @ + -)"),
                line(&["profiles", &name, "model"]),
            ));
        }
        let etype = body
            .get("endpoint_type")
            .cloned()
            .unwrap_or_else(|| Yv::Str("chat".to_owned()));
        let etype_ok = matches!(&etype, Yv::Str(e) if ENDPOINT_TYPES.contains(&e.as_str()));
        if !etype_ok {
            diags.push(diag(
                Severity::Error,
                format!(
                    "{name}: 'endpoint_type' must be one of {}",
                    ENDPOINT_TYPES.join(", ")
                ),
                line(&["profiles", &name, "endpoint_type"]),
            ));
        }
        let streaming = body.get("streaming").cloned().unwrap_or(Yv::Bool(true));
        if !matches!(streaming, Yv::Bool(_)) {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'streaming' must be true or false"),
                line(&["profiles", &name, "streaming"]),
            ));
        }
        let desc = body
            .get("description")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        let desc_ok = matches!(&desc, Yv::Str(d) if d.chars().count() <= 300 && !d.contains('\n'));
        if !desc_ok {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'description' must be a single line of at most 300 characters"),
                line(&["profiles", &name, "description"]),
            ));
        }
        let mut ints: HashMap<&str, i64> = HashMap::new();
        for (key, lo, hi, required) in INT_KEYS {
            let present = fields
                .iter()
                .find(|(k, _)| matches!(k, Yv::Str(s) if s == key))
                .map(|(_, v)| v);
            let Some(v) = present else {
                if required {
                    diags.push(diag(
                        Severity::Error,
                        format!("{name}: '{key}' is required"),
                        line(&["profiles", &name]),
                    ));
                }
                continue;
            };
            match v {
                Yv::Int(i) if lo <= i128::from(*i) && i128::from(*i) <= hi => {
                    ints.insert(key, *i);
                }
                _ => diags.push(diag(
                    Severity::Error,
                    format!("{name}: '{key}' must be an integer from {lo} to {hi}"),
                    line(&["profiles", &name, key]),
                )),
            }
        }
        if diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
            > before
        {
            continue;
        }
        if streaming == Yv::Bool(false) {
            diags.push(diag(
                Severity::Warn,
                format!("{name}: streaming is off, so time to first token and inter-token latency are not measured"),
                line(&["profiles", &name, "streaming"]),
            ));
        }
        let get = |k: &str| ints.get(k).copied();
        if get("requests") < get("concurrency") {
            diags.push(diag(
                Severity::Warn,
                format!("{name}: fewer requests than the concurrency, so the target concurrency is never reached"),
                line(&["profiles", &name, "requests"]),
            ));
        }
        presets.insert(
            name.clone(),
            Preset {
                name: name.clone(),
                model: model.map(Yv::py_str).unwrap_or_default(),
                endpoint_type: etype.py_str(),
                streaming: streaming == Yv::Bool(true),
                concurrency: get("concurrency").unwrap_or(0),
                requests: get("requests").unwrap_or(0),
                isl: get("isl").unwrap_or(0),
                osl: get("osl").unwrap_or(0),
                warmup_requests: get("warmup_requests").unwrap_or(0),
                timeout_s: get("timeout_s").unwrap_or(120),
                seed: get("seed").unwrap_or(42),
                description: desc.py_str(),
            },
        );
    }
    Ok((presets, diags))
}

/// The exact aiperf-local argv for a preset. The wrapper adds the URL, the credential, the tokenizer and the
/// offline/telemetry-off policy; nothing secret is, or can be, in this list.
#[must_use]
pub fn render_argv(launcher: &str, p: &Preset, artifact_dir: &Path) -> Vec<String> {
    let mut argv = wsb::argv(&[
        launcher,
        "profile",
        "--model",
        &p.model,
        "--endpoint-type",
        &p.endpoint_type,
    ]);
    if p.streaming {
        argv.push("--streaming".into());
    }
    for (flag, v) in [
        ("--concurrency", p.concurrency),
        ("--request-count", p.requests),
        ("--prompt-input-tokens-mean", p.isl),
        ("--prompt-output-tokens-mean", p.osl),
    ] {
        argv.push(flag.into());
        argv.push(v.to_string());
    }
    if p.warmup_requests > 0 {
        argv.push("--warmup-request-count".into());
        argv.push(p.warmup_requests.to_string());
    }
    argv.extend(wsb::argv(&[
        "--request-timeout-seconds",
        &p.timeout_s.to_string(),
        "--random-seed",
        &p.seed.to_string(),
        "--export-level",
        "records",
        "--use-server-token-count",
        "--artifact-dir",
        &artifact_dir.to_string_lossy(),
    ]));
    argv
}

/// The few numbers the console keeps from profile_export_aiperf.json. Missing keys are simply absent.
#[must_use]
pub fn summarize(doc: &Map<String, Value>) -> IndexMap<String, f64> {
    let mut out = IndexMap::new();
    let mut put = |key: &str, section: &str, field: &str| {
        if let Some(v) = wsb::json_num(
            doc.get(section)
                .and_then(Value::as_object)
                .and_then(|s| s.get(field)),
        ) {
            out.insert(key.to_owned(), v);
        }
    };
    for q in ["p50", "p99"] {
        put(&format!("ttft_{q}_ms"), "time_to_first_token", q);
        put(&format!("itl_{q}_ms"), "inter_token_latency", q);
        put(&format!("latency_{q}_ms"), "request_latency", q);
    }
    put("output_tps", "output_token_throughput", "avg");
    put("request_tps", "request_throughput", "avg");
    put("requests", "request_count", "avg");
    put("error_rate_pct", "request_error_rate", "avg");
    put("duration_s", "benchmark_duration", "avg");
    put("isl_avg", "input_sequence_length", "avg");
    put("osl_avg", "output_sequence_length", "avg");
    out
}

/// The failure text of a run: ANSI and box-drawing removed, routine log lines dropped, the error panel kept.
#[must_use]
pub fn clean_error(text: &str) -> String {
    let plain = sub(&BOX_RE, &sub(&ANSI_RE, text, ""), " ");
    let lines: Vec<String> = pystr::splitlines(&plain)
        .into_iter()
        .map(|l| pystr::split_ws(l).join(" "))
        .filter(|l| !l.is_empty() && !is(&LOG_LINE_RE, l))
        .collect();
    let keep: Vec<&String> = lines.iter().filter(|l| is(&KEEP_RE, l)).collect();
    let pick: Vec<&String> = if keep.is_empty() {
        lines.iter().collect()
    } else {
        keep
    };
    let start = pick.len().saturating_sub(8);
    let joined = pick[start..]
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    pystr::head(&redact(&joined), 1500).to_owned()
}

/// (failed requests seen, the first failure as one line) from profile_export.jsonl. Never fails.
#[must_use]
pub fn request_errors(path: &Path) -> (usize, String) {
    let (mut count, mut first) = (0usize, String::new());
    if path.is_symlink() || !path.is_file() {
        return (0, String::new());
    }
    // at most MAX_EXPORT_BYTES of the file (audit finding 9; only its first MAX_RECORD_LINES lines are read anyway)
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| {
        use std::io::Read as _;
        f.take(MAX_EXPORT_BYTES).read_to_end(&mut bytes)
    });
    if read.is_err() {
        return (count, first);
    }
    for (n, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if n >= MAX_RECORD_LINES {
            break;
        }
        let Ok(rec) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(err) = rec
            .as_object()
            .and_then(|r| r.get("error"))
            .and_then(Value::as_object)
            .filter(|e| !e.is_empty())
        else {
            continue;
        };
        count += 1;
        if first.is_empty() {
            let mut msg = match err.get("message") {
                Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
                _ => String::new(),
            };
            if let Ok(inner) = serde_json::from_str::<Value>(&msg)
                && let Some(Value::String(m)) = inner
                    .as_object()
                    .and_then(|o| o.get("error"))
                    .and_then(Value::as_object)
                    .and_then(|e| e.get("message"))
            {
                msg = m.clone();
            }
            let code = err
                .get("code")
                .map_or_else(|| "?".to_owned(), wsb::json_str);
            let typ = err.get("type").map_or_else(String::new, wsb::json_str);
            first = pystr::head(
                &redact(pystr::strip(&format!("HTTP {code} {typ}: {msg}"))),
                400,
            )
            .to_owned();
        }
    }
    (count, first)
}

fn num(meta: &Map<String, Value>, k: &str) -> Option<f64> {
    wsb::json_num(meta.get(k))
}

fn get_or(meta: &Map<String, Value>, k: &str, default: &str) -> String {
    meta.get(k)
        .map_or_else(|| default.to_owned(), wsb::json_str)
}

struct LiveRun {
    run_id: String,
    preset: String,
    started: f64,
    /// Python `task.done()`: set when the task ends, finished or cancelled.
    done: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl LiveRun {
    fn running(&self) -> bool {
        !self.done.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Marks the live run done when the task ends, however it ends (dropped last, after the outcome is written).
struct DoneFlag(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DoneFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// (checked at, launcher, version)
type VersionCache = (Instant, Option<String>, Option<String>);
/// (presets, diagnostics, file found)
type Profiles = (IndexMap<String, Preset>, Vec<Diagnostic>, bool);

struct Inner {
    ctx: AdapterContext,
    version_cache: Mutex<Option<VersionCache>>,
    follow_interval: Mutex<f64>,
    live: Mutex<Option<LiveRun>>,
}

/// The AIPerf adapter. Cheap to clone handles share one state (the background run needs it).
pub struct AiPerfAdapter {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn done(t0: Instant, ok: bool, out: &str, problems: Vec<Problem>) -> ActionResult {
    ActionResult {
        ok,
        output: pystr::head(&redact(out), 20_000).to_owned(),
        duration_ms: i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX),
        problems,
    }
}

fn problem(code: &str, message: &str, fix: String) -> Vec<Problem> {
    vec![Problem::new(Severity::Error, code, message, Some(fix))]
}

impl Inner {
    fn litellm(&self) -> String {
        self.ctx
            .urls
            .get("litellm")
            .map_or("http://127.0.0.1:4000", String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn profiles_path(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("aiperf_profiles")
    }

    fn key_file(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("aiperf_key_file")
    }

    fn runs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("aiperf").join("runs")
    }

    /// A lab helper: `rel`'s file under the `aiperf_dir` layout dir, else `rel` under the repo root.
    fn lab_file(&self, rel: &str) -> Option<PathBuf> {
        let name = Path::new(rel)
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_default();
        let cands = [
            self.ctx.paths.get("aiperf_dir").map(|b| b.join(&name)),
            self.ctx.paths.get("repo").map(|r| r.join(rel)),
        ];
        cands.into_iter().flatten().find(|p| p.exists())
    }

    fn install_fix(&self) -> String {
        match self.lab_file(LAB_INSTALLER) {
            Some(lab) => format!(
                "{}   # pinned AIPerf {PINNED_VERSION} + the aiperf-local wrapper + the vendored tokenizer",
                lab.display()
            ),
            None => format!(
                "run clients/aiperf/install.sh in the lab repository (pinned AIPerf {PINNED_VERSION}; it links the aiperf-local wrapper onto PATH)"
            ),
        }
    }

    fn key_fix(&self) -> String {
        match self.lab_file(LAB_MINT_KEY) {
            Some(lab) => format!("{}   # mints AIPerf's own router key (key_alias 'aiperf')", lab.display()),
            None => "run clients/aiperf/mint-key.sh in the lab repository (it writes ~/.config/aiperf/litellm.key, 0600)".to_owned(),
        }
    }

    fn tokenizer_fix(&self) -> String {
        let where_ = self
            .ctx
            .paths
            .get("aiperf_dir")
            .cloned()
            .unwrap_or_else(|| PathBuf::from("clients/aiperf"));
        format!(
            "git -C {} checkout -- vendor/o200k_base.tiktoken && {} install-tokenizer   # the vendored, hash-pinned tokenizer; nothing is downloaded",
            where_.display(),
            where_.join(LAUNCHER).display()
        )
    }

    /// Ready when the runtime cache holds a file OR the vendored vocabulary exists.
    fn has_tokenizer(&self) -> bool {
        let Some(dir) = self.ctx.paths.get("aiperf_dir") else {
            return true;
        };
        if dir.join("vendor").join("o200k_base.tiktoken").is_file() {
            return true;
        }
        let cache = dir.join(".tiktoken-cache");
        if !cache.is_dir() {
            return false;
        }
        match std::fs::read_dir(&cache) {
            Ok(rd) => rd.flatten().any(|e| e.path().is_file()),
            Err(_) => false,
        }
    }

    async fn run(&self, argv: &[String], opts: RunOpts) -> CommandResult {
        wsb::run(&self.ctx, argv, opts).await
    }

    async fn launcher(&self) -> (Option<String>, Option<String>) {
        if let Some((ts, Some(l), v)) = lock(&self.version_cache).clone()
            && ts.elapsed() < Duration::from_secs_f64(VERSION_TTL)
        {
            return (Some(l), v);
        }
        let (mut launcher, mut version) = (None, None);
        let r = self
            .run(&wsb::argv(&[LAUNCHER, "--version"]), wsb::opts(20.0, 4096))
            .await;
        if r.returncode == 0 {
            let stripped = sub(&ANSI_RE, &r.stdout, "");
            let first = pystr::splitlines(pystr::strip(&stripped))
                .first()
                .map(|l| pystr::head(l, 40).to_owned());
            launcher = Some(LAUNCHER.to_owned());
            version = first;
        }
        *lock(&self.version_cache) = Some((Instant::now(), launcher.clone(), version.clone()));
        (launcher, version)
    }

    async fn router_up(&self) -> (bool, String) {
        match self
            .ctx
            .http
            .get(&format!("{}/health/liveliness", self.litellm()), 3.0)
            .await
        {
            Ok(r) => (r.status < 500, format!("HTTP {}", r.status)),
            Err(e) => (false, e.class.to_owned()),
        }
    }

    fn live_run_id(&self) -> Option<String> {
        lock(&self.live)
            .as_ref()
            .filter(|l| l.running())
            .map(|l| l.run_id.clone())
    }

    /// (presets, diagnostics, file_found). A missing or unreadable file is zero presets.
    fn read_profiles(&self) -> Result<Profiles, AdapterError> {
        let Some(path) = self.profiles_path() else {
            return Ok((IndexMap::new(), Vec::new(), false));
        };
        if path.is_symlink() || !path.is_file() {
            return Ok((IndexMap::new(), Vec::new(), false));
        }
        let too_large = std::fs::metadata(path).map(|m| m.len() > MAX_PROFILES_BYTES as u64);
        match too_large {
            Ok(true) => {
                return Ok((
                    IndexMap::new(),
                    vec![diag(Severity::Error, "profiles.yaml is too large", None)],
                    true,
                ));
            }
            Ok(false) => {}
            Err(_) => {
                return Ok((
                    IndexMap::new(),
                    vec![diag(Severity::Error, "profiles.yaml cannot be read", None)],
                    true,
                ));
            }
        }
        let Ok(Ok(text)) = std::fs::read(path).map(String::from_utf8) else {
            return Ok((
                IndexMap::new(),
                vec![diag(Severity::Error, "profiles.yaml cannot be read", None)],
                true,
            ));
        };
        let (p, d) = parse_profiles(&text)?;
        Ok((p, d, true))
    }

    // ── stored runs ─────────────────────────────────────────────────────────────────────────
    /// run.json; a run left at `running` by a process that no longer runs it reads as interrupted.
    fn load_meta(&self, run_dir: &Path) -> Option<Map<String, Value>> {
        let mut meta = wsb::load_json_dict(&run_dir.join("run.json"))?;
        if meta.get("verdict").and_then(Value::as_str) == Some("running") {
            let live = self.live_run_id();
            let same = match (meta.get("run_id"), &live) {
                (Some(Value::String(r)), Some(l)) => r == l,
                _ => false,
            };
            if !same {
                meta.insert("verdict".into(), json!("interrupted"));
                meta.insert("interrupted_by".into(), json!("restart"));
            }
        }
        Some(meta)
    }

    fn headline(meta: &Map<String, Value>) -> String {
        let mut parts = vec![get_or(meta, "preset", "?"), get_or(meta, "model", "?")];
        if let Some(c) = num(meta, "concurrency") {
            parts.push(format!("c={c:.0}"));
        }
        parts.push(get_or(meta, "verdict", "?"));
        let m = meta.get("metrics").and_then(Value::as_object);
        if let Some(v) = m.and_then(|m| wsb::json_num(m.get("ttft_p50_ms"))) {
            parts.push(format!("TTFT p50 {v:.0} ms"));
        }
        if let Some(v) = m.and_then(|m| wsb::json_num(m.get("output_tps"))) {
            parts.push(format!("{v:.0} output tok/s"));
        }
        if meta.get("verdict").and_then(Value::as_str) == Some("running") {
            let now = now_ts();
            let ts = num(meta, "ts").filter(|t| *t != 0.0).unwrap_or(now);
            let mins = ((now - ts) / 60.0).max(0.0);
            parts.push(format!("running for {mins:.0} min"));
        }
        pystr::head(&parts.join(" · "), 300).to_owned()
    }

    fn event(meta: &Map<String, Value>) -> SystemEvent {
        let mut data = Map::new();
        for k in [
            "run_id",
            "preset",
            "model",
            "verdict",
            "concurrency",
            "requests",
            "isl",
            "osl",
            "duration_ms",
        ] {
            data.insert(k.to_owned(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        let m = meta.get("metrics").and_then(Value::as_object);
        for k in [
            "ttft_p50_ms",
            "ttft_p99_ms",
            "itl_p50_ms",
            "output_tps",
            "request_tps",
            "error_rate_pct",
        ] {
            if let Some(v) = m.and_then(|m| wsb::json_num(m.get(k))) {
                data.insert(k.to_owned(), json!(v));
            }
        }
        SystemEvent {
            ts: num(meta, "ts").filter(|t| *t != 0.0).unwrap_or(0.0),
            system: SystemId::Aiperf,
            kind: EVENT_KIND.to_owned(),
            summary: Self::headline(meta),
            data,
        }
    }

    fn run_dirs(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.runs_dir()) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| is(&RUN_ID_RE, n))
                    && p.is_dir()
            })
            .collect();
        dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        dirs
    }

    fn metas(&self, limit: usize) -> Vec<Map<String, Value>> {
        self.run_dirs()
            .iter()
            .take(limit)
            .filter_map(|d| self.load_meta(d))
            .collect()
    }

    fn list_runs(&self, limit: usize) -> Vec<SystemEvent> {
        self.metas(limit).iter().map(Self::event).collect()
    }

    fn prune(&self) {
        let live = self.live_run_id();
        for d in self.run_dirs().iter().skip(KEEP_RUNS) {
            if d.file_name().and_then(|n| n.to_str()) != live.as_deref() {
                wsb::rmtree(d);
            }
        }
    }

    fn new_run_dir(&self, preset: &str) -> std::io::Result<(String, PathBuf)> {
        let run_id = format!("{}-{preset}", wsb::run_stamp());
        let run_dir = self.runs_dir().join(&run_id);
        wsb::mkdir_leaf(&run_dir, 0o700)?;
        Ok((run_id, run_dir))
    }

    fn load_json(p: &Path) -> Option<Map<String, Value>> {
        if p.is_symlink() {
            return None;
        }
        if std::fs::metadata(p).ok()?.len() > MAX_JSON_BYTES {
            return None;
        }
        wsb::load_json_dict(p)
    }

    // ── report ──────────────────────────────────────────────────────────────────────────────
    fn report_text(&self, run_id: &str) -> (bool, String) {
        let run_dir = if run_id.is_empty() {
            let metas = self.metas(1);
            let Some(first) = metas.first() else {
                return (false, "no stored runs yet".to_owned());
            };
            self.runs_dir().join(wsb::json_get_str(first.get("run_id")))
        } else {
            if !is(&RUN_ID_RE, run_id) {
                return (false, "unknown run id".to_owned());
            }
            self.runs_dir().join(run_id)
        };
        let Some(meta) = self.load_meta(&run_dir) else {
            return (false, "unknown run id".to_owned());
        };
        let mut lines = vec![Self::headline(&meta), String::new()];
        let verdict = meta.get("verdict").and_then(Value::as_str).unwrap_or("");
        if verdict == "running" {
            let now = now_ts();
            let ts = num(&meta, "ts").filter(|t| *t != 0.0).unwrap_or(now);
            let mins = ((now - ts) / 60.0).max(0.0);
            lines.push(format!(
                "Running for {mins:.0} min. This report fills in when the run ends."
            ));
            lines.push(String::new());
        }
        if verdict == "error" || verdict == "failed" {
            lines.push("AIPerf did not complete the run:".to_owned());
            lines.push(get_or(&meta, "error", ""));
            lines.push(String::new());
        }
        if verdict == "interrupted" {
            lines.push("Interrupted: the run was cancelled or the console stopped or restarted while it ran.".to_owned());
            lines.push(String::new());
        }
        lines.push(format!(
            "Load: model {}, {} endpoint, streaming {}, concurrency {}, {} requests (+{} warmup), {} input / {} output tokens.",
            get_or(&meta, "model", "?"),
            get_or(&meta, "endpoint_type", "?"),
            if wsb::json_truthy(meta.get("streaming")) { "on" } else { "off" },
            get_or(&meta, "concurrency", "?"),
            get_or(&meta, "requests", "?"),
            get_or(&meta, "warmup_requests", "0"),
            get_or(&meta, "isl", "?"),
            get_or(&meta, "osl", "?"),
        ));
        if verdict == "succeeded"
            && let Some(n) = num(&meta, "failed_requests").filter(|n| *n != 0.0)
        {
            lines.push(format!(
                "{n:.0} request(s) failed; the first: {}",
                get_or(&meta, "first_error", "?")
            ));
        }
        if let Some(m) = meta
            .get("metrics")
            .and_then(Value::as_object)
            .filter(|m| !m.is_empty())
        {
            let cell = |key: &str, digits: usize| match wsb::json_num(m.get(key)) {
                Some(v) => format!("{v:.digits$}"),
                None => "-".to_owned(),
            };
            lines.extend([
                String::new(),
                "| Metric | p50 | p99 |".to_owned(),
                "| --- | --- | --- |".to_owned(),
                format!("| Time to first output (ms) | {} | {} |", cell("ttft_p50_ms", 1), cell("ttft_p99_ms", 1)),
                format!("| Inter-token latency (ms) | {} | {} |", cell("itl_p50_ms", 2), cell("itl_p99_ms", 2)),
                format!("| Request latency (ms) | {} | {} |", cell("latency_p50_ms", 1), cell("latency_p99_ms", 1)),
                String::new(),
                format!(
                    "Output throughput: {} tokens/s · request throughput: {} requests/s · error rate: {} % · duration {} s.",
                    cell("output_tps", 1),
                    cell("request_tps", 2),
                    cell("error_rate_pct", 1),
                    cell("duration_s", 2)
                ),
                "These numbers include the router hop (this measures the lab path, not the bare engine).".to_owned(),
            ]);
        }
        let artifacts = run_dir.join("artifacts");
        if artifacts.join(SUMMARY_FILE).is_file() {
            lines.push(String::new());
            lines.push(format!(
                "Artifacts (JSON, CSV, console table, log): {}",
                artifacts.display()
            ));
        }
        (true, lines.join("\n"))
    }
}

/// `_write_meta`: errors are redacted even if a caller forgot; `.run.json.tmp` + chmod 0600 + rename.
pub fn write_meta(run_dir: &Path, meta: &Map<String, Value>) -> std::io::Result<()> {
    let mut out = meta.clone();
    if let Some(Value::String(e)) = out.get("error") {
        let red = redact(e);
        out.insert("error".into(), Value::String(red));
    }
    wsb::write_private(
        &run_dir.join("run.json"),
        &wsb::json_indent1(&Value::Object(out)),
    )
}

/// Writes the "interrupted" outcome if the run task is dropped (console stop / cancel) before it finished.
struct InterruptGuard {
    run_dir: PathBuf,
    meta: Map<String, Value>,
    started: Instant,
    finishing: bool,
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if self.finishing {
            return;
        }
        // The console is stopping: the runner has already interrupted the wrapper (SIGINT, then SIGKILL).
        self.meta.insert("verdict".into(), json!("interrupted"));
        self.meta
            .insert("interrupted_by".into(), json!("console stop"));
        self.meta.insert(
            "duration_ms".into(),
            json!(i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)),
        );
        let _ = write_meta(&self.run_dir, &self.meta);
    }
}

async fn profile_task(
    inner: Arc<Inner>,
    run_dir: PathBuf,
    argv: Vec<String>,
    meta: Map<String, Value>,
    done: Arc<std::sync::atomic::AtomicBool>,
) {
    let _done = DoneFlag(done);
    let mut guard = InterruptGuard {
        run_dir: run_dir.clone(),
        meta,
        started: Instant::now(),
        finishing: false,
    };
    let r = inner
        .run(
            &argv,
            RunOpts::timeout(PROFILE_TIMEOUT)
                .max_bytes(PROFILE_MAX_BYTES)
                .cwd(run_dir.clone()),
        )
        .await;
    guard.finishing = true; // from here this task writes the outcome; never overwrite it
    let ms = i64::try_from(guard.started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let mut meta = std::mem::take(&mut guard.meta);
    if let Err(e) = finish(&inner, &run_dir, &mut meta, &r, ms) {
        // bookkeeping failed: still close the run, never leave it "running"
        let class = match e.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            _ => "OSError",
        };
        meta.insert("verdict".into(), json!("error"));
        meta.insert(
            "error".into(),
            json!(pystr::head(&format!("{class}: {e}"), 300)),
        );
        let _ = write_meta(&run_dir, &meta);
    }
}

fn finish(
    inner: &Inner,
    run_dir: &Path,
    meta: &mut Map<String, Value>,
    r: &CommandResult,
    ms: i64,
) -> std::io::Result<()> {
    meta.insert("duration_ms".into(), json!(ms));
    let artifacts = run_dir.join("artifacts");
    let doc = Inner::load_json(&artifacts.join(SUMMARY_FILE));
    let mut set = |k: &str, v: Value| {
        meta.insert(k.to_owned(), v);
    };
    if r.timed_out {
        set("verdict", json!("failed"));
        set("error", json!("timed out after 3600 s"));
    } else if r.returncode != 0 {
        let (n_err, first) = request_errors(&artifacts.join(RECORDS_FILE));
        if n_err > 0 {
            set("failed_requests", json!(n_err));
        }
        let detail = if first.is_empty() {
            clean_error(&format!("{}\n{}", r.stderr, r.stdout))
        } else {
            format!("{n_err} request(s) failed; the first: {first}")
        };
        set("verdict", json!("failed"));
        set(
            "error",
            json!(if detail.is_empty() {
                format!("exit {}", r.returncode)
            } else {
                detail
            }),
        );
    } else if let Some(doc) = doc {
        let metrics = summarize(&doc);
        let requests = metrics.get("requests").copied();
        set("metrics", json!(metrics));
        let (n_err, first) = request_errors(&artifacts.join(RECORDS_FILE));
        if n_err > 0 {
            set("failed_requests", json!(n_err));
            set("first_error", json!(first));
        }
        set(
            "aiperf_version",
            doc.get("aiperf_version")
                .filter(|v| v.is_string())
                .cloned()
                .unwrap_or(Value::Null),
        );
        if doc.get("was_cancelled") == Some(&Value::Bool(true)) {
            set("verdict", json!("interrupted"));
            set("interrupted_by", json!("aiperf (cancelled)"));
        } else if doc.get("is_complete") == Some(&Value::Bool(false)) {
            set("verdict", json!("failed"));
            set("error", json!("aiperf reported an incomplete run"));
        } else if requests.is_none_or(|n| n == 0.0) {
            set("verdict", json!("error"));
            set("error", json!("the summary records no completed request"));
        } else {
            set("verdict", json!("succeeded"));
        }
    } else {
        set("verdict", json!("error"));
        set(
            "error",
            json!(format!(
                "aiperf-local exited 0 but wrote no readable {SUMMARY_FILE}"
            )),
        );
    }
    write_meta(run_dir, meta)?;
    inner.prune();
    Ok(())
}

impl AiPerfAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        AiPerfAdapter {
            inner: Arc::new(Inner {
                ctx,
                version_cache: Mutex::new(None),
                follow_interval: Mutex::new(2.0),
                live: Mutex::new(None),
            }),
        }
    }

    #[must_use]
    pub fn ctx(&self) -> &AdapterContext {
        &self.inner.ctx
    }

    pub fn set_follow_interval(&self, s: f64) {
        *lock(&self.inner.follow_interval) = s;
    }

    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.inner.runs_dir()
    }

    /// `_live_run_id()`: the run the background task is writing, while it runs.
    #[must_use]
    pub fn live_run_id(&self) -> Option<String> {
        self.inner.live_run_id()
    }

    /// Whether a background run was ever started (Python `_live is not None`).
    #[must_use]
    pub fn has_live(&self) -> bool {
        lock(&self.inner.live).is_some()
    }

    /// Wait for the background run to end (Python `await a._live.task`); false if it was cancelled.
    pub async fn join_live(&self) -> bool {
        let handle = lock(&self.inner.live)
            .as_mut()
            .and_then(|l| l.handle.take());
        match handle {
            Some(h) => h.await.is_ok(),
            None => true,
        }
    }

    /// Cancel the background run (Python `a._live.task.cancel()`; the console stopping).
    pub fn abort_live(&self) {
        if let Some(h) = lock(&self.inner.live)
            .as_ref()
            .and_then(|l| l.handle.as_ref())
        {
            h.abort();
        }
    }

    /// `_report_text(run_id)`.
    #[must_use]
    pub fn report_text(&self, run_id: &str) -> (bool, String) {
        self.inner.report_text(run_id)
    }

    /// `_lab_file(rel)`.
    #[must_use]
    pub fn lab_file(&self, rel: &str) -> Option<PathBuf> {
        self.inner.lab_file(rel)
    }

    async fn start_profile(
        &self,
        params: &Map<String, Value>,
        t0: Instant,
    ) -> Result<ActionResult, AdapterError> {
        let inner = &self.inner;
        if let Some(busy) = inner.live_run_id() {
            return Ok(done(
                t0,
                false,
                &format!(
                    "A load run is already running ({busy}); one runs at a time. It shows in the runs list."
                ),
                vec![Problem::new(
                    Severity::Warn,
                    "ap.profile.busy",
                    format!("load run {busy} is still running"),
                    Some(format!(
                        "'Show a stored run' with run id {busy} shows its state"
                    )),
                )],
            ));
        }
        let (presets, diags, _) = inner.read_profiles()?;
        let name = match params.get("preset") {
            Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
            _ => String::new(),
        };
        let Some(preset) = presets.get(&name).cloned() else {
            let bad: Vec<&str> = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect();
            let extra = bad.first().map(|b| format!(" ({b})")).unwrap_or_default();
            return Ok(done(
                t0,
                false,
                &format!("unknown or invalid preset {}{extra}", str_repr(&name)),
                Vec::new(),
            ));
        };
        let (launcher, _) = inner.launcher().await;
        let Some(launcher) = launcher else {
            return Ok(done(
                t0,
                false,
                "AIPerf is not installed.",
                problem(
                    "ap.not_installed",
                    "aiperf-local is not on PATH.",
                    inner.install_fix(),
                ),
            ));
        };
        if let Some(k) = inner.key_file()
            && !k.exists()
        {
            return Ok(done(
                t0,
                false,
                "AIPerf's router key file is missing.",
                problem(
                    "ap.no_key",
                    &format!("{} is absent.", k.display()),
                    inner.key_fix(),
                ),
            ));
        }
        if !inner.has_tokenizer() {
            return Ok(done(
                t0,
                false,
                "The offline tokenizer is missing.",
                problem(
                    "ap.no_tokenizer",
                    "clients/aiperf/vendor/o200k_base.tiktoken and the cache are both absent.",
                    inner.tokenizer_fix(),
                ),
            ));
        }
        let (router_ok, router_detail) = inner.router_up().await;
        if !router_ok {
            return Ok(done(
                t0,
                false,
                &format!(
                    "The router ({}) is not answering ({router_detail}).",
                    inner.litellm()
                ),
                problem(
                    "ap.router_down",
                    "the router is down",
                    format!("curl -s {}/health/liveliness", inner.litellm()),
                ),
            ));
        }
        // From here to the spawn nothing awaits, and the live slot is held: no second request slips in between.
        let mut live = lock(&inner.live);
        if let Some(busy) = live
            .as_ref()
            .filter(|l| l.running())
            .map(|l| l.run_id.clone())
        {
            return Ok(done(
                t0,
                false,
                &format!("A load run is already running ({busy}); one runs at a time."),
                Vec::new(),
            ));
        }
        let (run_id, run_dir) = inner
            .new_run_dir(&name)
            .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let argv = render_argv(&launcher, &preset, &run_dir.join("artifacts"));
        let started = now_ts();
        let meta = json!({
            "run_id": run_id, "preset": name, "mode": "profile", "ts": started, "verdict": "running",
            "model": preset.model, "endpoint_type": preset.endpoint_type, "streaming": preset.streaming,
            "concurrency": preset.concurrency, "requests": preset.requests, "isl": preset.isl, "osl": preset.osl,
            "warmup_requests": preset.warmup_requests, "launcher": launcher,
        });
        let meta = meta.as_object().cloned().unwrap_or_default();
        write_meta(&run_dir, &meta).map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = profile_task(Arc::clone(inner), run_dir, argv, meta, Arc::clone(&flag));
        let handle = tokio::spawn(async move {
            crate::services::common::contained("aiperf profile run", task).await;
        });
        *live = Some(LiveRun {
            run_id: run_id.clone(),
            preset: name.clone(),
            started,
            done: flag,
            handle: Some(handle),
        });
        drop(live);
        Ok(done(
            t0,
            true,
            &format!(
                "Started load run {run_id} ({name}: {}, concurrency {}, {} requests, {} in / {} out). It runs in the background: watch the runs list, then 'Show a stored run' with this run id.",
                preset.model, preset.concurrency, preset.requests, preset.isl, preset.osl
            ),
            Vec::new(),
        ))
    }
}

fn profiles_spec(has_path: bool) -> ConfigFileSpec {
    ConfigFileSpec {
        id: PROFILES_ID.into(),
        label: "Load profiles".into(),
        description: "Named AIPerf load presets: model alias, endpoint type, streaming, concurrency, request count and input/output lengths. Each preset drives a live engine through the router, so keep loads small.".into(),
        display_path: if has_path { "clients/aiperf/profiles.yaml".into() } else { PROFILES_ID.into() },
        format: "yaml".into(),
        editable: true,
        why_readonly: None,
        validators: vec!["nemo-console aiperf profiles schema".into()],
        authoritative: true,
        apply_effect: "takes effect on the next profile run; nothing to restart".into(),
    }
}

#[async_trait]
impl SystemAdapter for AiPerfAdapter {
    fn id(&self) -> SystemId {
        SystemId::Aiperf
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let inner = &self.inner;
        let ((launcher, version), (router_ok, router_detail)) =
            tokio::join!(inner.launcher(), inner.router_up());
        let (presets, diags, found) = inner.read_profiles()?;
        let last = inner.metas(1);
        let has_tok = inner.has_tokenizer();
        let key_ok = inner.key_file().is_none_or(|k| k.exists());
        let mut problems: Vec<Problem> = Vec::new();
        let mut components = vec![
            Component {
                kind: "process".into(),
                name: launcher.clone().unwrap_or_else(|| LAUNCHER.to_owned()),
                state: if launcher.is_some() {
                    "running"
                } else {
                    "absent"
                }
                .into(),
                image: None,
                detail: Some(match &launcher {
                    Some(l) => format!(
                        "{l} {}: router-only, loopback, its own key file, offline tokenizer; the console passes no environment and never sees a key",
                        version.clone().unwrap_or_default()
                    ),
                    None => "aiperf-local is not on PATH (installed by clients/aiperf/install.sh)"
                        .to_owned(),
                }),
            },
            Component {
                kind: "http".into(),
                name: format!("router {}", inner.litellm()),
                state: if router_ok { "running" } else { "stopped" }.into(),
                image: None,
                detail: Some(format!(
                    "{router_detail} on GET /health/liveliness (the load target)"
                )),
            },
        ];
        if inner.live_run_id().is_some()
            && let Some(live) = lock(&inner.live).as_ref()
        {
            let mins = ((now_ts() - live.started) / 60.0).max(0.0);
            components.push(Component {
                kind: "process".into(),
                name: format!("profile {}", live.preset),
                state: "running".into(),
                image: None,
                detail: Some(format!(
                    "load run {}, running for {mins:.0} min (one at a time)",
                    live.run_id
                )),
            });
        }
        if launcher.is_none() {
            problems.push(Problem::new(
                Severity::Error,
                "ap.not_installed",
                "AIPerf is not installed (or aiperf-local is not on the console's PATH).",
                Some(inner.install_fix()),
            ));
        }
        if !router_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "ap.router_down",
                format!(
                    "The router ({}) is not answering; profile runs will fail.",
                    inner.litellm()
                ),
                Some(format!(
                    "curl -s -o /dev/null -w '%{{http_code}}\\n' {}/health/liveliness",
                    inner.litellm()
                )),
            ));
        }
        if launcher.is_some() && !key_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "ap.no_key",
                format!(
                    "AIPerf's router key file {} is absent; profile runs will be refused.",
                    inner
                        .key_file()
                        .map(|k| k.display().to_string())
                        .unwrap_or_else(|| "None".into())
                ),
                Some(inner.key_fix()),
            ));
        }
        if launcher.is_some() && !has_tok {
            problems.push(Problem::new(
                Severity::Warn,
                "ap.no_tokenizer",
                "Neither the vendored tokenizer (clients/aiperf/vendor/o200k_base.tiktoken) nor the runtime cache is present; profile runs will be refused.",
                Some(inner.tokenizer_fix()),
            ));
        }
        if !found {
            problems.push(Problem::new(
                Severity::Warn,
                "ap.no_profiles",
                format!(
                    "No profiles file at {}: there is no preset to run.",
                    inner.profiles_path().map(|p| p.display().to_string()).unwrap_or_else(|| "None".into())
                ),
                Some("restore clients/aiperf/profiles.yaml from the lab repository (git checkout -- clients/aiperf/profiles.yaml)".to_owned()),
            ));
        }
        let bad: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        if found && let Some(first) = bad.first() {
            let at = first
                .line
                .filter(|l| *l != 0)
                .map(|l| format!(" (line {l})"))
                .unwrap_or_default();
            problems.push(Problem::new(
                Severity::Warn,
                "ap.profiles_invalid",
                format!("profiles.yaml has {} error(s); first: {}{at}", bad.len(), first.message),
                Some("fix it on the Configure tab (the validator names each line) or restore clients/aiperf/profiles.yaml with git".to_owned()),
            ));
        }
        let health = if launcher.is_none() {
            Health::Down
        } else if !(router_ok && key_ok && has_tok) {
            Health::Degraded
        } else {
            Health::Up
        };
        let mut summary = format!("{} preset(s)", presets.len());
        if let Some(m) = last.first() {
            summary.push_str(&format!(" · last run: {}", Inner::headline(m)));
        }
        Ok(SystemStatus {
            system: SystemId::Aiperf,
            health,
            version,
            summary: pystr::head(&summary, 300).to_owned(),
            endpoints: vec![Endpoint {
                name: "router (load target)".into(),
                url: inner.litellm(),
                role: "api".into(),
                auth: "bearer".into(),
            }],
            components,
            problems,
            checked_at: now_ts(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let now = now_ts();
        let inner = &self.inner;
        let metas = inner.metas(KEEP_RUNS);
        #[allow(clippy::cast_precision_loss)]
        let mut points = vec![MetricPoint {
            key: "ap.runs".into(),
            value: metas.len() as f64,
            unit: "1".into(),
            labels: IndexMap::new(),
        }];
        if !metas.is_empty() {
            points.push(MetricPoint {
                key: "ap.live_running".into(),
                value: if inner.live_run_id().is_some() {
                    1.0
                } else {
                    0.0
                },
                unit: "1".into(),
                labels: IndexMap::new(),
            });
            for verdict in VERDICTS {
                let n = metas
                    .iter()
                    .filter(|m| m.get("verdict").and_then(Value::as_str) == Some(verdict))
                    .count();
                let mut labels = IndexMap::new();
                labels.insert("verdict".to_owned(), verdict.to_owned());
                #[allow(clippy::cast_precision_loss)]
                points.push(MetricPoint {
                    key: "ap.runs_by_verdict".into(),
                    value: n as f64,
                    unit: "1".into(),
                    labels,
                });
            }
        }
        let newest = metas.iter().find(|m| {
            m.get("verdict").and_then(Value::as_str) == Some("succeeded")
                && m.get("metrics")
                    .and_then(Value::as_object)
                    .is_some_and(|x| !x.is_empty())
        });
        if let Some(newest) = newest
            && let Some(m) = newest.get("metrics").and_then(Value::as_object)
        {
            for (key, name, unit) in [
                ("ttft", "ap.ttft_ms", "ms"),
                ("itl", "ap.itl_ms", "ms"),
                ("latency", "ap.latency_ms", "ms"),
            ] {
                for q in ["p50", "p99"] {
                    if let Some(v) = wsb::json_num(m.get(&format!("{key}_{q}_ms"))) {
                        let mut labels = IndexMap::new();
                        labels.insert("q".to_owned(), q.to_owned());
                        points.push(MetricPoint {
                            key: name.into(),
                            value: v,
                            unit: unit.into(),
                            labels,
                        });
                    }
                }
            }
            for (mk, name, unit) in [
                ("output_tps", "ap.output_tps", "tok/s"),
                ("request_tps", "ap.request_tps", "1/s"),
                ("error_rate_pct", "ap.error_rate", "%"),
            ] {
                if let Some(v) = wsb::json_num(m.get(mk)) {
                    points.push(MetricPoint {
                        key: name.into(),
                        value: v,
                        unit: unit.into(),
                        labels: IndexMap::new(),
                    });
                }
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Aiperf,
            ts: now,
            available: true,
            points,
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(self.inner.list_runs(limit.min(KEEP_RUNS)))
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        let inner = Arc::clone(&self.inner);
        Box::pin(async_stream::stream! {
            // keyed by (run, verdict): a background run is announced when it starts AND when it finishes
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            let mut first = true;
            loop {
                let mut events = inner.list_runs(50);
                events.sort_by(|a, b| a.ts.total_cmp(&b.ts));
                for ev in events {
                    let key = (wsb::json_get_str(ev.data.get("run_id")), wsb::json_get_str(ev.data.get("verdict")));
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.insert(key);
                    if first && since_ts.is_none_or(|s| ev.ts <= s) {
                        continue;
                    }
                    yield Ok(ev);
                }
                first = false;
                let interval = *lock(&inner.follow_interval);
                wsb::sleep_s(interval).await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        Ok(vec![profiles_spec(self.inner.profiles_path().is_some())])
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        match self.inner.profiles_path() {
            Some(p) if file_id == PROFILES_ID && !p.is_symlink() => Ok(p.clone()),
            _ => Err(AdapterError::UnknownId(file_id.to_owned())),
        }
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.config_path(file_id)?;
        let nf = || AdapterError::FileNotFound(file_id.to_owned());
        let md = std::fs::metadata(&p).map_err(|_| nf())?;
        if md.len() > MAX_PROFILES_BYTES as u64 {
            return Err(nf());
        }
        let bytes = std::fs::read(&p).map_err(|_| nf())?;
        let content = String::from_utf8(bytes).map_err(|_| {
            AdapterError::failed(
                "UnicodeDecodeError",
                "'utf-8' codec can't decode profiles.yaml",
            )
        })?;
        let mtime = std::fs::metadata(&p)
            .ok()
            .and_then(|m| crate::services::config_store::mtime_f64(&m))
            .ok_or_else(nf)?;
        Ok(ConfigFile {
            spec: profiles_spec(true),
            sha256: sha256_hex(content.as_bytes()),
            content,
            mtime,
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        self.config_path(file_id)?;
        let (_, diags) = parse_profiles(content)?;
        Ok(ValidationResult {
            ok: !diags.iter().any(|d| d.severity == Severity::Error),
            validator: "nemo-console aiperf profiles schema".into(),
            authoritative: true,
            diagnostics: diags,
            note: Some(
                "This is the console's own schema, which is the only thing that reads the file (aiperf-local gets plain flags rendered from it). Model aliases are not checked against the router."
                    .into(),
            ),
        })
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        self.config_path(file_id)?;
        let mut r = ApplyResult::new(
            true,
            "nothing to restart: presets are read by the next profile run",
        );
        r.post_check = PostCheck::Healthy;
        Ok(r)
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let (presets, _, _) = self.inner.read_profiles()?;
        let mut names: Vec<String> = presets.keys().cloned().collect();
        names.sort();
        let mut preset = ParamSpec::new(
            "preset",
            "Preset",
            "A preset from the Load profiles config file.",
            ParamType::Enum,
        );
        preset.default = names.first().map_or(Value::Null, |n| json!(n));
        preset.choices = if names.is_empty() { None } else { Some(names) };
        preset.requires_restart = false;
        let mut run_id = ParamSpec::new(
            "run_id",
            "Run",
            "A run id from the runs list; empty = the newest run.",
            ParamType::String,
        );
        run_id.requires_restart = false;
        let mut profile = ActionSpec::new(
            "profile",
            "Run a load profile",
            "Runs `aiperf-local profile` for a preset in the background (ONE at a time): synthetic requests through the router at the preset's concurrency, measuring time to first token, inter-token latency and throughput. disruptive: it loads a live engine and spends router capacity. Results land in the console's state dir (newest 100 runs); 'Show a stored run' reports them.",
            Danger::Disruptive,
        );
        profile.params = vec![preset];
        let mut report = ActionSpec::new(
            "report",
            "Show a stored run",
            "The report of a stored run (newest first): verdict, the load shape and the latency and throughput numbers.",
            Danger::Safe,
        );
        report.params = vec![run_id];
        Ok(vec![profile, report])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        if action_id == "report" {
            let run_id = match params.get("run_id") {
                Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
                _ => String::new(),
            };
            let (ok, text) = self.inner.report_text(&run_id);
            return Ok(done(t0, ok, &text, Vec::new()));
        }
        if action_id != "profile" {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        self.start_profile(params, t0).await
    }
}

/// Build the adapter (Python: registry `build_adapter`; AIPerf has no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(AiPerfAdapter::new(ctx.clone())))
}
