//! NeMo Relay adapter: persistent gateway (systemd user unit), plugin policy, ATOF/ATIF, routes.toml. OWNER: workstream A
//! (PORTING.md section 9). Port of `backend/src/nemo_console/adapters/relay.py`; see that module's docstring for the
//! facts it is built on (the gateway answers /health itself with 404 = UP; liveness never touches /v1/models, which
//! the gateway forwards to LiteLLM; ATOF lines are metadata-only events; rotations may be gzipped).
//!
//! Security: no secret file is ever opened. All file reads are O_NOFOLLOW, regular-file only, size-capped and
//! contained under paths handed in via ctx.paths.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, RunOpts, SystemAdapter,
};
use crate::adapters::switchyard::{int_value, re, type_name};
use crate::api::pyjson::PyJson;
use crate::config::Settings;
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, PlaygroundSpec, PostCheck, Problem,
    Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::sha256_hex;
use crate::services::config_store::mtime_f64;
use crate::services::gzip;
use crate::services::pyfmt::{
    char_len, char_prefix, char_suffix_from, float_repr, json_dumps, str_repr,
};
use crate::services::pytoml::{self, Table, TomlVal};
use crate::services::pyutil::{
    fromisoformat, json_loads_bytes, pj_get, pj_str, pj_truthy, pj_type, py_splitlines,
};
use crate::services::settings::resolve_lenient;

/// `RelayAdapter.label` in Python.
pub const LABEL: &str = "NeMo Relay";

pub const UNIT: &str = "nemo-relay-gateway.service";
pub const DEFAULT_URL: &str = "http://127.0.0.1:4040";
/// The gateway's upstream; ctx.urls["litellm"] overrides it.
pub const DEFAULT_LITELLM_URL: &str = "http://127.0.0.1:4000";
/// Fallback only (the playground reads route ids from clients/nemo-relay/routes.toml when it can).
pub const DEFAULT_ROUTE_IDS: [&str; 8] = [
    "super-lightning",
    "super-glm",
    "super-vl",
    "bolt",
    "direct-lightning",
    "auto-local",
    "direct-27b",
    "direct-flashnext",
];
pub const PLAYGROUND_DEFAULT_ROUTE: &str = "super-lightning";

pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;
/// Max bytes read from the end of an ATOF file per call.
pub const TAIL_WINDOW_BYTES: u64 = 4 * 1024 * 1024;
/// Max bytes consumed per follow poll.
pub const FOLLOW_CHUNK_BYTES: u64 = 1024 * 1024;
/// Longer ATOF lines are skipped, never buffered.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Directory listing bound.
pub const MAX_SCAN_ENTRIES: usize = 50_000;
/// A larger compressed rotation is skipped, not read.
pub const MAX_GZ_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Decompressed bytes read from one .gz rotation (bomb guard); over = skipped.
pub const MAX_GZ_BYTES: u64 = 64 * 1024 * 1024;

static ATOF_NAME: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^.+\.jsonl(\.\d+)?(\.gz)?$"));
static ATOF_ROTATION: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\.(\d+)(?:\.gz)?$"));
static IDENT: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9_.:/@+\-]{1,120}$"));
static PLUGIN_ID: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9_.:/@+\-]{0,119}$"));
static ENV_NAME: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[A-Z_][A-Z0-9_]*$"));
static SEMVER: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"\b(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.]+)?)\b"));
static TS_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?(Z|[+-]\d\d:\d\d)$"));
static TOML_LINE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"line (\d+)"));

static SECRET_SK: LazyLock<regex::Regex> = LazyLock::new(|| re(r"sk-[A-Za-z0-9_\-]{16,}"));
static SECRET_BEARER: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=\-]{8,}"));
#[allow(clippy::expect_used)]
static SECRET_KV: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(
        r#"(?im)^(\s*(?:api_key|apikey|secret|password|token|access_key|private_key)\s*=\s*)(["'])(?!\[REDACTED)[^"'\n]+\2"#,
    )
    .expect("static regex")
});

pub const VALIDATION_CAVEATS: &str = "`nemo-relay plugins validate` and `doctor` can PASS while the gateway still \
     fails to start (e.g. an unsigned plugin under host policy, or a routes.toml the plugin rejects). The \
     authoritative check is a gateway restart.";

fn doctor_caveats() -> [&'static str; 4] {
    [
        "`doctor` line `Upstream openai=unset anthropic=unset` reflects the CLI's own environment, not the gateway \
         unit: ops/nemo-relay-gateway.sh exports NEMO_RELAY_OPENAI_BASE_URL / _ANTHROPIC_BASE_URL.",
        "`plugins list` runtime_state `stopped` is per CLI process; it does not describe the running gateway.",
        "'not enabled by config alone' in doctor refers to desired-state; see `plugins list` for enabled.",
        VALIDATION_CAVEATS,
    ]
}

pub const ROUTES_NOTE: &str = "Validated by a TOML parse plus a semantic pass that MIRRORS the Switchyard \
     schema_version-1 language. `switchyard-server --dry-run` is NOT a valid proxy for the Relay plugin schema \
     (different builds; the server accepts fields such as capable_system_prompt that the plugin rejects), so it is \
     not used. `nemo-relay plugins validate` checks the plugin manifest/policy, not this file. The authoritative check \
     is a gateway restart: applying restarts nemo-relay-gateway.service and the console rolls back automatically if it \
     does not come back healthy.";

// ── small pure helpers ──────────────────────────────────────────────────────────────────────────

/// `re.sub` with a closure over fancy-regex captures.
fn sub_fancy(
    rx: &fancy_regex::Regex,
    text: &str,
    f: impl Fn(&fancy_regex::Captures<'_, str>) -> String,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in rx.captures_iter(text) {
        let Ok(caps) = caps else { break };
        let Some(m) = caps.get(0) else { continue };
        out.push_str(&text[last..m.start()]);
        out.push_str(&f(&caps));
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Redact secret-shaped substrings (defence in depth; the runner also redacts).
#[must_use]
pub fn scrub(text: &str) -> String {
    let out = SECRET_SK.replace_all(text, "[REDACTED]").into_owned();
    let out = SECRET_BEARER
        .replace_all(&out, "Bearer [REDACTED]")
        .into_owned();
    sub_fancy(&SECRET_KV, &out, |c| {
        format!("{}\"[REDACTED]\"", c.get(1).map_or("", |m| m.as_str()))
    })
}

fn ident(v: Option<&PyJson>) -> Option<String> {
    match v {
        Some(PyJson::Str(s)) if IDENT.is_match(s) => Some(s.clone()),
        _ => None,
    }
}

/// `_num`: an int or a float (never a bool).
fn num(v: Option<&PyJson>) -> Option<Value> {
    match v {
        Some(PyJson::Int(i)) => Some(int_value(*i)),
        Some(PyJson::Float(f)) => {
            Some(serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number))
        }
        _ => None,
    }
}

/// Python `str()` of an int/float JSON value.
fn num_str(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(0.0)),
        other => other.to_string(),
    }
}

/// `_parse_ts`: RFC 3339 with Z or an offset only; fractional digits truncated to microseconds.
#[must_use]
pub fn parse_ts(v: Option<&PyJson>) -> Option<f64> {
    let Some(PyJson::Str(s)) = v else { return None };
    let m = TS_RE.captures(s)?;
    let frac: String = m
        .get(2)
        .map_or("0", |g| g.as_str())
        .chars()
        .take(6)
        .collect();
    let frac = format!("{frac:0<6}");
    let tz = if &m[3] == "Z" { "+00:00" } else { &m[3] };
    fromisoformat(&format!("{}.{frac}{tz}", &m[1])).map(|d| d.timestamp())
}

/// A PermissionError / OSError / FileNotFoundError from the file helpers.
#[derive(Debug)]
pub struct FileErr {
    pub class: &'static str,
    pub message: String,
}

impl FileErr {
    fn perm(m: impl Into<String>) -> Self {
        FileErr {
            class: "PermissionError",
            message: m.into(),
        }
    }

    fn from_io(e: &std::io::Error, path: &Path) -> Self {
        let class = match e.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            _ => "OSError",
        };
        let errno = e.raw_os_error().unwrap_or(0);
        let text = e.to_string();
        let msg = text.split(" (os error").next().unwrap_or(&text).to_owned();
        FileErr {
            class,
            message: format!(
                "[Errno {errno}] {msg}: {}",
                str_repr(&path.display().to_string())
            ),
        }
    }

    fn to_adapter(&self) -> AdapterError {
        if self.class == "FileNotFoundError" {
            AdapterError::FileNotFound(self.message.clone())
        } else {
            AdapterError::failed(self.class, self.message.clone())
        }
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Open a regular, non-symlink file read-only (O_NOFOLLOW).
fn open_regular(
    path: &Path,
    cap: Option<u64>,
) -> Result<(std::fs::File, std::fs::Metadata), FileErr> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(nix::libc::ELOOP) {
                FileErr::perm(format!("{} is a symlink", name_of(path)))
            } else {
                FileErr::from_io(&e, path)
            }
        })?;
    let st = f.metadata().map_err(|e| FileErr::from_io(&e, path))?;
    if !st.file_type().is_file() {
        return Err(FileErr::perm(format!(
            "{} is not a regular file",
            name_of(path)
        )));
    }
    if let Some(cap) = cap
        && st.len() > cap
    {
        return Err(FileErr::perm(format!(
            "{} exceeds the {cap} byte read cap",
            name_of(path)
        )));
    }
    Ok((f, st))
}

fn contained(path: &Path, root: &Path) -> bool {
    resolve_lenient(path).starts_with(resolve_lenient(root))
}

/// `read_capped`: contained, no-follow, regular, at most `cap` bytes.
pub fn read_capped(
    path: &Path,
    root: &Path,
    cap: u64,
) -> Result<(Vec<u8>, std::fs::Metadata), FileErr> {
    if !contained(path, root) {
        return Err(FileErr::perm("path escapes its allowed root"));
    }
    let (f, st) = open_regular(path, Some(cap))?;
    let mut buf = Vec::new();
    f.take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| FileErr::from_io(&e, path))?;
    buf.truncate(usize::try_from(cap).unwrap_or(usize::MAX));
    Ok((buf, st))
}

/// Last <= window bytes of a file; second value true when the window starts mid-file.
pub fn tail_bytes(path: &Path, root: &Path, window: u64) -> Result<(Vec<u8>, bool), FileErr> {
    if !contained(path, root) {
        return Err(FileErr::perm("path escapes its allowed root"));
    }
    let (mut f, st) = open_regular(path, None)?;
    let start = st.len().saturating_sub(window);
    f.seek(SeekFrom::Start(start))
        .map_err(|e| FileErr::from_io(&e, path))?;
    let mut buf = Vec::new();
    f.take(window)
        .read_to_end(&mut buf)
        .map_err(|e| FileErr::from_io(&e, path))?;
    Ok((buf, start > 0))
}

/// Why `gz_tail_bytes` failed.
#[derive(Debug)]
pub enum GzTailErr {
    /// Python OSError family: the rotation is skipped.
    Os(FileErr),
    /// `zlib.error` (corrupt deflate data): Python lets it escape.
    Zlib(String),
}

/// Last <= window DECOMPRESSED bytes of a gzipped rotation; same rules as [`tail_bytes`]; a file over
/// `max_file` bytes, or one that decompresses past `max_out`, is refused instead of being read.
pub fn gz_tail_bytes(
    path: &Path,
    root: &Path,
    window: u64,
    max_file: u64,
    max_out: u64,
) -> Result<(Vec<u8>, bool), GzTailErr> {
    if !contained(path, root) {
        return Err(GzTailErr::Os(FileErr::perm(
            "path escapes its allowed root",
        )));
    }
    let (f, _st) = open_regular(path, Some(max_file)).map_err(GzTailErr::Os)?;
    let mut raw = Vec::new();
    f.take(max_file + 1)
        .read_to_end(&mut raw)
        .map_err(|e| GzTailErr::Os(FileErr::from_io(&e, path)))?;
    match gzip::read_tail(&raw, usize::try_from(window).unwrap_or(usize::MAX), max_out) {
        Ok(t) => Ok((t.data, t.total > window)),
        Err(gzip::GzError::TooLarge) => Err(GzTailErr::Os(FileErr::perm(format!(
            "{} decompresses past the {max_out} byte cap",
            name_of(path)
        )))),
        Err(gzip::GzError::Eof | gzip::GzError::BadGzip(_)) => Err(GzTailErr::Os(FileErr {
            class: "OSError",
            message: format!("{}: not a readable gzip file", name_of(path)),
        })),
        Err(gzip::GzError::Zlib(m)) => Err(GzTailErr::Zlib(format!(
            "Error -3 while decompressing data: {m}"
        ))),
    }
}

/// `_human_bytes`.
#[must_use]
pub fn human_bytes(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mut v = n as f64;
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if v < 1024.0 || unit == "GiB" {
            return if unit == "B" {
                format!("{v:.0} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
        v /= 1024.0;
    }
    format!("{n} B")
}

// ── ATOF line -> metadata-only event ────────────────────────────────────────────────────────────

fn usage_fields(usage: Option<&PyJson>, out: &mut Map<String, Value>) {
    let Some(PyJson::Dict(u)) = usage else { return };
    for (src, dst) in [
        ("input_tokens", "input_tokens"),
        ("prompt_tokens", "input_tokens"),
        ("output_tokens", "output_tokens"),
        ("completion_tokens", "output_tokens"),
        ("total_tokens", "total_tokens"),
        ("cache_read_tokens", "cache_read_tokens"),
        ("cache_write_tokens", "cache_write_tokens"),
    ] {
        if let Some(n) = num(u.get(src))
            && !out.contains_key(dst)
        {
            out.insert(dst.to_owned(), n);
        }
    }
}

fn bytes_strip_empty(raw: &[u8]) -> bool {
    raw.iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
}

/// One ATOF JSONL line -> SystemEvent carrying allowlisted METADATA only, or None if unusable.
///
/// Never copies `data.prompt`, `data.content`, `messages`, headers (except the session id) or any string that is not
/// identifier-shaped. `size_bytes` is the size of the original line.
#[must_use]
pub fn parse_atof_line(raw: &[u8]) -> Option<SystemEvent> {
    if bytes_strip_empty(raw) || raw.len() > MAX_LINE_BYTES {
        return None;
    }
    let Some(PyJson::Dict(o)) = json_loads_bytes(raw) else {
        return None;
    };
    let ts = parse_ts(o.get("timestamp"))?;
    let name = ident(o.get("name"))?;
    let md = o.get("metadata");
    let d = o.get("data");
    let mut data = Map::new();
    data.insert(
        "atof_kind".into(),
        ident(o.get("kind")).map_or(Value::Null, Value::String),
    );
    data.insert("name".into(), Value::String(name.clone()));
    data.insert("size_bytes".into(), Value::from(raw.len()));
    for (key, val) in [
        ("phase", o.get("scope_category")),
        ("category", o.get("category")),
        ("uuid", o.get("uuid")),
        ("gateway_path", pj_get(md, "gateway_path")),
        ("status_code", pj_get(md, "otel.status_code")),
        ("severity", pj_get(md, "nemo_relay.log.severity")),
        ("session_id", pj_get(md, "session_id")),
        ("turn_index", pj_get(md, "turn_index")),
    ] {
        if let Some(v) = ident(val) {
            data.insert(key.into(), Value::String(v));
        }
    }
    if !data.contains_key("session_id")
        && let Some(PyJson::Dict(h)) = pj_get(d, "headers")
        && let Some(v) = ident(h.get("x-nemo-relay-session-id"))
    {
        data.insert("session_id".into(), Value::String(v));
    }
    let content = pj_get(d, "content");
    let profile = o.get("category_profile");
    let model = ident(pj_get(d, "model"))
        .or_else(|| ident(pj_get(d, "selected_model")))
        .or_else(|| match content {
            Some(PyJson::Dict(c)) => ident(c.get("model")),
            _ => None,
        })
        .or_else(|| match profile {
            Some(PyJson::Dict(p)) => ident(p.get("model_name")),
            _ => None,
        });
    if let Some(m) = model {
        data.insert("model".into(), Value::String(m));
    }
    let is_err = name == "switchyard.routing.error";
    for key in [
        "served_model",
        "target",
        "target_model",
        "algorithm",
        "outcome",
        "call_role",
        "category",
        "failure_kind",
        "phase",
        "stop_reason",
        "event_type",
        "provider",
    ] {
        if let Some(v) = ident(pj_get(d, key)) {
            let k = if is_err && (key == "category" || key == "phase") {
                format!("error_{key}")
            } else {
                key.to_owned()
            };
            data.insert(k, Value::String(v));
        }
    }
    for key in ["latency_ms", "call_index", "upstream_status", "chunk_index"] {
        if let Some(n) = num(pj_get(d, key)) {
            data.insert(key.into(), n);
        }
    }
    if let Some(PyJson::Bool(b)) = pj_get(d, "fallback_used") {
        data.insert("fallback_used".into(), Value::Bool(*b));
    }
    if let Some(PyJson::List(ch)) = pj_get(d, "choices")
        && let Some(PyJson::Dict(c0)) = ch.first()
        && let Some(v) = ident(c0.get("finish_reason"))
    {
        data.insert("finish_reason".into(), Value::String(v));
    }
    usage_fields(pj_get(d, "usage"), &mut data);
    if let Some(PyJson::List(meas)) = pj_get(d, "measurements") {
        for m in meas.iter().take(8) {
            let PyJson::Dict(m) = m else { continue };
            let val = num(m.get("value"));
            let attrs = m.get("attributes");
            let (Some(val), Some(PyJson::Dict(attrs))) = (val, attrs) else {
                continue;
            };
            let tt = ident(attrs.get("token_type"));
            if let Some(tt) =
                tt.filter(|t| matches!(t.as_str(), "input" | "output" | "reasoning" | "cached"))
            {
                data.insert(format!("{tt}_tokens"), val);
            }
            if let Some(tm) = ident(attrs.get("target_model"))
                && !data.contains_key("model")
            {
                data.insert("model".into(), Value::String(tm));
            }
        }
    }
    let truthy_str = |k: &str| match data.get(k) {
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    };
    let mut bits = vec![name];
    if truthy_str("phase")
        && let Some(Value::String(p)) = data.get("phase")
    {
        bits.push(p.clone());
    }
    if truthy_str("model")
        && let Some(Value::String(m)) = data.get("model")
    {
        bits.push(format!("model={m}"));
    }
    if data.contains_key("input_tokens") || data.contains_key("output_tokens") {
        let g = |k: &str| data.get(k).map_or_else(|| "?".to_owned(), num_str);
        bits.push(format!("tok={}/{}", g("input_tokens"), g("output_tokens")));
    }
    if let Some(l) = data.get("latency_ms") {
        bits.push(format!("{:.0}ms", l.as_f64().unwrap_or(0.0)));
    }
    if let Some(u) = data.get("upstream_status") {
        bits.push(format!("upstream={}", num_str(u)));
    }
    let summary = char_prefix(&bits.join(" "), 200).to_owned();
    Some(SystemEvent {
        ts,
        system: SystemId::Relay,
        kind: "relay.atof".into(),
        summary,
        data,
    })
}

// ── routes.toml semantic validation (mirrors the Switchyard schema_version-1 language) ──────────

const CLIENT_FIELDS: [&str; 5] = [
    "format",
    "base_url",
    "api_key_env",
    "forward_auth",
    "max_retries",
];
const TARGET_FIELDS: [&str; 3] = ["id", "llm_client", "system_prompt"];
const ROUTE_FIELDS: [&str; 13] = [
    "id",
    "type",
    "capable_target",
    "efficient_target",
    "picker",
    "confidence_threshold",
    "recent_turn_window",
    "context_window",
    "tool_calling",
    "reasoning",
    "handoff_notes",
    "classifier",
    "target",
];
const CLASSIFIER_FIELDS: [&str; 3] = ["target", "base_threshold", "max_output_tokens"];
const PLUGIN_REJECTED: [&str; 2] = ["capable_system_prompt", "efficient_system_prompt"];
const FORMATS: [&str; 2] = ["anthropic_messages", "openai_chat"];
const ROUTE_TYPES: [&str; 2] = ["passthrough", "stage_router"];

static URL_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^https?://([^/?#]*)(?:[/?#][^\s\\]*)?$"));
static HOST_PORT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^([^:@\[\]]+)(?::(\d{1,5}))?$"));
static V6_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^\[([0-9A-Fa-f:.]+)\](?::(\d{1,5}))?$"));
static SERVICE_LABEL: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9_-]*$"));

/// `ipaddress.IPv4Address(s)` (strict dotted decimal, no leading zeros).
fn ipv4(s: &str) -> Option<std::net::Ipv4Addr> {
    match crate::adapters::switchyard::ip_address(s) {
        Some(std::net::IpAddr::V4(v)) if !s.contains('%') => Some(v),
        _ => None,
    }
}

/// None when the URL may only reach this box (loopback or a bare single-label docker service name).
///
/// Rejects userinfo entirely (`http://127.0.0.1@evil`), whitespace/backslashes, non-loopback IPs and any dotted
/// hostname, so a routes.toml cannot send prompts or the LiteLLM credential off-box. One trailing dot is tolerated.
#[must_use]
pub fn base_url_problem(url: &str) -> Option<String> {
    if url.chars().any(|c| (c as u32) < 33 || (c as u32) > 126) || url.contains('\\') {
        return Some("contains whitespace, control or non-ASCII characters".into());
    }
    let Some(m) = URL_RE.captures(url) else {
        return Some("must be an http(s) URL".into());
    };
    let auth = m.get(1).map_or("", |g| g.as_str());
    if auth.contains('@') {
        return Some("must not contain userinfo (user@host)".into());
    }
    let (host_ok, port) = if let Some(v6) = V6_RE.captures(auth) {
        let Ok(a) = v6[1].parse::<std::net::Ipv6Addr>() else {
            return Some("has an invalid IPv6 address".into());
        };
        (a.is_loopback(), v6.get(2).map(|g| g.as_str().to_owned()))
    } else {
        let Some(hp) = HOST_PORT_RE.captures(auth) else {
            return Some("has an unparseable host".into());
        };
        let host = hp[1].to_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
        let ok = match ipv4(&host) {
            Some(a) => a.is_loopback(),
            None => host == "localhost" || SERVICE_LABEL.is_match(&host),
        };
        (ok, hp.get(2).map(|g| g.as_str().to_owned()))
    };
    if let Some(p) = port
        && p.parse::<u32>().unwrap_or(u32::MAX) > 65535
    {
        return Some("has an out-of-range port".into());
    }
    if !host_ok {
        return Some(
            "points off-box: only loopback (127.0.0.0/8, ::1, localhost) or a bare single-label service name (e.g. \
             litellm) is allowed, otherwise prompts and the LiteLLM credential would leave this machine"
                .into(),
        );
    }
    None
}

fn line_of(lines: &[&str], header: &str) -> Option<i64> {
    let pat = re(&format!(r"^\s*\[{}\]\s*(#.*)?$", regex::escape(header)));
    lines
        .iter()
        .position(|ln| pat.is_match(ln))
        .map(|i| i64::try_from(i + 1).unwrap_or(0))
}

/// `TypeError: unhashable type` for a value used as a set member / dict key.
fn hashable(v: Option<&TomlVal>) -> Result<(), AdapterError> {
    match v {
        Some(t @ (TomlVal::Array(_) | TomlVal::Table(_))) => Err(AdapterError::failed(
            "TypeError",
            format!("unhashable type: '{}'", type_name(t)),
        )),
        _ => Ok(()),
    }
}

fn repr_opt(v: Option<&TomlVal>) -> String {
    v.map_or_else(|| "None".to_owned(), TomlVal::repr)
}

fn is_int(v: &TomlVal) -> bool {
    matches!(v, TomlVal::Int(_) | TomlVal::BigInt(_))
}

/// Python `v == 1` for a TOML value (1, 1.0 and True all compare equal to 1).
fn eq_one(v: Option<&TomlVal>) -> bool {
    match v {
        Some(TomlVal::Int(i)) => *i == 1,
        Some(TomlVal::Float(f)) => *f == 1.0,
        Some(TomlVal::Bool(b)) => *b,
        _ => false,
    }
}

/// `validate_routes_semantics(text)`; an `Err` is an exception the Python lets escape (unhashable values).
pub fn validate_routes_semantics(text: &str) -> Result<Vec<Diagnostic>, AdapterError> {
    let lines = py_splitlines(text);
    let data = match pytoml::loads(text) {
        Ok(d) => d,
        Err(e) => {
            let line = TOML_LINE
                .captures(&e.0)
                .and_then(|c| c[1].parse::<i64>().ok());
            return Ok(vec![Diagnostic {
                severity: Severity::Error,
                message: format!("TOML syntax error: {e}"),
                line,
            }]);
        }
    };
    let mut diags: Vec<Diagnostic> = Vec::new();
    let push = |diags: &mut Vec<Diagnostic>, sev: Severity, msg: String, header: Option<&str>| {
        diags.push(Diagnostic {
            severity: sev,
            message: msg,
            line: header.and_then(|h| line_of(&lines, h)),
        });
    };
    if !eq_one(data.get("schema_version")) {
        push(
            &mut diags,
            Severity::Error,
            format!(
                "schema_version must be 1 (got {})",
                repr_opt(data.get("schema_version"))
            ),
            None,
        );
    }
    for k in data.keys() {
        if !["schema_version", "llm_clients", "targets", "routes"].contains(&k.as_str()) {
            push(
                &mut diags,
                Severity::Warn,
                format!(
                    "unknown top-level key {}; the plugin rejects unknown fields",
                    str_repr(k)
                ),
                None,
            );
        }
    }
    let table = |key: &str, diags: &mut Vec<Diagnostic>| -> IndexMap<String, Table> {
        match data.get(key) {
            None => IndexMap::new(),
            Some(TomlVal::Table(t)) if t.values().all(|x| matches!(x, TomlVal::Table(_))) => t
                .iter()
                .filter_map(|(k, v)| v.as_table().map(|t| (k.clone(), t.clone())))
                .collect(),
            Some(_) => {
                diags.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!("[{key}] must be a table of tables"),
                    line: None,
                });
                IndexMap::new()
            }
        }
    };
    let clients = table("llm_clients", &mut diags);
    let targets = table("targets", &mut diags);
    let routes = table("routes", &mut diags);
    if routes.is_empty() {
        push(
            &mut diags,
            Severity::Error,
            "no [routes.*] defined; the gateway would have nothing to route".into(),
            None,
        );
    }
    let fmts = "['anthropic_messages', 'openai_chat']";
    for (cname, c) in &clients {
        let h = format!("llm_clients.{cname}");
        hashable(c.get("format"))?;
        if !matches!(c.get("format"), Some(TomlVal::Str(f)) if FORMATS.contains(&f.as_str())) {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: format must be one of {fmts}"),
                Some(&h),
            );
        }
        match c.get("base_url") {
            Some(TomlVal::Str(bu)) => {
                if let Some(bad) = base_url_problem(bu) {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: base_url {bad}"),
                        Some(&h),
                    );
                }
            }
            _ => push(
                &mut diags,
                Severity::Error,
                format!("{h}: base_url must be an http(s) URL"),
                Some(&h),
            ),
        }
        if c.contains_key("api_key") || c.contains_key("apikey") {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: literal API keys are not allowed; use api_key_env = \"ENV_NAME\""),
                Some(&h),
            );
        }
        if let Some(ake) = c.get("api_key_env")
            && !matches!(ake, TomlVal::Str(s) if ENV_NAME.is_match(s))
        {
            push(
                &mut diags,
                Severity::Error,
                format!(
                    "{h}: api_key_env must be an environment variable NAME (e.g. LITELLM_MASTER_KEY), never a value"
                ),
                Some(&h),
            );
        }
        if let Some(mr) = c.get("max_retries") {
            let ok = match mr {
                TomlVal::Int(i) => *i >= 0,
                TomlVal::BigInt(s) => !s.starts_with('-'),
                _ => false,
            };
            if !ok {
                push(
                    &mut diags,
                    Severity::Error,
                    format!("{h}: max_retries must be a non-negative integer"),
                    Some(&h),
                );
            }
        }
        for k in c.keys() {
            if !CLIENT_FIELDS.contains(&k.as_str()) && k != "api_key" && k != "apikey" {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: unknown field {}; the plugin rejects unknown fields",
                        str_repr(k)
                    ),
                    Some(&h),
                );
            }
        }
    }
    let mut target_identity: IndexMap<(String, String), String> = IndexMap::new();
    for (tname, t) in &targets {
        let h = format!("targets.{tname}");
        let (tid, cl) = (t.get("id"), t.get("llm_client"));
        if !matches!(tid, Some(TomlVal::Str(s)) if !s.is_empty()) {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: id must be a non-empty string"),
                Some(&h),
            );
        }
        if !matches!(cl, Some(TomlVal::Str(s)) if clients.contains_key(s)) {
            push(
                &mut diags,
                Severity::Error,
                format!(
                    "{h}: llm_client {} is not defined under [llm_clients]",
                    repr_opt(cl)
                ),
                Some(&h),
            );
        }
        if let (Some(TomlVal::Str(tid)), Some(TomlVal::Str(cl))) = (tid, cl) {
            let key = (tid.clone(), cl.clone());
            if let Some(prev) = target_identity.get(&key) {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: same (id, llm_client) as targets.{prev}; the plugin silently DROPS one with only a WARN"
                    ),
                    Some(&h),
                );
            }
            target_identity.insert(key, tname.clone());
        }
        for k in t.keys() {
            if !TARGET_FIELDS.contains(&k.as_str()) {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: unknown field {}; the plugin rejects unknown fields",
                        str_repr(k)
                    ),
                    Some(&h),
                );
            }
        }
    }
    // fmt_of(tname)
    let fmt_of = |tname: &str| -> Result<Option<String>, AdapterError> {
        let Some(t) = targets.get(tname) else {
            return Ok(None);
        };
        let lc = t.get("llm_client");
        hashable(lc)?;
        let c = match lc {
            None => clients.get(""),
            Some(TomlVal::Str(s)) => clients.get(s),
            Some(_) => None,
        };
        Ok(match c.and_then(|c| c.get("format")) {
            Some(TomlVal::Str(f)) => Some(f.clone()),
            _ => None,
        })
    };
    let types = "['passthrough', 'stage_router']";
    let mut route_ids: IndexMap<String, String> = IndexMap::new();
    for (rname, r) in &routes {
        let h = format!("routes.{rname}");
        let (rid, rtype) = (r.get("id"), r.get("type"));
        match rid {
            Some(TomlVal::Str(s)) if !s.is_empty() => {
                if let Some(prev) = route_ids.get(s) {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: route id {} duplicates routes.{prev}", str_repr(s)),
                        Some(&h),
                    );
                } else {
                    route_ids.insert(s.clone(), rname.clone());
                }
            }
            _ => push(
                &mut diags,
                Severity::Error,
                format!("{h}: id must be a non-empty string"),
                Some(&h),
            ),
        }
        hashable(rtype)?;
        let rtype_s = match rtype {
            Some(TomlVal::Str(s)) => Some(s.as_str()),
            _ => None,
        };
        if !rtype_s.is_some_and(|t| ROUTE_TYPES.contains(&t)) {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: type must be one of {types}"),
                Some(&h),
            );
        }
        // Python iterates `_PLUGIN_REJECTED & set(r)` (set order); sorted here, see the workstream A deviations
        for bad in PLUGIN_REJECTED {
            if r.contains_key(bad) {
                push(
                    &mut diags,
                    Severity::Error,
                    format!(
                        "{h}: {bad} is rejected by the Relay plugin ('unknown field') although switchyard-server \
                         0.2.0 accepts it"
                    ),
                    Some(&h),
                );
            }
        }
        for k in r.keys() {
            if !ROUTE_FIELDS.contains(&k.as_str()) && !PLUGIN_REJECTED.contains(&k.as_str()) {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: unknown field {}; the plugin rejects unknown fields",
                        str_repr(k)
                    ),
                    Some(&h),
                );
            }
        }
        let mut used: Vec<String> = Vec::new();
        let need = |field: &str, diags: &mut Vec<Diagnostic>| -> Option<String> {
            let v = r.get(field);
            match v {
                Some(TomlVal::Str(s)) if targets.contains_key(s) => Some(s.clone()),
                _ => {
                    diags.push(Diagnostic {
                        severity: Severity::Error,
                        message: format!(
                            "{h}: {field} {} is not defined under [targets]",
                            repr_opt(v)
                        ),
                        line: line_of(&lines, &h),
                    });
                    None
                }
            }
        };
        if rtype_s == Some("passthrough") {
            if let Some(tv) = need("target", &mut diags) {
                used.push(tv);
            }
        } else if rtype_s == Some("stage_router") {
            for f in ["capable_target", "efficient_target"] {
                if let Some(tv) = need(f, &mut diags) {
                    used.push(tv);
                }
            }
            if let Some(thr) = r.get("confidence_threshold") {
                let ok = match thr {
                    TomlVal::Int(i) => (0..=1).contains(i),
                    TomlVal::Float(f) => (0.0..=1.0).contains(f),
                    _ => false,
                };
                if !ok {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: confidence_threshold must be a number between 0 and 1"),
                        Some(&h),
                    );
                }
            }
            for f in ["recent_turn_window", "context_window"] {
                if let Some(v) = r.get(f) {
                    let ok = match v {
                        TomlVal::Int(i) => *i > 0,
                        TomlVal::BigInt(s) => !s.starts_with('-'),
                        _ => false,
                    };
                    if !ok || !is_int(v) {
                        push(
                            &mut diags,
                            Severity::Error,
                            format!("{h}: {f} must be a positive integer"),
                            Some(&h),
                        );
                    }
                }
            }
            for f in ["tool_calling", "reasoning"] {
                if let Some(v) = r.get(f)
                    && !matches!(v, TomlVal::Bool(_))
                {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: {f} must be a boolean"),
                        Some(&h),
                    );
                }
            }
        }
        if let Some(cl) = r.get("classifier") {
            let ch = format!("routes.{rname}.classifier");
            match cl {
                TomlVal::Table(cl) => {
                    let tv = cl.get("target");
                    match tv {
                        Some(TomlVal::Str(s)) if targets.contains_key(s) => used.push(s.clone()),
                        _ => push(
                            &mut diags,
                            Severity::Error,
                            format!(
                                "{ch}: target {} is not defined under [targets]",
                                repr_opt(tv)
                            ),
                            Some(&ch),
                        ),
                    }
                    for k in cl.keys() {
                        if !CLASSIFIER_FIELDS.contains(&k.as_str()) {
                            push(
                                &mut diags,
                                Severity::Warn,
                                format!(
                                    "{ch}: unknown field {}; the plugin rejects unknown fields",
                                    str_repr(k)
                                ),
                                Some(&ch),
                            );
                        }
                    }
                }
                _ => push(
                    &mut diags,
                    Severity::Error,
                    format!("{ch} must be a table"),
                    Some(&ch),
                ),
            }
        }
        // Switchyard 0.3.0 plugin refuses a route that holds one model id on two wire formats.
        let mut by_model: IndexMap<String, Vec<String>> = IndexMap::new();
        for tn in &used {
            let fm = fmt_of(tn)?;
            let tid = targets.get(tn).and_then(|t| t.get("id"));
            if let (Some(fm), Some(TomlVal::Str(tid))) = (fm, tid) {
                let e = by_model.entry(tid.clone()).or_default();
                if !e.contains(&fm) {
                    e.push(fm);
                }
            }
        }
        for (tid, mut fs) in by_model {
            if fs.len() > 1 {
                fs.sort();
                push(
                    &mut diags,
                    Severity::Error,
                    format!(
                        "{h}: model id {} is used on two wire formats ({}); the 0.3.0 plugin refuses this. Give the \
                         second use its own alias (see the nemotron-lightning-judge alias)",
                        str_repr(&tid),
                        fs.join(", ")
                    ),
                    Some(&h),
                );
            }
        }
    }
    Ok(diags)
}

// ── plugins.toml summary (no raw content; public keys are only counted) ─────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginsSummary {
    pub parsed: bool,
    pub dynamic_manifests: usize,
    pub attestation: Option<String>,
    pub trusted_keys: usize,
    pub atof: bool,
    pub atif: bool,
    pub otel: bool,
    pub pii_redaction: bool,
    pub observability_component: bool,
    pub atof_dir: Option<String>,
    pub error: Option<String>,
}

impl PluginsSummary {
    #[must_use]
    pub fn exporters_enabled(&self) -> usize {
        usize::from(self.atof) + usize::from(self.atif) + usize::from(self.otel)
    }
}

/// `summarize_plugins_toml(text)`.
#[must_use]
pub fn summarize_plugins_toml(text: &str) -> PluginsSummary {
    let d = match pytoml::loads(text) {
        Ok(d) => d,
        Err(e) => {
            return PluginsSummary {
                parsed: false,
                error: Some(format!(
                    "plugins.toml does not parse: {}",
                    char_prefix(&e.0, 160)
                )),
                ..PluginsSummary::default()
            };
        }
    };
    let empty = Table::new();
    let tbl = |v: Option<&TomlVal>| -> Table {
        v.and_then(TomlVal::as_table).cloned().unwrap_or_default()
    };
    let plugins = tbl(d.get("plugins"));
    let dyn_ = plugins.get("dynamic");
    let policy = tbl(plugins.get("policy"));
    let defaults = tbl(policy.get("defaults"));
    let att = defaults.get("attestation");
    let keys = defaults.get("trusted_public_keys");
    let mut s = PluginsSummary {
        parsed: true,
        ..PluginsSummary::default()
    };
    if let Some(TomlVal::Array(comps)) = d.get("components") {
        for c in comps {
            let TomlVal::Table(c) = c else { continue };
            if matches!(c.get("enabled"), Some(TomlVal::Bool(false))) {
                continue;
            }
            let cfg = c
                .get("config")
                .and_then(TomlVal::as_table)
                .unwrap_or(&empty);
            match c.get("kind").and_then(TomlVal::as_str) {
                Some("pii_redaction") => s.pii_redaction = true,
                Some("observability") => {
                    s.observability_component = true;
                    let on = |key: &str| {
                        matches!(cfg.get(key), Some(TomlVal::Table(sub))
                            if matches!(sub.get("enabled"), Some(TomlVal::Bool(true))))
                    };
                    s.atof = on("atof");
                    s.atif = on("atif");
                    s.otel = on("opentelemetry");
                    let atof = cfg
                        .get("atof")
                        .and_then(TomlVal::as_table)
                        .unwrap_or(&empty);
                    if let Some(TomlVal::Array(sinks)) = atof.get("sinks")
                        && let Some(TomlVal::Table(s0)) = sinks.first()
                    {
                        s.atof_dir = s0
                            .get("output_directory")
                            .and_then(TomlVal::as_str)
                            .map(str::to_owned);
                    }
                }
                _ => {}
            }
        }
    }
    s.dynamic_manifests = match dyn_ {
        Some(TomlVal::Array(a)) => a.len(),
        _ => 0,
    };
    s.attestation = att.and_then(TomlVal::as_str).map(str::to_owned);
    s.trusted_keys = match keys {
        Some(TomlVal::Array(a)) => a.len(),
        _ => 0,
    };
    s
}

/// An HTTP liveness probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub reachable: bool,
    pub status: Option<u16>,
    pub detail: String,
}

/// `systemctl show` of the unit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnitState {
    pub known: bool,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub pid: u64,
    pub restarts: u64,
    pub result: String,
    pub enabled: String,
    pub active_since: Option<f64>,
}

/// `parse_systemctl_show(text)`.
#[must_use]
pub fn parse_systemctl_show(text: &str) -> UnitState {
    let mut kv: IndexMap<String, String> = IndexMap::new();
    for ln in py_splitlines(text) {
        if let Some((k, v)) = ln.split_once('=') {
            kv.insert(
                crate::services::pyutil::py_strip(k).to_owned(),
                crate::services::pyutil::py_strip(v).to_owned(),
            );
        }
    }
    let Some(active) = kv.get("ActiveState") else {
        return UnitState::default();
    };
    let since = kv
        .get("ActiveEnterTimestamp")
        .and_then(|t| t.strip_prefix('@'))
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<f64>().ok());
    let int = |k: &str| -> u64 {
        kv.get(k)
            .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let g = |k: &str| kv.get(k).cloned().unwrap_or_default();
    UnitState {
        known: true,
        load: g("LoadState"),
        active: active.clone(),
        sub: g("SubState"),
        pid: int("MainPID"),
        restarts: int("NRestarts"),
        result: g("Result"),
        enabled: g("UnitFileState"),
        active_since: since,
    }
}

pub const SHOW_PROPS: &str =
    "ActiveEnterTimestamp,ActiveState,SubState,MainPID,NRestarts,Result,LoadState,UnitFileState";

/// The read position of one follow_events iterator (`state` dict in Python).
#[derive(Debug, Clone, Default)]
pub struct FollowState {
    pub ino: Option<u64>,
    pub offset: u64,
    pub rest: Vec<u8>,
}

type Row = IndexMap<String, PyJson>;

#[derive(Default)]
struct Cache {
    version: Option<(Instant, Option<String>)>,
    plugins: Option<(Instant, Option<Vec<Row>>)>,
}

pub struct RelayAdapter {
    pub ctx: AdapterContext,
    /// systemd --user unit (addendum 9.11: the container sets `relay_unit`)
    pub unit: String,
    /// health poll cadence after a restart (tests set 0)
    pub poll_interval: f64,
    /// consecutive good polls required (config errors surface AFTER the banner)
    pub settle_checks: u32,
    pub restart_timeout: f64,
    pub follow_interval: f64,
    /// The module caps Python tests monkeypatch.
    pub max_gz_file_bytes: u64,
    pub max_gz_bytes: u64,
    cache: Mutex<Cache>,
}

/// Build the adapter (Python: registry `build_adapter` + the container's configure hook).
pub fn build(ctx: &AdapterContext, cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(RelayAdapter::from_settings(ctx, cfg)))
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn files() -> [(&'static str, &'static str); 3] {
    [
        ("routes", "relay_routes"),
        ("plugins", "relay_plugins"),
        ("config", "relay_config"),
    ]
}

fn ms(t0: Instant) -> i64 {
    i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX)
}

impl RelayAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        RelayAdapter {
            ctx,
            unit: UNIT.to_owned(),
            poll_interval: 1.0,
            settle_checks: 3,
            restart_timeout: 30.0,
            follow_interval: 1.0,
            max_gz_file_bytes: MAX_GZ_FILE_BYTES,
            max_gz_bytes: MAX_GZ_BYTES,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// `build_adapter` + `container._configure_relay`.
    #[must_use]
    pub fn from_settings(ctx: &AdapterContext, cfg: &Settings) -> Self {
        let mut a = RelayAdapter::new(ctx.clone());
        a.unit.clone_from(&cfg.relay_unit);
        a
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        match self.cache.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    // ── paths / urls ────────────────────────────────────────────────────────────────────────
    fn base(&self) -> String {
        self.ctx
            .urls
            .get("relay")
            .map_or(DEFAULT_URL, String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn litellm(&self) -> String {
        self.ctx
            .urls
            .get("litellm")
            .map_or(DEFAULT_LITELLM_URL, String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn path(&self, key: &str) -> Result<PathBuf, AdapterError> {
        self.ctx.paths.get(key).cloned().ok_or_else(|| {
            AdapterError::UnknownId(format!("ctx.paths is missing {}", str_repr(key)))
        })
    }

    fn read_file(&self, file_id: &str) -> Result<(Vec<u8>, std::fs::Metadata), AdapterError> {
        let p = self.config_path(file_id)?;
        let root = p.parent().map_or_else(|| p.clone(), Path::to_path_buf);
        read_capped(&p, &root, MAX_CONFIG_BYTES).map_err(|e| e.to_adapter())
    }

    // ── runner helper (failure is data) ─────────────────────────────────────────────────────
    async fn run(&self, a: &[&str], timeout: f64, max_bytes: usize) -> CommandResult {
        match self
            .ctx
            .runner
            .run(&argv(a), RunOpts::timeout(timeout).max_bytes(max_bytes))
            .await
        {
            Ok(r) => r,
            Err(e) => CommandResult::new(
                127,
                "",
                &format!("{}: {}", e.py_class(), char_prefix(&e.to_string(), 200)),
            ),
        }
    }

    /// `_unit()`: `systemctl --user show` of the configured unit.
    pub async fn unit_state(&self) -> UnitState {
        let r = self
            .run(
                &[
                    "systemctl",
                    "--user",
                    "--timestamp=unix",
                    "show",
                    &self.unit,
                    "-p",
                    SHOW_PROPS,
                ],
                8.0,
                256_000,
            )
            .await;
        if r.returncode == 0 {
            parse_systemctl_show(&r.stdout)
        } else {
            UnitState::default()
        }
    }

    async fn get_probe(&self, url: &str) -> Probe {
        match self.ctx.http.get(url, 3.0).await {
            Ok(r) => Probe {
                reachable: true,
                status: Some(r.status),
                detail: format!("HTTP {}", r.status),
            },
            Err(e) => Probe {
                reachable: false,
                status: None,
                detail: e.class.to_owned(),
            },
        }
    }

    /// The gateway answers /health itself (404): any answer below 500 means it is up. Never /v1/models, which the
    /// gateway forwards to LiteLLM.
    async fn probe(&self) -> Probe {
        self.get_probe(&format!("{}/health", self.base())).await
    }

    /// LiteLLM's unauthenticated liveness route: not a model route, so it leaves no Langfuse trace.
    async fn upstream(&self) -> Probe {
        self.get_probe(&format!("{}/health/liveliness", self.litellm()))
            .await
    }

    async fn version(&self) -> Option<String> {
        if let Some((at, v)) = &self.cache().version
            && at.elapsed() < Duration::from_secs(300)
        {
            return v.clone();
        }
        let now = Instant::now();
        let r = self.run(&["nemo-relay", "--version"], 8.0, 4096).await;
        let v = if r.returncode == 0 {
            SEMVER.captures(&r.stdout).map(|c| c[1].to_owned())
        } else {
            None
        };
        self.cache().version = Some((now, v.clone()));
        v
    }

    async fn plugins_list(&self) -> Option<Vec<Row>> {
        if let Some((at, v)) = &self.cache().plugins
            && at.elapsed() < Duration::from_secs(20)
        {
            return v.clone();
        }
        let now = Instant::now();
        let r = self
            .run(&["nemo-relay", "plugins", "list", "--json"], 15.0, 256_000)
            .await;
        let v = if r.returncode == 0 {
            match json_loads_bytes(r.stdout.as_bytes()) {
                Some(PyJson::Dict(d)) => match d.get("data") {
                    Some(PyJson::List(rows)) => Some(
                        rows.iter()
                            .filter_map(|x| match x {
                                PyJson::Dict(m) => Some(m.clone()),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        self.cache().plugins = Some((now, v.clone()));
        v
    }

    fn plugins_summary(&self) -> (PluginsSummary, Option<f64>) {
        match self.read_file("plugins") {
            Err(e) => {
                let class = match &e {
                    AdapterError::UnknownId(_) => "KeyError",
                    other => other.py_class(),
                };
                (
                    PluginsSummary {
                        parsed: false,
                        error: Some(format!("cannot read plugins.toml: {class}")),
                        ..PluginsSummary::default()
                    },
                    None,
                )
            }
            Ok((raw, st)) => (
                summarize_plugins_toml(&String::from_utf8_lossy(&raw)),
                mtime_f64(&st),
            ),
        }
    }

    /// (count, bytes) of regular non-symlink files in a fixed dir, or None when unreadable.
    fn scan_dir(&self, key: &str, pattern: Option<&regex::Regex>) -> Option<(u64, u64)> {
        let root = self.path(key).ok()?;
        let it = std::fs::read_dir(&root).ok()?;
        let (mut n, mut size) = (0u64, 0u64);
        for (i, e) in it.enumerate() {
            if i >= MAX_SCAN_ENTRIES {
                break;
            }
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            if pattern.is_some_and(|p| !p.is_match(&name)) {
                continue;
            }
            let Ok(ft) = e.file_type() else { return None };
            if ft.is_symlink() || !ft.is_file() {
                continue;
            }
            n += 1;
            size += e.metadata().ok()?.len();
        }
        Some((n, size))
    }

    fn plugin_row(plist: Option<&Vec<Row>>) -> Option<&Row> {
        let plist = plist?;
        if plist.is_empty() {
            return None;
        }
        plist
            .iter()
            .find(|r| {
                matches!(r.get("enabled"), Some(PyJson::Bool(true)))
                    && !pj_truthy(r.get("tombstoned"))
            })
            .or_else(|| plist.first())
    }

    // ── events (ATOF) ───────────────────────────────────────────────────────────────────────
    /// ATOF files of the fixed dir, newest rotation first (`_atof_files_newest_first`).
    pub fn atof_files_newest_first(&self) -> Result<Vec<PathBuf>, AdapterError> {
        let root = self.path("relay_atof_dir")?;
        let it = std::fs::read_dir(&root).map_err(|e| FileErr::from_io(&e, &root).to_adapter())?;
        let mut found: Vec<(u64, String, PathBuf)> = Vec::new();
        for (i, e) in it.enumerate() {
            if i >= MAX_SCAN_ENTRIES {
                break;
            }
            let Ok(e) = e else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if !ATOF_NAME.is_match(&name) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() || !ft.is_file() {
                continue;
            }
            let rot = ATOF_ROTATION
                .captures(&name)
                .and_then(|c| c[1].parse::<u64>().ok())
                .unwrap_or(0);
            found.push((rot, name, e.path()));
        }
        found.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Ok(found.into_iter().map(|(_, _, p)| p).collect())
    }

    fn read_recent(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let root = self.path("relay_atof_dir")?;
        let mut need = limit;
        let mut collected: Vec<Vec<SystemEvent>> = Vec::new();
        for p in self.atof_files_newest_first()?.into_iter().take(4) {
            let read = if name_of(&p).ends_with(".gz") {
                match gz_tail_bytes(
                    &p,
                    &root,
                    TAIL_WINDOW_BYTES,
                    self.max_gz_file_bytes,
                    self.max_gz_bytes,
                ) {
                    Ok(x) => Ok(x),
                    Err(GzTailErr::Os(e)) => Err(e),
                    Err(GzTailErr::Zlib(m)) => return Err(AdapterError::failed("error", m)),
                }
            } else {
                tail_bytes(&p, &root, TAIL_WINDOW_BYTES)
            };
            let Ok((buf, partial)) = read else { continue };
            let mut lines: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
            if partial && !lines.is_empty() {
                lines.remove(0); // first line may be cut mid-record
            }
            let evs: Vec<SystemEvent> = lines
                .iter()
                .filter_map(|ln| parse_atof_line(ln))
                .filter(|e| e.data.get("name").and_then(Value::as_str) != Some("llm.chunk"))
                .collect();
            if !evs.is_empty() {
                let take = evs.len().min(need);
                let chunk: Vec<SystemEvent> = evs[evs.len() - take..].to_vec();
                need -= chunk.len();
                collected.push(chunk);
            }
            if need == 0 {
                break;
            }
        }
        let mut out: Vec<SystemEvent> = collected.into_iter().rev().flatten().collect();
        if out.len() > limit {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }

    /// Read whatever was appended since the last poll (`_follow_step`).
    pub fn follow_step(&self, state: &mut FollowState) -> Result<Vec<SystemEvent>, AdapterError> {
        let root = self.path("relay_atof_dir")?;
        let files: Vec<PathBuf> = self
            .atof_files_newest_first()?
            .into_iter()
            .filter(|f| !name_of(f).ends_with(".gz"))
            .collect();
        let Some(active) = files.first() else {
            return Ok(Vec::new());
        };
        if !contained(active, &root) {
            return Ok(Vec::new());
        }
        let (mut f, st) = open_regular(active, None).map_err(|e| e.to_adapter())?;
        let ino = st.ino();
        if state.ino != Some(ino) {
            state.offset = if state.ino.is_some() { 0 } else { st.len() };
            state.ino = Some(ino);
            state.rest.clear();
        } else if st.len() < state.offset {
            state.offset = 0; // truncated in place
            state.rest.clear();
        }
        if st.len() <= state.offset {
            return Ok(Vec::new());
        }
        f.seek(SeekFrom::Start(state.offset))
            .map_err(|e| FileErr::from_io(&e, active).to_adapter())?;
        let mut chunk = Vec::new();
        f.take(FOLLOW_CHUNK_BYTES.min(st.len() - state.offset))
            .read_to_end(&mut chunk)
            .map_err(|e| FileErr::from_io(&e, active).to_adapter())?;
        state.offset += chunk.len() as u64;
        let mut buf = std::mem::take(&mut state.rest);
        buf.extend_from_slice(&chunk);
        let mut parts: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
        let rest = parts.pop().unwrap_or_default();
        let evs = parts
            .iter()
            .filter_map(|ln| parse_atof_line(ln))
            .filter(|e| e.data.get("name").and_then(Value::as_str) != Some("llm.chunk"))
            .collect();
        state.rest = if rest.len() < MAX_LINE_BYTES {
            rest.to_vec()
        } else {
            Vec::new()
        };
        Ok(evs)
    }

    // ── configure ───────────────────────────────────────────────────────────────────────────
    fn specs(&self) -> Vec<ConfigFileSpec> {
        let policy_why = "Carries the plugin signature-enforcement policy (attestation mode and trusted_public_keys). \
                          Local security policy: signature enforcement settings are a human decision, so in v1 the \
                          console shows a proposed diff only and never writes this file.";
        vec![
            ConfigFileSpec {
                id: "routes".into(),
                label: "Switchyard routes (Relay plugin)".into(),
                description: "schema_version-1 Switchyard deployment loaded by the Relay native plugin (host-side \
                              copy; clients here speak anthropic_messages, the judge speaks openai_chat)."
                    .into(),
                display_path: "clients/nemo-relay/routes.toml".into(),
                format: "toml".into(),
                editable: true,
                why_readonly: None,
                validators: vec!["TOML parse".into(), "Switchyard schema mirror (semantic)".into()],
                authoritative: false,
                apply_effect: format!(
                    "restarts {}; ~5-10 s of dropped requests on :4040; auto-rolled back if the gateway is not healthy",
                    self.unit
                ),
            },
            ConfigFileSpec {
                id: "plugins".into(),
                label: "Relay plugins & observability policy".into(),
                description: "Plugin registration, attestation policy, PII redaction and the ATOF/ATIF/OTLP exporters."
                    .into(),
                display_path: "~/.config/nemo-relay/plugins.toml".into(),
                format: "toml".into(),
                editable: false,
                why_readonly: Some(policy_why.into()),
                validators: vec!["nemo-relay plugins validate".into()],
                authoritative: true,
                apply_effect: String::new(),
            },
            ConfigFileSpec {
                id: "config".into(),
                label: "Relay user config".into(),
                description: "Relay user config layer (agents and CLI defaults).".into(),
                display_path: "~/.config/nemo-relay/config.toml".into(),
                format: "toml".into(),
                editable: false,
                why_readonly: Some(policy_why.into()),
                validators: vec!["nemo-relay doctor --offline".into()],
                authoritative: true,
                apply_effect: String::new(),
            },
        ]
    }

    async fn wait_healthy(&self) -> (bool, String) {
        let deadline = Instant::now() + Duration::from_secs_f64(self.restart_timeout.max(0.0));
        let mut good = 0u32;
        let mut baseline: Option<(u64, u64)> = None;
        let mut last = "no health check completed".to_owned();
        loop {
            let (unit, probe) = tokio::join!(self.unit_state(), self.probe());
            let running = unit.known && unit.active == "active" && unit.sub == "running";
            if running && probe.reachable && probe.status.unwrap_or(0) < 500 {
                let id = (unit.pid, unit.restarts);
                if baseline.is_none() || baseline == Some(id) {
                    baseline = Some(id);
                    good += 1;
                } else {
                    // pid or restart count moved: the unit is flapping
                    baseline = Some(id);
                    good = 1;
                    last = "unit restarted again while settling (crash loop?)".into();
                }
                if good >= self.settle_checks {
                    return (
                        true,
                        format!(
                            "gateway healthy: {}, pid {}, {good} consecutive checks",
                            probe.detail, unit.pid
                        ),
                    );
                }
            } else {
                good = 0;
                last = format!("unit={}/{} probe={}", unit.active, unit.sub, probe.detail);
            }
            if Instant::now() >= deadline {
                return (
                    false,
                    format!(
                        "gateway not healthy within {:.0}s: {last}",
                        self.restart_timeout
                    ),
                );
            }
            tokio::time::sleep(Duration::from_secs_f64(self.poll_interval.max(0.0))).await;
        }
    }

    async fn restart(&self) -> (bool, String) {
        let r = self
            .run(
                &["systemctl", "--user", "restart", &self.unit],
                60.0,
                16_000,
            )
            .await;
        if r.returncode != 0 || r.timed_out {
            let text = if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            };
            return (
                false,
                scrub(&format!(
                    "systemctl restart failed (rc={}, timed_out={}): {}",
                    r.returncode,
                    if r.timed_out { "True" } else { "False" },
                    char_prefix(text, 400)
                )),
            );
        }
        let (ok, mut detail) = self.wait_healthy().await;
        if !ok {
            let st = self
                .run(
                    &[
                        "systemctl",
                        "--user",
                        "status",
                        "--no-pager",
                        "-n",
                        "15",
                        &self.unit,
                    ],
                    10.0,
                    8_000,
                )
                .await;
            let text = scrub(if st.stdout.is_empty() {
                &st.stderr
            } else {
                &st.stdout
            });
            let n = char_len(&text);
            detail.push('\n');
            detail.push_str(char_suffix_from(&text, n.saturating_sub(1800)));
        }
        (ok, detail)
    }

    fn doctor_summary(d: &IndexMap<String, PyJson>) -> String {
        let empty = IndexMap::new();
        let dd = |v: Option<&PyJson>| -> IndexMap<String, PyJson> {
            match v {
                Some(PyJson::Dict(m)) => m.clone(),
                _ => empty.clone(),
            }
        };
        let mut lines = vec![format!(
            "nemo-relay {} (doctor schema {})",
            pj_str(d.get("binary_version")),
            pj_str(d.get("schema_version"))
        )];
        let cfg = dd(d.get("configuration"));
        let ph = dd(cfg.get("plugin_host_validation"));
        lines.push(format!(
            "plugin host validation: {} - {}",
            pj_str(ph.get("status")),
            pj_str(ph.get("details"))
        ));
        let rep = dd(ph.get("report"));
        if let Some(PyJson::List(dp)) = rep.get("dynamic_plugins") {
            for p in dp {
                if let PyJson::Dict(p) = p {
                    let s = dd(p.get("status"));
                    lines.push(format!(
                        "  plugin {}: manifest={} integrity={} authenticity={} policy_satisfied={}",
                        pj_str(p.get("plugin_id")),
                        pj_str(s.get("manifest")),
                        pj_str(s.get("integrity")),
                        pj_str(s.get("authenticity")),
                        pj_str(s.get("policy_satisfied"))
                    ));
                }
            }
        }
        if let Some(PyJson::List(obs)) = d.get("observability") {
            for o in obs {
                if let PyJson::Dict(o) = o {
                    lines.push(format!(
                        "observability: {}: {} - {}",
                        pj_str(o.get("name")),
                        pj_str(o.get("status")),
                        char_prefix(&pj_str(o.get("details")), 160)
                    ));
                }
            }
        }
        if let Some(ua @ PyJson::Dict(_)) = cfg.get("upstream_auth") {
            lines.push(format!(
                "upstream_auth (CLI env only): {}",
                json_dumps(&ua.to_value())
            ));
        }
        lines.push(String::new());
        for c in doctor_caveats() {
            lines.push(format!("NOTE: {c}"));
        }
        lines.join("\n")
    }

    fn playground_ids(&self) -> Result<Vec<String>, AdapterError> {
        let mut ids: Vec<String> = DEFAULT_ROUTE_IDS.iter().map(|s| (*s).to_owned()).collect();
        let Ok((raw, _)) = self.read_file("routes") else {
            return Ok(ids);
        };
        let Ok(doc) = pytoml::loads(&String::from_utf8_lossy(&raw)) else {
            return Ok(ids);
        };
        let routes = match doc.get("routes") {
            None => return Ok(ids),
            Some(TomlVal::Table(t)) => t.clone(),
            Some(other) => {
                return Err(AdapterError::failed(
                    "AttributeError",
                    format!("'{}' object has no attribute 'values'", type_name(other)),
                ));
            }
        };
        let found: Vec<String> = routes
            .values()
            .filter_map(|r| match r {
                TomlVal::Table(r) => match r.get("id") {
                    Some(TomlVal::Str(s)) if IDENT.is_match(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if !found.is_empty() {
            ids = found;
        }
        Ok(ids)
    }
}

/// Python `str(e)` of an action error entry, capped.
fn err_entry(e: &PyJson) -> String {
    char_prefix(&pj_str(Some(e)), 300).to_owned()
}

#[async_trait]
impl SystemAdapter for RelayAdapter {
    fn id(&self) -> SystemId {
        SystemId::Relay
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let now = now_ts();
        let (probe, upstream, unit, version, plist) = tokio::join!(
            self.probe(),
            self.upstream(),
            self.unit_state(),
            self.version(),
            self.plugins_list()
        );
        let (summary, plugins_mtime) = self.plugins_summary();
        let atof_scan = self.scan_dir("relay_atof_dir", None);
        let mut problems: Vec<Problem> = Vec::new();
        let restart_fix = format!("systemctl --user restart {}", self.unit);
        let unit_running = unit.known && unit.active == "active";
        let ustate = if !unit.known {
            "unknown"
        } else if unit.load == "not-found" {
            "absent"
        } else if unit.active == "failed" {
            "failed"
        } else if unit_running {
            "running"
        } else {
            "stopped"
        };
        let base = self.base();
        let litellm = self.litellm();
        let components = vec![
            Component {
                kind: "systemd".into(),
                name: self.unit.clone(),
                state: ustate.into(),
                image: None,
                detail: Some(if unit.known {
                    format!(
                        "{}/{} pid={} restarts={} enabled={}",
                        unit.active, unit.sub, unit.pid, unit.restarts, unit.enabled
                    )
                } else {
                    "systemctl --user show failed".into()
                }),
            },
            Component {
                kind: "http".into(),
                name: format!("gateway {base}"),
                state: if probe.reachable {
                    "running"
                } else {
                    "stopped"
                }
                .into(),
                image: None,
                detail: Some(if probe.reachable {
                    format!(
                        "{} on GET /health (the gateway answers it itself; 404 is expected and means UP)",
                        probe.detail
                    )
                } else {
                    format!("unreachable ({})", probe.detail)
                }),
            },
            Component {
                kind: "http".into(),
                name: format!("upstream {litellm}"),
                state: if upstream.reachable && upstream.status.unwrap_or(0) < 500 {
                    "running"
                } else {
                    "stopped"
                }
                .into(),
                image: None,
                detail: Some(if upstream.reachable {
                    format!("{} on GET /health/liveliness", upstream.detail)
                } else {
                    format!("unreachable ({})", upstream.detail)
                }),
            },
        ];
        let mut health;
        if probe.reachable {
            health = Health::Up;
            if probe.status.unwrap_or(0) >= 500 {
                health = Health::Degraded;
                problems.push(Problem::new(
                    Severity::Warn,
                    "relay.gateway.error",
                    format!(
                        "Gateway answered /health with HTTP {}.",
                        probe
                            .status
                            .map_or_else(|| "None".to_owned(), |s| s.to_string())
                    ),
                    Some(restart_fix.clone()),
                ));
            }
            if !upstream.reachable || upstream.status.unwrap_or(0) >= 500 {
                health = Health::Degraded;
                problems.push(Problem::new(
                    Severity::Warn,
                    "relay.gateway.upstream_error",
                    format!(
                        "Gateway is up but its upstream LiteLLM ({litellm}) is not answering /health/liveliness ({}); \
                         requests through the gateway will fail.",
                        upstream.detail
                    ),
                    Some(format!(
                        "curl -s -o /dev/null -w '%{{http_code}}\\n' {litellm}/health/liveliness"
                    )),
                ));
            }
            if unit.known && !unit_running {
                problems.push(Problem::new(
                    Severity::Info,
                    "relay.gateway.not_under_systemd",
                    format!(
                        "The gateway answers but {} is {}; it is being run by hand.",
                        self.unit, unit.active
                    ),
                    None,
                ));
            }
        } else {
            health = if unit_running {
                Health::Degraded
            } else {
                Health::Down
            };
            problems.push(Problem::new(
                Severity::Error,
                "relay.gateway.down",
                if unit_running {
                    format!("{} is active but {base} does not answer.", self.unit)
                } else {
                    format!(
                        "NeMo Relay gateway is not reachable at {base} ({} is {}).",
                        self.unit,
                        if unit.active.is_empty() {
                            "unknown"
                        } else {
                            &unit.active
                        }
                    )
                },
                Some(restart_fix.clone()),
            ));
        }
        if summary.parsed {
            if summary.attestation.as_deref() != Some("signature_required") {
                let att = summary.attestation.clone().unwrap_or_default();
                let shown = if att.is_empty() {
                    "unset".to_owned()
                } else {
                    att
                };
                problems.push(Problem::new(
                    if summary.dynamic_manifests > 0 {
                        Severity::Error
                    } else {
                        Severity::Warn
                    },
                    "relay.plugin.attestation_weak",
                    format!(
                        "Plugin attestation is {}, not 'signature_required': anyone who can write the plugin directory \
                         can swap the .so and its digest together.",
                        str_repr(&shown)
                    ),
                    Some(
                        "Local security policy: signature enforcement settings are a human decision. In \
                         ~/.config/nemo-relay/plugins.toml set [plugins.policy.defaults] attestation = \
                         \"signature_required\" with your trusted_public_keys; the console shows a diff only."
                            .into(),
                    ),
                ));
            }
            if summary.dynamic_manifests == 0 {
                problems.push(Problem::new(
                    Severity::Warn,
                    "relay.plugin.none_configured",
                    "No [[plugins.dynamic]] entry: Switchyard routing is not configured in Relay.",
                    Some("nemo-relay plugins add --user <path-to>/relay-plugin.toml".into()),
                ));
            }
            if summary.exporters_enabled() == 0 {
                problems.push(Problem::new(
                    Severity::Warn,
                    "relay.exporters.off",
                    "No exporter is enabled (ATOF, ATIF and OpenTelemetry are all off): Relay records nothing.",
                    Some(
                        "Set enabled = true under [components.config.atof] / .atif / .opentelemetry in \
                         ~/.config/nemo-relay/plugins.toml (human decision)."
                            .into(),
                    ),
                ));
            } else if !summary.pii_redaction && (summary.atof || summary.atif) {
                problems.push(Problem::new(
                    Severity::Warn,
                    "relay.pii_redaction.off",
                    "ATOF/ATIF are on but no enabled pii_redaction component precedes them; prompts land on disk \
                     unredacted.",
                    Some(
                        "Add the pii_redaction component from clients/nemo-relay/plugins.toml.example to \
                         ~/.config/nemo-relay/plugins.toml (human decision)."
                            .into(),
                    ),
                ));
            }
        } else {
            problems.push(Problem::new(
                Severity::Warn,
                "relay.plugins_toml.unreadable",
                summary
                    .error
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| "plugins.toml could not be read.".into()),
                Some("nemo-relay doctor --offline".into()),
            ));
        }
        let plugin_row = Self::plugin_row(plist.as_ref());
        let mut plugin_state = "not configured".to_owned();
        match (&plist, plugin_row) {
            (None, _) => problems.push(Problem::new(
                Severity::Info,
                "relay.plugins.list_unavailable",
                "`nemo-relay plugins list --json` failed; plugin enablement is unknown.",
                Some("nemo-relay plugins list".into()),
            )),
            (Some(_), Some(row)) => {
                let g = |k: &str| pj_str(row.get(k));
                plugin_state = format!(
                    "{} enabled={} validation={} policy={} attestation={}",
                    g("id"),
                    g("enabled"),
                    g("validation_state"),
                    g("policy_state"),
                    g("attestation_mode")
                );
                if !matches!(row.get("enabled"), Some(PyJson::Bool(true))) {
                    if health == Health::Up {
                        health = Health::Degraded;
                    }
                    problems.push(Problem::new(
                        Severity::Warn,
                        "relay.plugin.disabled",
                        format!("Plugin {} is registered but not enabled.", g("id")),
                        Some(format!("nemo-relay plugins enable {}", g("id"))),
                    ));
                } else if !matches!(row.get("validation_state"), Some(PyJson::Str(s)) if s == "valid")
                    || !matches!(row.get("policy_state"), Some(PyJson::Str(s)) if s == "valid")
                {
                    if health == Health::Up {
                        health = Health::Degraded;
                    }
                    problems.push(Problem::new(
                        Severity::Error,
                        "relay.plugin.invalid",
                        format!(
                            "Plugin {} validation={} policy={}.",
                            g("id"),
                            g("validation_state"),
                            g("policy_state")
                        ),
                        Some(format!("nemo-relay plugins validate {}", g("id"))),
                    ));
                }
                if pj_truthy(row.get("last_error")) {
                    problems.push(Problem::new(
                        Severity::Warn,
                        "relay.plugin.last_error",
                        char_prefix(&scrub(&g("last_error")), 300),
                        None,
                    ));
                }
            }
            (Some(_), None) => {}
        }
        if let (Some(since), Some(mt)) = (unit.active_since, plugins_mtime)
            && since != 0.0
            && mt != 0.0
            && mt > since + 1.0
            && unit_running
        {
            problems.push(Problem::new(
                Severity::Warn,
                "relay.config.newer_than_gateway",
                "plugins.toml changed after the gateway started; the running gateway has not loaded it.",
                Some(restart_fix),
            ));
        }
        if let Some((n, size)) = atof_scan
            && size > 1024u64.pow(3)
        {
            problems.push(Problem::new(
                Severity::Warn,
                "relay.atof.large",
                format!(
                    "ATOF directory holds {} in {n} files; Relay has no retention policy.",
                    human_bytes(size)
                ),
                Some(
                    "Prune old rotated files under ~/.local/share/nemo-relay/atof/ (relay-events.jsonl.N).".into(),
                ),
            ));
        }
        let endpoints = vec![Endpoint {
            name: "gateway".into(),
            url: base,
            role: "api".into(),
            auth: "bearer".into(),
        }];
        let mut summary_line = match health {
            Health::Up => "gateway up",
            Health::Degraded => "gateway degraded",
            Health::Down => "gateway down",
            _ => "unknown",
        }
        .to_owned();
        if plist.is_some() {
            summary_line.push_str(&format!(" · {plugin_state}"));
        }
        Ok(SystemStatus {
            system: SystemId::Relay,
            health,
            version,
            summary: char_prefix(&summary_line, 240).to_owned(),
            endpoints,
            components,
            problems,
            checked_at: now,
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let now = now_ts();
        let (probe, unit, plist) =
            tokio::join!(self.probe(), self.unit_state(), self.plugins_list());
        if !probe.reachable {
            return Ok(MetricsSnapshot {
                system: SystemId::Relay,
                ts: now,
                available: false,
                points: Vec::new(),
                error: Some(format!(
                    "gateway not reachable at {} ({})",
                    self.base(),
                    probe.detail
                )),
            });
        }
        let pt = |key: &str, v: f64, unit: &str| {
            let mut p = MetricPoint::new(key, v);
            p.unit = unit.to_owned();
            p
        };
        let mut pts = vec![pt("relay.gateway_up", 1.0, "1")];
        let (summary, plugins_mtime) = self.plugins_summary();
        let row = Self::plugin_row(plist.as_ref());
        if plist.is_some() {
            let stale = matches!((unit.active_since, plugins_mtime), (Some(s), Some(m)) if s != 0.0 && m != 0.0 && m > s + 1.0);
            let loaded = row.is_some_and(|r| {
                matches!(r.get("enabled"), Some(PyJson::Bool(true)))
                    && matches!(r.get("validation_state"), Some(PyJson::Str(s)) if s == "valid")
                    && matches!(r.get("policy_state"), Some(PyJson::Str(s)) if s == "valid")
            }) && !stale;
            pts.push(pt(
                "relay.plugin_loaded",
                if loaded { 1.0 } else { 0.0 },
                "1",
            ));
        }
        #[allow(clippy::cast_precision_loss)]
        if let Some((n, size)) = self.scan_dir("relay_atof_dir", None) {
            pts.push(pt("relay.atof_files", n as f64, "1"));
            pts.push(pt("relay.atof_bytes", size as f64, "B"));
        }
        let json_re = re(r"^.+\.json$");
        #[allow(clippy::cast_precision_loss)]
        if let Some((n, _)) = self.scan_dir("relay_atif_dir", Some(&json_re)) {
            pts.push(pt("relay.atif_trajectories", n as f64, "1"));
        }
        #[allow(clippy::cast_precision_loss)]
        if summary.parsed {
            pts.push(pt(
                "relay.exporters_enabled",
                summary.exporters_enabled() as f64,
                "1",
            ));
        }
        Ok(MetricsSnapshot {
            system: SystemId::Relay,
            ts: now,
            available: true,
            points: pts,
            error: None,
        })
    }

    /// Chronological (oldest first), at most `limit`, newest ATOF file(s) only. llm.chunk marks are skipped (noise).
    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let limit = limit.clamp(1, 1000);
        match self.read_recent(limit) {
            Ok(v) => Ok(v),
            Err(e @ AdapterError::Failed { .. }) if e.py_class() == "error" => Err(e),
            Err(_) => Ok(Vec::new()),
        }
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        Box::pin(async_stream::stream! {
            let mut state = FollowState::default();
            if let Some(since) = since_ts
                && let Ok(evs) = self.read_recent(500)
            {
                for ev in evs {
                    if ev.ts > since {
                        yield Ok(ev);
                    }
                }
            }
            loop {
                let events = self.follow_step(&mut state).unwrap_or_default();
                for ev in events {
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
        let key = files()
            .iter()
            .find(|(id, _)| *id == file_id)
            .map(|(_, k)| *k)
            .ok_or_else(|| AdapterError::UnknownId(file_id.to_owned()))?;
        self.path(key)
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        self.config_path(file_id)?;
        let spec = self
            .specs()
            .into_iter()
            .find(|s| s.id == file_id)
            .ok_or_else(|| AdapterError::failed("StopIteration", ""))?;
        let (raw, st) = self.read_file(file_id)?;
        Ok(ConfigFile {
            spec,
            content: scrub(&String::from_utf8_lossy(&raw)),
            sha256: sha256_hex(&raw),
            mtime: mtime_f64(&st).unwrap_or(0.0),
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        if file_id != "routes" {
            if !files().iter().any(|(id, _)| *id == file_id) {
                return Err(AdapterError::UnknownId(file_id.to_owned()));
            }
            let mut diags = Vec::new();
            if let Err(e) = pytoml::loads(content) {
                diags.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!("TOML syntax error: {e}"),
                    line: None,
                });
            }
            return Ok(ValidationResult {
                ok: diags.is_empty(),
                validator: "TOML parse".into(),
                authoritative: false,
                diagnostics: diags,
                note: Some(format!(
                    "Read-only in the console; this parse is only for reviewing a proposed diff. {VALIDATION_CAVEATS}"
                )),
            });
        }
        if content.len() as u64 > MAX_CONFIG_BYTES {
            return Ok(ValidationResult {
                ok: false,
                validator: "toml+switchyard-schema-mirror".into(),
                authoritative: false,
                diagnostics: vec![Diagnostic {
                    severity: Severity::Error,
                    message: "content exceeds 256 KiB".into(),
                    line: None,
                }],
                note: Some(ROUTES_NOTE.into()),
            });
        }
        let mut diags = validate_routes_semantics(content)?;
        if SECRET_SK.is_match(content) || SECRET_BEARER.is_match(content) {
            diags.push(Diagnostic {
                severity: Severity::Error,
                message: "content looks like it contains a literal secret; use api_key_env = \"ENV_NAME\"".into(),
                line: None,
            });
        }
        let ok = !diags.iter().any(|d| d.severity == Severity::Error);
        Ok(ValidationResult {
            ok,
            validator: "toml+switchyard-schema-mirror".into(),
            authoritative: false,
            diagnostics: diags,
            note: Some(ROUTES_NOTE.into()),
        })
    }

    async fn post_apply(&self, file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError> {
        if file_id != "routes" {
            return Ok(ApplyResult::new(
                false,
                format!("{file_id} is read-only in the console"),
            ));
        }
        if !restart {
            return Ok(ApplyResult::new(
                true,
                "written without restart: the gateway keeps its old routes until it is restarted",
            ));
        }
        let (ok, detail) = self.restart().await;
        let mut r = ApplyResult::new(ok, char_prefix(&detail, 2500));
        r.restarted = true;
        r.post_check = if ok {
            PostCheck::Healthy
        } else {
            PostCheck::Unhealthy
        };
        Ok(r)
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        Ok(vec![
            ActionSpec::new(
                "gateway_restart",
                "Restart gateway",
                &format!(
                    "systemctl --user restart {}, then health-check for up to 30 s. Drops in-flight requests on :4040 \
                     (opencode and other host clients).",
                    self.unit
                ),
                Danger::Disruptive,
            ),
            ActionSpec::new(
                "gateway_status",
                "Gateway status",
                &format!(
                    "systemctl --user status {} (Relay has no `gateway status` subcommand).",
                    self.unit
                ),
                Danger::Safe,
            ),
            ActionSpec::new(
                "doctor_offline",
                "Doctor (offline)",
                "nemo-relay doctor --offline --json, summarised, with notes on its known misleading lines.",
                Danger::Safe,
            ),
            ActionSpec::new(
                "agents_list",
                "List agents",
                "nemo-relay agents --json: supported coding agents and whether each is detected.",
                Danger::Safe,
            ),
            ActionSpec::new(
                "plugins_validate",
                "Validate plugins",
                "nemo-relay plugins validate <id> --json for every registered plugin. Passing does NOT prove the \
                 gateway will start.",
                Danger::Safe,
            ),
        ])
    }

    async fn run_action(
        &self,
        action_id: &str,
        _params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        let done = |ok: bool, out: &str, problems: Vec<Problem>| ActionResult {
            ok,
            output: char_prefix(&scrub(out), 20_000).to_owned(),
            duration_ms: ms(t0),
            problems,
        };
        let either = |r: &CommandResult| -> String {
            if r.stdout.is_empty() {
                r.stderr.clone()
            } else {
                r.stdout.clone()
            }
        };
        let err_first = |r: &CommandResult| -> String {
            if r.stderr.is_empty() {
                r.stdout.clone()
            } else {
                r.stderr.clone()
            }
        };
        match action_id {
            "gateway_restart" => {
                let (ok, detail) = self.restart().await;
                let probs = if ok {
                    Vec::new()
                } else {
                    vec![Problem::new(
                        Severity::Error,
                        "relay.gateway.restart_unhealthy",
                        "Gateway did not become healthy after restart.",
                        Some(format!("systemctl --user status {}", self.unit)),
                    )]
                };
                Ok(done(ok, &detail, probs))
            }
            "gateway_status" => {
                let r = self
                    .run(
                        &[
                            "systemctl",
                            "--user",
                            "status",
                            "--no-pager",
                            "-n",
                            "20",
                            &self.unit,
                        ],
                        10.0,
                        256_000,
                    )
                    .await;
                Ok(done(matches!(r.returncode, 0 | 3), &either(&r), Vec::new()))
            }
            "agents_list" => {
                let r = self
                    .run(&["nemo-relay", "agents", "--json"], 20.0, 256_000)
                    .await;
                if r.returncode != 0 {
                    return Ok(done(false, &err_first(&r), Vec::new()));
                }
                let Some(rows) = json_loads_bytes(r.stdout.as_bytes()) else {
                    return Ok(done(false, "agents --json did not return JSON", Vec::new()));
                };
                let items: Vec<PyJson> = match rows {
                    PyJson::List(a) => a,
                    PyJson::Dict(_) | PyJson::Str(_) => Vec::new(),
                    other => {
                        return Err(AdapterError::failed(
                            "TypeError",
                            format!("'{}' object is not iterable", pj_type(&other)),
                        ));
                    }
                };
                let lines: Vec<String> = items
                    .iter()
                    .filter_map(|x| match x {
                        PyJson::Dict(x) => Some(format!(
                            "{}: {} configured={} version={}",
                            pj_str(x.get("name")),
                            pj_str(x.get("status")),
                            pj_str(x.get("configured")),
                            pj_str(x.get("version"))
                        )),
                        _ => None,
                    })
                    .collect();
                Ok(done(true, &lines.join("\n"), Vec::new()))
            }
            "doctor_offline" => {
                let r = self
                    .run(
                        &["nemo-relay", "doctor", "--offline", "--json"],
                        30.0,
                        1_000_000,
                    )
                    .await;
                if r.returncode != 0 {
                    return Ok(done(false, char_prefix(&err_first(&r), 2000), Vec::new()));
                }
                match json_loads_bytes(r.stdout.as_bytes()) {
                    None => Ok(done(false, "doctor --json did not return JSON", Vec::new())),
                    Some(PyJson::Dict(d)) => Ok(done(true, &Self::doctor_summary(&d), Vec::new())),
                    Some(other) => Err(AdapterError::failed(
                        "AttributeError",
                        format!("'{}' object has no attribute 'get'", pj_type(&other)),
                    )),
                }
            }
            "plugins_validate" => {
                let plist = self.plugins_list().await;
                let ids: Vec<String> = plist
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| match r.get("id") {
                        Some(PyJson::Str(s)) if PLUGIN_ID.is_match(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                if ids.is_empty() {
                    return Ok(done(
                        false,
                        "no registered plugins found (`nemo-relay plugins list --json` returned none)",
                        Vec::new(),
                    ));
                }
                let mut out: Vec<String> = Vec::new();
                let mut ok = true;
                for pid in &ids {
                    let r = self
                        .run(
                            &["nemo-relay", "plugins", "validate", "--json", "--", pid],
                            30.0,
                            256_000,
                        )
                        .await;
                    let dd: PyJson = match json_loads_bytes(r.stdout.as_bytes()) {
                        Some(PyJson::Dict(d)) => d
                            .get("data")
                            .cloned()
                            .unwrap_or_else(|| PyJson::Dict(IndexMap::new())),
                        _ => PyJson::Dict(IndexMap::new()),
                    };
                    let PyJson::Dict(dd) = dd else {
                        return Err(AdapterError::failed(
                            "AttributeError",
                            format!("'{}' object has no attribute 'get'", pj_type(&dd)),
                        ));
                    };
                    let valid = pj_truthy(dd.get("valid")) && r.returncode == 0;
                    ok = ok && valid;
                    out.push(format!(
                        "{pid}: valid={} authenticity={} integrity={} policy={} attestation={}",
                        pj_str(dd.get("valid")),
                        pj_str(dd.get("authenticity_state")),
                        pj_str(dd.get("integrity_state")),
                        pj_str(dd.get("policy_state")),
                        pj_str(dd.get("attestation_mode"))
                    ));
                    let errs: Vec<PyJson> = if pj_truthy(dd.get("errors")) {
                        match dd.get("errors") {
                            Some(PyJson::List(a)) => a.clone(),
                            Some(PyJson::Str(s)) => {
                                s.chars().map(|c| PyJson::Str(c.to_string())).collect()
                            }
                            Some(PyJson::Dict(m)) => {
                                m.keys().map(|k| PyJson::Str(k.clone())).collect()
                            }
                            Some(other) => {
                                return Err(AdapterError::failed(
                                    "TypeError",
                                    format!("'{}' object is not subscriptable", pj_type(other)),
                                ));
                            }
                            None => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    for e in errs.iter().take(10) {
                        out.push(format!("  error: {}", err_entry(e)));
                    }
                }
                out.push(format!("NOTE: {VALIDATION_CAVEATS}"));
                Ok(done(ok, &out.join("\n"), Vec::new()))
            }
            other => Err(AdapterError::UnknownId(other.to_owned())),
        }
    }

    /// OpenAI wire on POST /v1/chat/completions through the gateway; route ids from routes.toml.
    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        let ids = self.playground_ids()?;
        let default = if ids.iter().any(|i| i == PLAYGROUND_DEFAULT_ROUTE) {
            PLAYGROUND_DEFAULT_ROUTE.to_owned()
        } else {
            ids[0].clone()
        };
        Ok(Some(PlaygroundSpec {
            wire: "openai_chat".into(),
            models: ids,
            default_model: default,
            note: "Chat goes through the Relay gateway on :4040 (Switchyard plugin routes) and is recorded to \
                   ATOF/ATIF (PII-redacted, if enabled). /v1/* answers 401 without a bearer, so the core attaches a \
                   LiteLLM virtual key by env NAME: playground_key_env['relay'] (default LITELLM_API_KEY, what \
                   opencode sends). Route ids come from routes.toml; engines behind them may be down."
                .into(),
        }))
    }

    fn playground_url(&self) -> Option<String> {
        Some(format!("{}/v1/chat/completions", self.base()))
    }
}
