//! NeMo Switchyard adapter (switchyard-server 0.3.0). OWNER: workstream A (PORTING.md section 9). Port of
//! `backend/src/nemo_console/adapters/switchyard.py`.
//!
//! Everything here was derived from measured output of the live container (fixtures under
//! tests/fixtures/switchyard): /health, /v1/models, /v1/stats, /metrics, routing.jsonl and
//! `switchyard-server --dry-run`. Process control goes through ctx.runner only; HTTP through ctx.http.
//!
//! Config keys the core provides:
//!   ctx.urls["switchyard"]            base URL, e.g. http://127.0.0.1:4010
//!   ctx.paths["switchyard_routes"]    absolute path of inference/switchyard/routes.toml
//!   ctx.paths["switchyard_dir"]       absolute path of inference/switchyard/ (compose project dir)
//!                                     (either one may be omitted; the other is derived from it)
//!   ctx.secret_env["switchyard"]      env-var NAME of the bearer the console sends (never read here)

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, RunOpts, SystemAdapter,
};
use crate::adapters::switchyard_capable::capable_models;
use crate::api::pyjson::{self, PyJson};
use crate::config::Settings;
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, PlaygroundSpec, PostCheck, Problem,
    Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::sha256_hex;
use crate::services::config_store::mtime_f64;
use crate::services::http::HttpResponse;
use crate::services::pyfmt::{char_len, char_suffix_from, list_repr, str_repr};

use crate::services::pytoml::{self, Table, TomlVal};
use crate::services::pyutil::{float_error, fromisoformat, py_float, py_splitlines, py_strip};

/// `SwitchyardAdapter.label` in Python.
pub const LABEL: &str = "NeMo Switchyard";

pub const CONTAINER: &str = "ai-lab-switchyard-switchyard-1";
pub const COMPOSE_PROJECT: &str = "ai-lab-switchyard";
pub const COMPOSE_SERVICE: &str = "switchyard";
pub const FALLBACK_IMAGE: &str = "ai-lab-switchyard:0.3.0";
pub const LOG_PATH: &str = "/var/log/switchyard/routing.jsonl";
pub const MAX_CONFIG_BYTES: usize = 256 * 1024;
/// Newest routing records used for sy.route.calls.
pub const ROUTE_COUNT_WINDOW: usize = 2000;
pub const DRY_RUN_VALIDATOR: &str = "switchyard-server --dry-run";
/// The route LiteLLM's `switchyard` alias forwards to (2026-10-16).
pub const PLAYGROUND_DEFAULT_ROUTE: &str = "super-lightning";
pub const SEMANTIC_VALIDATOR: &str = "tomllib + route/target/client cross-check";
pub const REDACTED: &str = "<redacted>";

/// Compile a literal pattern (all patterns here are constants; a failure would be a typo caught by the tests).
pub(crate) fn re(pat: &str) -> regex::Regex {
    regex::Regex::new(pat).unwrap_or_else(|_| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"[^\s\S]").unwrap()
    })
}

static LABEL_RE: LazyLock<regex::Regex> = LazyLock::new(|| re(r#"(\w+)="((?:[^"\\]|\\.)*)""#));
static SAMPLE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{(.*)\})?\s+(\S+)(?:\s+\S+)?$"));
static LINE_RE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"line (\d+)"));
static INVALID_CFG_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^invalid server config \S+: "));
static SERVICE_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9_-]*$"));

/// One Prometheus sample: (name, labels, value).
pub type Sample = (String, IndexMap<String, String>, f64);

// ── helpers ─────────────────────────────────────────────────────────────────────────────────────

/// Parse the Prometheus text format; tolerant: unparseable lines are skipped.
#[must_use]
pub fn parse_prometheus(text: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for raw in py_splitlines(text) {
        let line = py_strip(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(m) = SAMPLE_RE.captures(line) else {
            continue;
        };
        let Some(value) = m.get(3).and_then(|v| py_float(v.as_str())) else {
            continue;
        };
        let mut labels = IndexMap::new();
        for c in LABEL_RE.captures_iter(m.get(2).map_or("", |g| g.as_str())) {
            labels.insert(c[1].to_owned(), c[2].to_owned());
        }
        out.push((m[1].to_owned(), labels, value));
    }
    out
}

/// Quantile from cumulative `le` buckets (lifetime, linear interpolation). None if empty.
#[must_use]
pub fn histogram_quantile(q: f64, buckets: &[(f64, f64)]) -> Option<f64> {
    if buckets.is_empty() {
        return None;
    }
    let mut sorted: Vec<(f64, f64)> = buckets.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total = sorted.last().map_or(0.0, |b| b.1);
    if total <= 0.0 {
        return None;
    }
    let rank = q * total;
    let (mut prev_bound, mut prev_count) = (0.0f64, 0.0f64);
    for (b, count) in sorted {
        if count >= rank {
            if b == f64::INFINITY {
                return Some(prev_bound); // overflow bucket: best finite bound we know
            }
            #[allow(clippy::float_cmp)]
            if count == prev_count {
                return Some(b);
            }
            return Some(
                prev_bound + (b - prev_bound) * ((rank - prev_count) / (count - prev_count)),
            );
        }
        prev_bound = b;
        prev_count = count;
    }
    Some(prev_bound)
}

fn to_epoch(ts: Option<&PyJson>) -> Option<f64> {
    match ts {
        Some(PyJson::Str(s)) => fromisoformat(s).map(|d| d.timestamp()),
        _ => None,
    }
}

/// Python `urllib.parse.urlsplit` (+ the `port` / `hostname` properties), for [`base_url_problem`].
struct SplitUrl {
    scheme: String,
    netloc: String,
}

fn ip_v4(s: &str) -> Option<std::net::Ipv4Addr> {
    // ipaddress.IPv4Address: four decimal octets, no leading zeros, each <= 255
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut o = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if p.len() > 1 && p.starts_with('0') {
            return None;
        }
        o[i] = p.parse::<u8>().ok()?;
    }
    Some(std::net::Ipv4Addr::from(o))
}

/// `ipaddress.ip_address(s)` (a `%scope` suffix is allowed on IPv6, as in Python 3.9+).
pub(crate) fn ip_address(s: &str) -> Option<std::net::IpAddr> {
    if let Some(v4) = ip_v4(s) {
        return Some(std::net::IpAddr::V4(v4));
    }
    let core = match s.split_once('%') {
        Some((a, z)) if !z.is_empty() => a,
        Some(_) => return None,
        None => s,
    };
    core.parse::<std::net::Ipv6Addr>()
        .ok()
        .map(std::net::IpAddr::V6)
}

fn urlsplit(url: &str) -> Result<SplitUrl, String> {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = String::new();
    let mut rest = url.as_str();
    if let Some(i) = url.find(':')
        && i > 0
        && url.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && url[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        scheme = url[..i].to_ascii_lowercase();
        rest = &url[i + 1..];
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_owned();
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err("Invalid IPv6 URL".to_owned());
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    Ok(SplitUrl { scheme, netloc })
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    static FUT: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?s)^v[a-fA-F0-9]+\..+$"));
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err("Invalid IPv6 URL".to_owned());
            }
            let (h, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err("Invalid IPv6 URL".to_owned());
            }
            h
        }
        None => host_port.split_once(']').map_or(host_port, |(h, _)| h),
    };
    if hostname.starts_with('v') {
        if !FUT.is_match(hostname) {
            return Err("IPvFuture address is invalid".to_owned());
        }
        return Ok(());
    }
    match ip_address(hostname) {
        Some(std::net::IpAddr::V4(_)) => Err("An IPv4 address cannot be in brackets".to_owned()),
        Some(std::net::IpAddr::V6(_)) => Ok(()),
        None => Err(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            str_repr(hostname)
        )),
    }
}

impl SplitUrl {
    /// `_hostinfo`: (hostname, port) as raw strings.
    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, h)| h);
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (h, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (h, after.split_once(':').map_or("", |(_, p)| p))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (
            hostname.to_owned(),
            if port.is_empty() {
                None
            } else {
                Some(port.to_owned())
            },
        )
    }

    fn port(&self) -> Result<Option<u32>, String> {
        let (_, port) = self.hostinfo();
        let Some(p) = port else { return Ok(None) };
        if !(p.chars().all(char::is_numeric) && p.is_ascii()) {
            return Err(format!(
                "Port could not be cast to integer value as {}",
                str_repr(&p)
            ));
        }
        match p.parse::<u64>() {
            Ok(v) if v <= 65535 => Ok(Some(u32::try_from(v).unwrap_or(0))),
            _ => Err("Port out of range 0-65535".to_owned()),
        }
    }

    fn hostname(&self) -> Option<String> {
        let (h, _) = self.hostinfo();
        if h.is_empty() {
            return None;
        }
        Some(match h.split_once('%') {
            Some((a, z)) => format!("{}%{z}", a.to_lowercase()),
            None => h.to_lowercase(),
        })
    }
}

/// Why an llm_client base_url is not allowed, or None. Allowed hosts: loopback (127.0.0.0/8, ::1, localhost) and bare
/// single-label names (docker service names such as `litellm`). No userinfo.
#[must_use]
pub fn base_url_problem(value: Option<&TomlVal>) -> Option<String> {
    let Some(TomlVal::Str(value)) = value else {
        return Some("base_url is missing or not a string".to_owned());
    };
    let stripped = py_strip(value);
    if stripped.is_empty() {
        return Some("base_url is missing or not a string".to_owned());
    }
    let parts = match urlsplit(stripped) {
        Ok(p) => p,
        Err(e) => return Some(format!("base_url '{value}' does not parse: {e}")),
    };
    if let Err(e) = parts.port() {
        return Some(format!("base_url '{value}' does not parse: {e}"));
    }
    if parts.scheme != "http" && parts.scheme != "https" {
        return Some(format!("base_url '{value}' must use http or https"));
    }
    if parts.netloc.contains('@') {
        return Some("base_url must not contain userinfo (user:pass@host)".to_owned());
    }
    let host = parts.hostname().unwrap_or_default().to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
    if host.is_empty() {
        return Some(format!("base_url '{value}' has no host"));
    }
    if let Some(ip) = ip_address(&host) {
        let loopback = match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback(),
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|m| m.is_loopback())
            }
        };
        if loopback {
            return None;
        }
        return Some(format!(
            "base_url host {host} is not loopback; only loopback or an in-network service name is allowed"
        ));
    }
    if host == "localhost" || SERVICE_NAME_RE.is_match(&host) {
        return None;
    }
    Some(format!(
        "base_url host '{host}' is neither loopback nor a bare service name (no dots); external hosts need an \
         egress-allowlist change, not a routes.toml edit"
    ))
}

fn is_ident(v: &str) -> bool {
    let b = v.as_bytes();
    let Some(first) = b.first() else { return false };
    if !first.is_ascii_alphanumeric() || b.len() > 120 {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || b"_.:/@+-".contains(c))
}

/// Client-controlled strings are copied into events only if identifier-shaped; else a placeholder. Non-strings ->
/// None (keeps the metadata-only guarantee: no free text via model/route/session names).
fn ident(v: Option<&PyJson>, allow_empty: bool) -> Option<String> {
    let Some(PyJson::Str(s)) = v else { return None };
    if s.is_empty() && allow_empty {
        return Some(String::new());
    }
    Some(if is_ident(s) {
        s.clone()
    } else {
        REDACTED.to_owned()
    })
}

/// `_int(v)`: an int (never a bool) or 0.
pub(crate) fn py_int(v: Option<&PyJson>) -> i128 {
    match v {
        Some(PyJson::Int(i)) => *i,
        _ => 0,
    }
}

/// A Python int as JSON.
pub(crate) fn int_value(i: i128) -> Value {
    if let Ok(v) = i64::try_from(i) {
        return Value::from(v);
    }
    if let Ok(v) = u64::try_from(i) {
        return Value::from(v);
    }
    #[allow(clippy::cast_precision_loss)]
    serde_json::Number::from_f64(i as f64).map_or(Value::Null, Value::Number)
}

#[allow(clippy::cast_precision_loss)]
fn i2f(i: i128) -> f64 {
    i as f64
}

fn opt_str(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::String)
}

fn dict(v: Option<&PyJson>) -> Option<&IndexMap<String, PyJson>> {
    match v {
        Some(PyJson::Dict(d)) => Some(d),
        _ => None,
    }
}

/// `capable_models` (core contract) on a tomllib document.
#[must_use]
pub fn capable_models_of(cfg: &Table) -> IndexMap<String, Vec<String>> {
    capable_models(cfg)
}

/// One routing.jsonl line -> event. METADATA ONLY (whitelisted fields); None if malformed. Older records (before
/// route_id/algorithm were logged) lack those keys: tolerated.
#[must_use]
pub fn parse_routing_line(line: &str) -> Option<SystemEvent> {
    let line = py_strip(line);
    if line.is_empty() {
        return None;
    }
    let Ok(PyJson::Dict(rec)) = pyjson::loads(line.as_bytes()) else {
        return None;
    };
    let ts = to_epoch(rec.get("ts"))?;
    let route = ident(rec.get("route_id"), false);
    let algo = ident(rec.get("algorithm"), false);
    let model = ident(rec.get("model"), false);
    let tier = ident(rec.get("tier"), true).unwrap_or_default();
    let session = ident(rec.get("session_id"), false);
    let pt = py_int(rec.get("prompt_tokens"));
    let ct = py_int(rec.get("completion_tokens"));
    let mut data = Map::new();
    data.insert("route".into(), opt_str(route.clone()));
    data.insert("algorithm".into(), opt_str(algo));
    data.insert("model".into(), opt_str(model.clone()));
    data.insert("tier".into(), Value::String(tier.clone()));
    data.insert("session_id".into(), opt_str(session));
    data.insert("prompt_tokens".into(), int_value(pt));
    data.insert("completion_tokens".into(), int_value(ct));
    for k in [
        "reasoning_tokens",
        "cached_tokens",
        "cache_creation_tokens",
        "total_tokens",
    ] {
        data.insert(k.into(), int_value(py_int(rec.get(k))));
    }
    let tier_txt = if tier.is_empty() {
        String::new()
    } else {
        format!(" [{tier}]")
    };
    let route_s = route
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "-".into());
    let model_s = model
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "?".into());
    let summary = format!("{route_s} -> {model_s}{tier_txt} {pt}+{ct} tok");
    Some(SystemEvent {
        ts,
        system: SystemId::Switchyard,
        kind: "routing.decision".into(),
        summary,
        data,
    })
}

/// The binary repeats its message ('X: X: X'); keep one copy, drop the path prefix.
#[must_use]
pub fn tidy_dry_run_message(text: &str) -> String {
    let text = py_strip(text);
    let text = INVALID_CFG_RE.replace(text, "").into_owned();
    let mut text = match text.find("\n: ") {
        Some(cut) => text[..cut].to_owned(),
        None => text,
    };
    let half: Vec<&str> = text.split(": ").collect();
    if half.len() == 2 && half[0] == half[1] {
        text = half[0].to_owned();
    }
    py_strip(&text).to_owned()
}

fn line_of(msg: &str) -> Option<i64> {
    LINE_RE.captures(msg).and_then(|c| c[1].parse::<i64>().ok())
}

fn diag(sev: Severity, message: impl Into<String>, line: Option<i64>) -> Diagnostic {
    Diagnostic {
        severity: sev,
        message: message.into(),
        line,
    }
}

/// Python `str(v)` of an optional TOML value (`None` for a missing key).
fn py_str_opt(v: Option<&TomlVal>) -> String {
    v.map_or_else(|| "None".to_owned(), TomlVal::py_str)
}

/// `SwitchyardAdapter.semantic_check(cfg)`.
#[must_use]
pub fn semantic_check(cfg: &Table) -> Vec<Diagnostic> {
    let empty = Table::new();
    let tbl = |k: &str| cfg.get(k).and_then(TomlVal::as_table).unwrap_or(&empty);
    let (clients, targets, routes) = (tbl("llm_clients"), tbl("targets"), tbl("routes"));
    let mut d = Vec::new();
    for (cname, c) in clients {
        if let TomlVal::Table(c) = c
            && let Some(why) = base_url_problem(c.get("base_url"))
        {
            d.push(diag(
                Severity::Error,
                format!("llm_client '{cname}': {why}"),
                None,
            ));
        }
    }
    let mut seen: IndexMap<(String, String), String> = IndexMap::new();
    for (name, t) in targets {
        let TomlVal::Table(t) = t else { continue };
        let client = t.get("llm_client");
        let known = matches!(client, Some(TomlVal::Str(s)) if clients.contains_key(s));
        if !known {
            d.push(diag(
                Severity::Error,
                format!(
                    "target '{name}' uses llm_client '{}' which is not defined under [llm_clients]",
                    py_str_opt(client)
                ),
                None,
            ));
        }
        let id = (py_str_opt(t.get("id")), py_str_opt(client));
        if let Some(prev) = seen.get(&id) {
            d.push(diag(
                Severity::Warn,
                format!(
                    "targets '{prev}' and '{name}' share (id '{}', llm_client '{}'): switchyard keeps only one and \
                     silently drops the other (with its system_prompt). Give one its own [llm_clients.*] block.",
                    id.0, id.1
                ),
                None,
            ));
        } else {
            seen.insert(id, name.clone());
        }
    }
    for (rname, r) in routes {
        let TomlVal::Table(r) = r else { continue };
        let mut refs: Vec<(String, &TomlVal)> = r
            .iter()
            .filter(|(k, _)| *k == "target" || k.ends_with("_target"))
            .map(|(k, v)| (k.clone(), v))
            .collect();
        if let Some(TomlVal::Table(cls)) = r.get("classifier")
            && let Some(t) = cls.get("target")
        {
            refs.push(("classifier.target".to_owned(), t));
        }
        for (field, rf) in refs {
            let ok = matches!(rf, TomlVal::Str(s) if targets.contains_key(s));
            if !ok {
                d.push(diag(
                    Severity::Error,
                    format!(
                        "route '{rname}': {field} = '{}' is not a defined [targets.*] entry",
                        rf.py_str()
                    ),
                    None,
                ));
            }
        }
        if let Some(id) = r.get("id")
            && !matches!(id, TomlVal::Str(s) if s == rname)
        {
            d.push(diag(
                Severity::Warn,
                format!(
                    "route table key '{rname}' differs from its id '{}' (clients see the id)",
                    id.py_str()
                ),
                None,
            ));
        }
    }
    if routes.is_empty() {
        d.push(diag(Severity::Error, "no [routes.*] defined", None));
    }
    d
}

/// Python truthiness of a TOML value (`cfg.get("routes") or {}`).
pub(crate) fn truthy(v: &TomlVal) -> bool {
    match v {
        TomlVal::Str(s) => !s.is_empty(),
        TomlVal::Int(i) => *i != 0,
        TomlVal::BigInt(_) | TomlVal::Date(_) | TomlVal::Time(_) | TomlVal::DateTime(..) => true,
        TomlVal::Float(f) => *f != 0.0,
        TomlVal::Bool(b) => *b,
        TomlVal::Array(a) => !a.is_empty(),
        TomlVal::Table(t) => !t.is_empty(),
    }
}

/// `type(v).__name__`.
pub(crate) fn type_name(v: &TomlVal) -> &'static str {
    match v {
        TomlVal::Str(_) => "str",
        TomlVal::Int(_) | TomlVal::BigInt(_) => "int",
        TomlVal::Float(_) => "float",
        TomlVal::Bool(_) => "bool",
        TomlVal::Date(_) => "date",
        TomlVal::Time(_) => "time",
        TomlVal::DateTime(..) => "datetime",
        TomlVal::Array(_) => "list",
        TomlVal::Table(_) => "dict",
    }
}

/// `sorted(str(r.get("id", k)) for k, r in (cfg.get("routes") or {}).items() if isinstance(r, dict))`.
fn file_route_ids(cfg: &Table) -> Result<Vec<String>, AdapterError> {
    let Some(routes) = cfg.get("routes").filter(|v| truthy(v)) else {
        return Ok(Vec::new());
    };
    let TomlVal::Table(routes) = routes else {
        return Err(AdapterError::failed(
            "AttributeError",
            format!("'{}' object has no attribute 'items'", type_name(routes)),
        ));
    };
    let mut ids: Vec<String> = routes
        .iter()
        .filter_map(|(k, r)| match r {
            TomlVal::Table(r) => Some(r.get("id").map_or_else(|| k.clone(), TomlVal::py_str)),
            _ => None,
        })
        .collect();
    ids.sort();
    Ok(ids)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

// ── adapter ─────────────────────────────────────────────────────────────────────────────────────

/// Mutable observations (Python instance attributes).
#[derive(Default)]
struct SyState {
    /// Last live /v1/models ids.
    route_ids: Vec<String>,
    /// model -> (calls = SUCCESSES, errors)
    prev_model_counts: HashMap<String, (i128, i128)>,
    route_calls_cache: Option<(Instant, Vec<MetricPoint>)>,
}

pub struct SwitchyardAdapter {
    pub ctx: AdapterContext,
    // deployment identities (addendum 9.11): defaults = the module constants; `from_settings` sets the configured ones
    pub container: String,
    pub compose_project: String,
    pub compose_service: String,
    pub fallback_image: String,
    pub log_path: String,
    /// post-apply /health budget (s)
    pub poll_seconds: f64,
    pub poll_interval: f64,
    pub follow_interval: f64,
    st: Mutex<SyState>,
}

/// Build the adapter (Python: registry `build_adapter` + the container's configure hook).
pub fn build(ctx: &AdapterContext, cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(SwitchyardAdapter::from_settings(ctx, cfg)))
}

impl SwitchyardAdapter {
    /// `SwitchyardAdapter(ctx)` with the class defaults.
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        SwitchyardAdapter {
            ctx,
            container: CONTAINER.to_owned(),
            compose_project: COMPOSE_PROJECT.to_owned(),
            compose_service: COMPOSE_SERVICE.to_owned(),
            fallback_image: FALLBACK_IMAGE.to_owned(),
            log_path: LOG_PATH.to_owned(),
            poll_seconds: 30.0,
            poll_interval: 1.0,
            follow_interval: 2.0,
            st: Mutex::new(SyState::default()),
        }
    }

    /// `build_adapter` + `container._configure_switchyard`.
    #[must_use]
    pub fn from_settings(ctx: &AdapterContext, cfg: &Settings) -> Self {
        let mut a = SwitchyardAdapter::new(ctx.clone());
        a.container.clone_from(&cfg.switchyard_container);
        a.compose_project
            .clone_from(&cfg.switchyard_compose_project);
        a.compose_service
            .clone_from(&cfg.switchyard_compose_service);
        a.fallback_image.clone_from(&cfg.switchyard_image);
        a.log_path.clone_from(&cfg.switchyard_log_path);
        a
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SyState> {
        match self.st.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// Python `adapter._route_ids = [...]` (tests).
    pub fn set_route_ids(&self, ids: Vec<String>) {
        self.state().route_ids = ids;
    }

    // ── paths / urls ────────────────────────────────────────────────────────────────────────
    #[must_use]
    pub fn base_url(&self) -> String {
        self.ctx
            .urls
            .get("switchyard")
            .map_or("http://127.0.0.1:4010", String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn dir(&self) -> Result<PathBuf, AdapterError> {
        if let Some(d) = self.ctx.paths.get("switchyard_dir") {
            return Ok(d.clone());
        }
        match self.ctx.paths.get("switchyard_routes") {
            Some(r) => Ok(r.parent().map_or_else(|| r.clone(), PathBuf::from)),
            None => Err(AdapterError::UnknownId("switchyard_routes".into())),
        }
    }

    fn routes_path(&self) -> Result<PathBuf, AdapterError> {
        if let Some(r) = self.ctx.paths.get("switchyard_routes") {
            return Ok(r.clone());
        }
        match self.ctx.paths.get("switchyard_dir") {
            Some(d) => Ok(d.join("routes.toml")),
            None => Err(AdapterError::UnknownId("switchyard_dir".into())),
        }
    }

    // ── http ────────────────────────────────────────────────────────────────────────────────
    async fn get(&self, path: &str, timeout: f64) -> Option<HttpResponse> {
        self.ctx
            .http
            .get(&format!("{}{path}", self.base_url()), timeout)
            .await
            .ok()
    }

    async fn get_json(&self, path: &str) -> Option<PyJson> {
        let r = self.get(path, 5.0).await?;
        if r.status != 200 {
            return None;
        }
        pyjson::loads(&r.body).ok()
    }

    // ── docker (through the runner only) ────────────────────────────────────────────────────
    /// `_inspect`: (state, health, image) after verifying the compose labels; None when absent/foreign.
    pub async fn inspect(&self) -> Result<Option<(String, String, String)>, AdapterError> {
        let fmt = "{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{end}}|{{.Config.Image}}|\
                   {{index .Config.Labels \"com.docker.compose.project\"}}|\
                   {{index .Config.Labels \"com.docker.compose.service\"}}";
        let a = argv(&["docker", "inspect", "--format", fmt, &self.container]);
        let res = self.ctx.runner.run(&a, RunOpts::timeout(10.0)).await?;
        if res.returncode != 0 {
            return Ok(None);
        }
        let parts: Vec<&str> = py_strip(&res.stdout).split('|').collect();
        if parts.len() != 5 || parts[3] != self.compose_project || parts[4] != self.compose_service
        {
            return Ok(None);
        }
        Ok(Some((
            parts[0].to_owned(),
            parts[1].to_owned(),
            parts[2].to_owned(),
        )))
    }

    /// Newest n routing lines, or None when the log cannot be read.
    async fn tail_log(&self, n: usize) -> Result<Option<Vec<String>>, AdapterError> {
        if self.inspect().await?.is_none() {
            return Ok(None);
        }
        let a = argv(&[
            "docker",
            "exec",
            &self.container,
            "tail",
            "-n",
            &n.to_string(),
            &self.log_path,
        ]);
        let res = self
            .ctx
            .runner
            .run(
                &a,
                RunOpts::timeout(15.0).max_bytes(2_000_000.min(n * 700 + 4096)),
            )
            .await?;
        if res.returncode != 0 {
            return Ok(None);
        }
        Ok(Some(
            py_splitlines(&res.stdout)
                .into_iter()
                .map(str::to_owned)
                .collect(),
        ))
    }

    // ── status ──────────────────────────────────────────────────────────────────────────────
    fn endpoints(&self) -> Vec<Endpoint> {
        let b = self.base_url();
        let e = |name: &str, url: String, role: &str, auth: &str| Endpoint {
            name: name.to_owned(),
            url,
            role: role.to_owned(),
            auth: auth.to_owned(),
        };
        vec![
            e(
                "chat",
                format!("{b}/v1/chat/completions"),
                "api",
                "forwarded",
            ),
            e("messages", format!("{b}/v1/messages"), "api", "forwarded"),
            e("models", format!("{b}/v1/models"), "api", "none"),
            e("health", format!("{b}/health"), "health", "none"),
            e("metrics", format!("{b}/metrics"), "metrics", "none"),
        ]
    }

    fn compose_file(&self) -> Result<String, AdapterError> {
        Ok(self.dir()?.join("docker-compose.yml").display().to_string())
    }

    fn compose_up_cmd(&self) -> Result<String, AdapterError> {
        Ok(format!(
            "docker compose -f {} up -d --no-build",
            self.compose_file()?
        ))
    }

    fn read_routes_file(&self) -> Option<Table> {
        let p = self.routes_path().ok()?;
        let md = std::fs::metadata(&p).ok()?;
        if md.len() > MAX_CONFIG_BYTES as u64 {
            return None;
        }
        let raw = std::fs::read(&p).ok()?;
        let text = String::from_utf8(raw).ok()?;
        pytoml::loads(&text).ok()
    }

    async fn status_inner(&self) -> Result<SystemStatus, AdapterError> {
        let now = now_ts();
        let mut problems: Vec<Problem> = Vec::new();
        let mut components: Vec<Component> = Vec::new();
        match self.inspect().await.unwrap_or(None) {
            None => components.push(Component {
                kind: "container".into(),
                name: self.container.clone(),
                state: "absent".into(),
                image: None,
                detail: Some(format!(
                    "not found or not the compose service {}/{}",
                    self.compose_project, self.compose_service
                )),
            }),
            Some((st, hl, img)) => {
                let state = if st == "running" {
                    "running"
                } else if matches!(st.as_str(), "exited" | "created" | "paused") {
                    "stopped"
                } else {
                    "unknown"
                };
                components.push(Component {
                    kind: "container".into(),
                    name: self.container.clone(),
                    state: state.into(),
                    image: Some(img),
                    detail: Some(format!(
                        "status={st} health={}",
                        if hl.is_empty() { "none" } else { &hl }
                    )),
                });
                if hl == "unhealthy" {
                    problems.push(Problem::new(
                        Severity::Error,
                        "switchyard.container.unhealthy",
                        "Docker healthcheck reports the switchyard container unhealthy.",
                        Some(format!("docker logs --tail 50 {}", self.container)),
                    ));
                }
            }
        }
        let health_r = self.get("/health", 5.0).await;
        let up = health_r.as_ref().is_some_and(|r| r.status == 200);
        components.push(Component {
            kind: "http".into(),
            name: format!("{}/health", self.base_url()),
            state: if up { "running" } else { "failed" }.into(),
            image: None,
            detail: health_r.as_ref().map(|r| format!("HTTP {}", r.status)),
        });
        if !up {
            problems.push(Problem::new(
                Severity::Error,
                "switchyard.down",
                format!(
                    "Switchyard is not answering /health at {}.",
                    self.base_url()
                ),
                Some(self.compose_up_cmd()?),
            ));
            return Ok(SystemStatus {
                system: SystemId::Switchyard,
                health: Health::Down,
                version: None,
                summary: "Switchyard is down".into(),
                endpoints: self.endpoints(),
                components,
                problems,
                checked_at: now,
            });
        }
        let version = self.version().await;
        let models = self.get_json("/v1/models").await;
        let mut route_ids: Vec<String> = Vec::new();
        if let Some(PyJson::Dict(m)) = &models
            && let Some(PyJson::List(data)) = m.get("data")
        {
            route_ids = data
                .iter()
                .filter_map(|x| match x {
                    PyJson::Dict(d) => match d.get("id") {
                        Some(PyJson::Str(s)) => Some(s.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            route_ids.sort();
            self.state().route_ids.clone_from(&route_ids);
        }
        let stats = self.get_json("/v1/stats").await;
        problems.extend(self.route_problems(&route_ids)?);
        let stats_d = dict(stats.as_ref());
        if let Some(s) = stats_d {
            problems.extend(self.stats_problems(s));
        }
        let worst = if problems.is_empty() {
            Health::Up
        } else {
            Health::Degraded
        };
        let mut summary = format!("{} routes", route_ids.len());
        if let Some(s) = stats_d {
            summary.push_str(&format!(
                ", {} requests, {} errors (since start)",
                py_int(s.get("total_requests")),
                py_int(s.get("total_errors"))
            ));
        }
        if !problems.is_empty() {
            summary.push_str(&format!("; {} problem(s)", problems.len()));
        }
        Ok(SystemStatus {
            system: SystemId::Switchyard,
            health: worst,
            version,
            summary,
            endpoints: self.endpoints(),
            components,
            problems,
            checked_at: now,
        })
    }

    async fn version(&self) -> Option<String> {
        let r = self.get("/metrics", 5.0).await?;
        if r.status != 200 {
            return None;
        }
        for (name, labels, _) in parse_prometheus(&r.text()) {
            if name == "switchyard_build_info" {
                return labels.get("version").cloned();
            }
        }
        None
    }

    fn route_problems(&self, live_ids: &[String]) -> Result<Vec<Problem>, AdapterError> {
        let Some(cfg) = self.read_routes_file() else {
            return Ok(Vec::new());
        };
        if live_ids.is_empty() {
            return Ok(Vec::new());
        }
        let file_ids = file_route_ids(&cfg)?;
        if !file_ids.is_empty() && file_ids != live_ids {
            let live: HashSet<&String> = live_ids.iter().collect();
            let file: HashSet<&String> = file_ids.iter().collect();
            let mut missing: Vec<&String> = file.difference(&live).copied().collect();
            missing.sort();
            let mut extra: Vec<&String> = live.difference(&file).copied().collect();
            extra.sort();
            let show = |v: &[&String]| {
                if v.is_empty() {
                    "-".to_owned()
                } else {
                    list_repr(v)
                }
            };
            return Ok(vec![Problem::new(
                Severity::Warn,
                "switchyard.config.drift",
                format!(
                    "routes.toml differs from what the running server publishes (only in file: {}; only live: {}). \
                     The container has not been recreated since the file changed.",
                    show(&missing),
                    show(&extra)
                ),
                Some(format!(
                    "docker compose -f {} up -d --force-recreate --no-build",
                    self.compose_file()?
                )),
            )]);
        }
        Ok(Vec::new())
    }

    fn stats_problems(&self, stats: &IndexMap<String, PyJson>) -> Vec<Problem> {
        let mut problems = Vec::new();
        let empty = IndexMap::new();
        let models = dict(stats.get("models")).unwrap_or(&empty);
        let mut capable: Vec<(String, Vec<String>)> =
            capable_models_of(&self.read_routes_file().unwrap_or_default())
                .into_iter()
                .collect();
        capable.sort();
        let mut st = self.state();
        for (mid, routes) in capable {
            let Some(PyJson::Dict(m)) = models.get(&mid) else {
                continue;
            };
            // `calls` counts SUCCESSES only: calls + errors = attempts. A target that never succeeded has calls == 0.
            let (calls, errs) = (py_int(m.get("calls")), py_int(m.get("errors")));
            let prev = st.prev_model_counts.get(&mid).copied();
            let failing_now = prev.is_some_and(|p| errs > p.1 && calls <= p.0);
            let mostly_failing = errs > 0 && errs >= calls;
            if failing_now || mostly_failing {
                problems.push(Problem::new(
                    Severity::Warn,
                    "switchyard.capable_tier.upstream_errors",
                    format!(
                        "Capable-tier model '{mid}' (routes {}) is returning upstream errors ({errs} errors, {calls} \
                         successes). The engine behind it may be stopped.",
                        routes.join(", ")
                    ),
                    Some(format!(
                        "curl -s http://127.0.0.1:4010/v1/stats | jq '.models[\"{mid}\"]'  # then start the engine \
                         that serves this alias, or route via a direct-* / other auto-* route"
                    )),
                ));
            }
        }
        for (mid, m) in models {
            if let PyJson::Dict(m) = m {
                st.prev_model_counts.insert(
                    mid.clone(),
                    (py_int(m.get("calls")), py_int(m.get("errors"))),
                );
            }
        }
        drop(st);
        if let Some(PyJson::Dict(fb)) = stats.get("routing_fallbacks") {
            let n = py_int(fb.get("unavailable"));
            if n > 0 {
                problems.push(Problem::new(
                    Severity::Warn,
                    "switchyard.routing.fallback_unavailable",
                    format!("{n} request(s) were re-routed because a target was unavailable."),
                    Some("curl -s http://127.0.0.1:4010/v1/stats | jq .routing_fallbacks".into()),
                ));
            }
        }
        problems
    }

    // ── metrics ─────────────────────────────────────────────────────────────────────────────
    async fn metrics_inner(&self, now: f64) -> Result<MetricsSnapshot, AdapterError> {
        let mr = self.get("/metrics", 5.0).await;
        let stats = self.get_json("/v1/stats").await;
        let samples = match &mr {
            Some(r) if r.status == 200 => parse_prometheus(&r.text()),
            _ => Vec::new(),
        };
        let stats_d = dict(stats.as_ref());
        if samples.is_empty() && stats_d.is_none() {
            return Ok(MetricsSnapshot {
                system: SystemId::Switchyard,
                ts: now,
                available: false,
                points: Vec::new(),
                error: Some("neither /metrics nor /v1/stats answered".into()),
            });
        }
        let mut pts: Vec<MetricPoint> = Vec::new();
        let add = |pts: &mut Vec<MetricPoint>,
                   key: &str,
                   value: f64,
                   unit: &str,
                   labels: &[(&str, &str)]| {
            pts.push(MetricPoint {
                key: key.to_owned(),
                value,
                unit: unit.to_owned(),
                labels: labels
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            });
        };
        let total = |name: &str| -> Option<f64> {
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
        };
        if let Some(s) = stats_d {
            if s.contains_key("total_requests") {
                add(
                    &mut pts,
                    "sy.requests_total",
                    i2f(py_int(s.get("total_requests"))),
                    "1",
                    &[],
                );
            }
            if s.contains_key("total_errors") {
                add(
                    &mut pts,
                    "sy.errors_total",
                    i2f(py_int(s.get("total_errors"))),
                    "1",
                    &[],
                );
            }
            if let Some(PyJson::Dict(tt)) = s.get("total_tokens") {
                for kind in ["prompt", "completion", "reasoning", "cached"] {
                    if tt.contains_key(kind) {
                        add(
                            &mut pts,
                            "sy.tokens",
                            i2f(py_int(tt.get(kind))),
                            "tok",
                            &[("kind", kind)],
                        );
                    }
                }
            }
            if let Some(PyJson::Dict(models)) = s.get("models") {
                let mut sorted: Vec<(&String, &PyJson)> = models.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(b.0));
                for (mid, m) in sorted {
                    if let PyJson::Dict(m) = m
                        && m.contains_key("calls")
                    {
                        add(
                            &mut pts,
                            "sy.model.calls",
                            i2f(py_int(m.get("calls"))),
                            "1",
                            &[("model", mid)],
                        );
                    }
                }
            }
        } else {
            for (key, name) in [
                ("sy.requests_total", "switchyard_total_requests"),
                ("sy.errors_total", "switchyard_total_errors"),
            ] {
                if let Some(v) = total(name) {
                    add(&mut pts, key, v, "1", &[]);
                }
            }
            for (kind, name) in [
                ("prompt", "switchyard_prompt_tokens_total"),
                ("completion", "switchyard_completion_tokens_total"),
                ("reasoning", "switchyard_reasoning_tokens_total"),
            ] {
                if let Some(v) = total(name) {
                    add(&mut pts, "sy.tokens", v, "tok", &[("kind", kind)]);
                }
            }
            for (n, labels, v) in &samples {
                if n == "switchyard_requests_total"
                    && let Some(model) = labels.get("model")
                {
                    add(&mut pts, "sy.model.calls", *v, "1", &[("model", model)]);
                }
            }
        }
        if let Some(v) = total("switchyard_algorithms_in_flight") {
            add(&mut pts, "sy.in_flight", v, "1", &[]);
        }
        let mut buckets: Vec<(f64, f64)> = Vec::new();
        for (n, labels, v) in &samples {
            if n == "switchyard_llm_call_duration_ms_bucket"
                && let Some(le) = labels.get("le")
            {
                let le = if le == "+Inf" || le == "Inf" {
                    f64::INFINITY
                } else {
                    py_float(le)
                        .ok_or_else(|| AdapterError::failed("ValueError", float_error(le)))?
                };
                #[allow(clippy::float_cmp)]
                match buckets.iter_mut().find(|(b, _)| *b == le) {
                    Some(slot) => slot.1 += v,
                    None => buckets.push((le, 0.0 + v)),
                }
            }
        }
        for (qname, q) in [("p50", 0.5), ("p95", 0.95)] {
            if let Some(val) = histogram_quantile(q, &buckets) {
                add(&mut pts, "sy.latency_ms", val, "ms", &[("q", qname)]);
            }
        }
        pts.extend(self.route_call_points().await?);
        Ok(MetricsSnapshot {
            system: SystemId::Switchyard,
            ts: now,
            available: true,
            points: pts,
            error: None,
        })
    }

    /// sy.route.calls{route}: /metrics has no per-route counter, so count the newest ROUTE_COUNT_WINDOW routing.jsonl
    /// records (classifier judge rows excluded). Labelled with the window so nobody mistakes it for a lifetime
    /// counter. Cached 15 s (it costs a docker exec).
    async fn route_call_points(&self) -> Result<Vec<MetricPoint>, AdapterError> {
        if let Some((at, pts)) = &self.state().route_calls_cache
            && at.elapsed() < Duration::from_secs(15)
        {
            return Ok(pts.clone());
        }
        let Some(lines) = self.tail_log(ROUTE_COUNT_WINDOW).await? else {
            return Ok(Vec::new());
        };
        let mut counts: IndexMap<String, i64> = IndexMap::new();
        let mut n = 0usize;
        for line in &lines {
            let Some(ev) = parse_routing_line(line) else {
                continue;
            };
            n += 1;
            let route = ev.data.get("route").and_then(Value::as_str).unwrap_or("");
            let tier = ev.data.get("tier").and_then(Value::as_str);
            if !route.is_empty() && tier != Some("classifier") {
                *counts.entry(route.to_owned()).or_insert(0) += 1;
            }
        }
        let window = format!("last_{n}_records");
        let mut sorted: Vec<(String, i64)> = counts.into_iter().collect();
        sorted.sort();
        #[allow(clippy::cast_precision_loss)]
        let pts: Vec<MetricPoint> = sorted
            .into_iter()
            .map(|(r, c)| {
                let mut p = MetricPoint::new("sy.route.calls", c as f64);
                p.unit = "1".into();
                p.labels.insert("route".into(), r);
                p.labels.insert("window".into(), window.clone());
                p
            })
            .collect();
        self.state().route_calls_cache = Some((Instant::now(), pts.clone()));
        Ok(pts)
    }

    // ── config ──────────────────────────────────────────────────────────────────────────────
    fn spec() -> ConfigFileSpec {
        ConfigFileSpec {
            id: "routes".into(),
            label: "Switchyard routes".into(),
            description: "Route table, upstream LLM clients and targets for switchyard-server 0.3.0. Mounted \
                          read-only into the container, so a change needs a recreate."
                .into(),
            display_path: "inference/switchyard/routes.toml".into(),
            format: "toml".into(),
            editable: true,
            why_readonly: None,
            validators: vec![DRY_RUN_VALIDATOR.into(), SEMANTIC_VALIDATOR.into()],
            authoritative: true,
            apply_effect: "Recreates the switchyard container (docker compose up -d --force-recreate --no-build): a \
                           few seconds of refused/dropped requests, and the in-memory /v1/stats counters reset. \
                           routing.jsonl survives (named volume)."
                .into(),
        }
    }

    async fn image(&self) -> String {
        match self.inspect().await {
            Ok(Some((_, _, img))) => img,
            _ => self.fallback_image.clone(),
        }
    }

    /// Candidate goes over stdin into a throwaway, network-less, read-only container.
    async fn dry_run(&self, raw: &[u8]) -> (Vec<Diagnostic>, Option<String>) {
        let image = self.image().await;
        let a = argv(&[
            "docker",
            "run",
            "--rm",
            "-i",
            "--pull",
            "never",
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--pids-limit",
            "64",
            "--memory",
            "256m",
            "--entrypoint",
            "switchyard-server",
            &image,
            "--config",
            "/dev/stdin",
            "--dry-run",
        ]);
        let res: CommandResult = match self
            .ctx
            .runner
            .run(&a, RunOpts::timeout(60.0).stdin(raw.to_vec()))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return (
                    vec![diag(
                        Severity::Error,
                        format!(
                            "could not run the authoritative dry-run: {}: {e}",
                            e.py_class()
                        ),
                        None,
                    )],
                    None,
                );
            }
        };
        if res.timed_out {
            return (vec![diag(Severity::Error, "dry-run timed out", None)], None);
        }
        if res.returncode == 0 {
            let mut diags = Vec::new();
            for ln in py_splitlines(&res.stderr) {
                if let Some((_, after)) = ln.split_once(" WARN ") {
                    diags.push(diag(Severity::Warn, py_strip(after), None));
                }
            }
            let note = py_strip(&res.stdout);
            return (
                diags,
                if note.is_empty() {
                    None
                } else {
                    Some(note.to_owned())
                },
            );
        }
        let src = if res.stderr.is_empty() {
            &res.stdout
        } else {
            &res.stderr
        };
        let mut msg = tidy_dry_run_message(src);
        if msg.contains("Unable to find image") || msg.contains("No such image") || msg.is_empty() {
            let what = if msg.is_empty() {
                format!("exit {}", res.returncode)
            } else {
                msg.clone()
            };
            msg = format!("dry-run could not start (image {image} missing?): {what}");
        }
        let line = line_of(&msg);
        (vec![diag(Severity::Error, msg, line)], None)
    }

    fn recreate_argv(&self) -> Result<Vec<String>, AdapterError> {
        let f = self.compose_file()?;
        Ok(argv(&[
            "docker",
            "compose",
            "-f",
            &f,
            "up",
            "-d",
            "--force-recreate",
            "--no-build",
        ]))
    }

    async fn wait_healthy(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(self.poll_seconds.max(0.0));
        loop {
            if let Some(r) = self.get("/health", 3.0).await
                && r.status == 200
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_secs_f64(self.poll_interval.max(0.0))).await;
        }
    }

    async fn post_apply_routes(&self, restart: bool) -> Result<ApplyResult, AdapterError> {
        if !restart {
            return Ok(ApplyResult::new(
                true,
                "file written; the running server still uses the old routes until recreated",
            ));
        }
        let res = self
            .ctx
            .runner
            .run(
                &self.recreate_argv()?,
                RunOpts::timeout(120.0).cwd(self.dir()?),
            )
            .await?;
        if res.returncode != 0 || res.timed_out {
            let text = if !res.stderr.is_empty() {
                res.stderr.clone()
            } else if !res.stdout.is_empty() {
                res.stdout.clone()
            } else {
                "compose recreate failed".to_owned()
            };
            let n = char_len(&text);
            let mut r = ApplyResult::new(false, char_suffix_from(&text, n.saturating_sub(2000)));
            r.post_check = PostCheck::Unhealthy;
            return Ok(r);
        }
        let healthy = self.wait_healthy().await;
        let mut r = ApplyResult::new(
            healthy,
            if healthy {
                "recreated; /health OK".to_owned()
            } else {
                format!(
                    "recreated but /health not OK within {:.0}s",
                    self.poll_seconds
                )
            },
        );
        r.restarted = true;
        r.post_check = if healthy {
            PostCheck::Healthy
        } else {
            PostCheck::Unhealthy
        };
        Ok(r)
    }

    fn playground_ids(&self) -> Result<Vec<String>, AdapterError> {
        let mut ids = self.state().route_ids.clone();
        if ids.is_empty() {
            let cfg = self.read_routes_file().unwrap_or_default();
            ids = file_route_ids(&cfg)?;
        }
        if ids.is_empty() {
            ids = vec![PLAYGROUND_DEFAULT_ROUTE.to_owned()];
        }
        Ok(ids)
    }
}

fn ms(t0: Instant) -> i64 {
    i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// `Severity` as its JSON string.
#[must_use]
pub fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

#[async_trait]
impl SystemAdapter for SwitchyardAdapter {
    fn id(&self) -> SystemId {
        SystemId::Switchyard
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        self.status_inner().await
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let now = now_ts();
        match self.metrics_inner(now).await {
            Ok(m) => Ok(m),
            Err(e) => Ok(MetricsSnapshot {
                system: SystemId::Switchyard,
                ts: now,
                available: false,
                points: Vec::new(),
                error: Some(e.py_text()),
            }),
        }
    }

    /// Newest `limit` routing decisions, oldest first. [] when the log is unreadable.
    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let limit = limit.clamp(1, 1000);
        let Ok(Some(lines)) = self.tail_log(limit).await else {
            return Ok(Vec::new());
        };
        let evs: Vec<SystemEvent> = lines.iter().filter_map(|l| parse_routing_line(l)).collect();
        let skip = evs.len().saturating_sub(limit);
        Ok(evs.into_iter().skip(skip).collect())
    }

    /// Poll the log tail every follow_interval s (no streaming exec in the runner port). since_ts=None starts at
    /// 'now': only records newer than the newest existing one are emitted.
    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        Box::pin(async_stream::stream! {
            let mut cutoff = since_ts;
            let mut seen_at_cutoff: HashSet<String> = HashSet::new();
            let mut primed = false;
            loop {
                let lines = self.tail_log(300).await.unwrap_or(None);
                if let Some(lines) = lines {
                    let batch: Vec<(SystemEvent, String)> = lines
                        .iter()
                        .filter_map(|ln| parse_routing_line(ln).map(|ev| (ev, sha256_hex(ln.as_bytes()))))
                        .collect();
                    if !primed {
                        let c = match cutoff {
                            Some(c) => c,
                            None => batch.iter().map(|(ev, _)| ev.ts).reduce(f64::max).unwrap_or_else(now_ts),
                        };
                        cutoff = Some(c);
                        #[allow(clippy::float_cmp)]
                        {
                            seen_at_cutoff = batch.iter().filter(|(ev, _)| ev.ts == c).map(|(_, h)| h.clone()).collect();
                        }
                        primed = true;
                    }
                    for (ev, h) in batch {
                        let c = cutoff.unwrap_or(0.0);
                        #[allow(clippy::float_cmp)]
                        let skip = ev.ts < c || (ev.ts == c && seen_at_cutoff.contains(&h));
                        if skip {
                            continue;
                        }
                        if ev.ts > c {
                            cutoff = Some(ev.ts);
                            seen_at_cutoff.clear();
                        }
                        seen_at_cutoff.insert(h);
                        yield Ok(ev);
                    }
                }
                tokio::time::sleep(Duration::from_secs_f64(self.follow_interval.max(0.0))).await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        Ok(vec![Self::spec()])
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        if file_id != "routes" {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        self.routes_path()
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let path = self.config_path(file_id)?;
        let md = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AdapterError::FileNotFound(format!(
                    "[Errno 2] No such file or directory: {}",
                    str_repr(&path.display().to_string())
                )));
            }
            Err(e) => return Err(AdapterError::failed("OSError", e.to_string())),
        };
        if md.len() > MAX_CONFIG_BYTES as u64 {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Err(AdapterError::Invalid(format!(
                "{name} exceeds {MAX_CONFIG_BYTES} bytes"
            )));
        }
        let raw =
            std::fs::read(&path).map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let content = String::from_utf8(raw.clone())
            .map_err(|e| AdapterError::failed("UnicodeDecodeError", e.to_string()))?;
        Ok(ConfigFile {
            spec: Self::spec(),
            content,
            sha256: sha256_hex(&raw),
            mtime: mtime_f64(&md).unwrap_or(0.0),
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        if file_id != "routes" {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        let raw = content.as_bytes();
        if raw.len() > MAX_CONFIG_BYTES {
            return Ok(ValidationResult {
                ok: false,
                validator: DRY_RUN_VALIDATOR.into(),
                authoritative: true,
                diagnostics: vec![diag(
                    Severity::Error,
                    format!("content exceeds {MAX_CONFIG_BYTES} bytes"),
                    None,
                )],
                note: None,
            });
        }
        let mut diags = Vec::new();
        match pytoml::loads(content) {
            Ok(parsed) => diags.extend(semantic_check(&parsed)),
            Err(e) => {
                let line = line_of(&e.0);
                diags.push(diag(Severity::Error, format!("TOML syntax: {e}"), line));
            }
        }
        // The authoritative pass runs even after a semantic error so the user sees the binary's view too.
        let (dry, note) = self.dry_run(raw).await;
        diags.extend(dry);
        let ok = !diags.iter().any(|d| d.severity == Severity::Error);
        Ok(ValidationResult {
            ok,
            validator: DRY_RUN_VALIDATOR.into(),
            authoritative: true,
            diagnostics: diags,
            note,
        })
    }

    async fn post_apply(&self, file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError> {
        if file_id != "routes" {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        self.post_apply_routes(restart).await
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        Ok(vec![
            ActionSpec::new(
                "restart",
                "Recreate Switchyard",
                "docker compose up -d --force-recreate --no-build in inference/switchyard. Recreate (not plain \
                 restart) so a replaced routes.toml is re-mounted. Drops in-flight requests for a few seconds and \
                 resets /v1/stats.",
                Danger::Disruptive,
            ),
            ActionSpec::new(
                "reset_stats",
                "Reset stats",
                "POST /v1/stats/reset: zeroes the in-memory counters behind /v1/stats. routing.jsonl is not touched.",
                Danger::Destructive,
            ),
            ActionSpec::new(
                "dry_run_config",
                "Dry-run live routes.toml",
                "Validate the on-disk routes.toml with switchyard-server --dry-run in a throwaway network-less \
                 container.",
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
        match action_id {
            "restart" => {
                let r = self.post_apply_routes(true).await?;
                let probs = if r.ok {
                    Vec::new()
                } else {
                    vec![Problem::new(
                        Severity::Error,
                        "switchyard.restart.unhealthy",
                        r.detail.clone(),
                        Some(format!("docker logs --tail 50 {}", self.container)),
                    )]
                };
                Ok(ActionResult {
                    ok: r.ok,
                    output: r.detail,
                    duration_ms: ms(t0),
                    problems: probs,
                })
            }
            "reset_stats" => {
                let url = format!("{}/v1/stats/reset", self.base_url());
                match self.ctx.http.post_empty(&url, 10.0).await {
                    Err(e) => Ok(ActionResult {
                        ok: false,
                        output: format!("request failed: {}", e.class),
                        duration_ms: ms(t0),
                        problems: vec![Problem::new(
                            Severity::Error,
                            "switchyard.down",
                            "Switchyard did not answer.",
                            Some(self.compose_up_cmd()?),
                        )],
                    }),
                    // /v1/stats/reset needs no credential (verified live): report the status plainly.
                    Ok(resp) => Ok(ActionResult {
                        ok: (200..300).contains(&resp.status),
                        output: format!("HTTP {}", resp.status),
                        duration_ms: ms(t0),
                        problems: Vec::new(),
                    }),
                }
            }
            "dry_run_config" => {
                let cf = self.read_config("routes").await?;
                let v = self.validate_config("routes", &cf.content).await?;
                let mut lines: Vec<String> = v
                    .diagnostics
                    .iter()
                    .map(|x| format!("[{}] {}", severity_str(x.severity), x.message))
                    .collect();
                if let Some(n) = &v.note {
                    lines.push(n.clone());
                }
                let out = lines.join("\n");
                Ok(ActionResult {
                    ok: v.ok,
                    output: if out.is_empty() { "ok".into() } else { out },
                    duration_ms: ms(t0),
                    problems: Vec::new(),
                })
            }
            other => Err(AdapterError::UnknownId(other.to_owned())),
        }
    }

    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        let ids = self.playground_ids()?;
        // 2026-10-17: auto-local escalates to an engine that is not deployed any more; LiteLLM's own `switchyard`
        // alias forwards to super-lightning. Prefer that route when it exists, else the first one.
        let default = if ids.iter().any(|i| i == PLAYGROUND_DEFAULT_ROUTE) {
            PLAYGROUND_DEFAULT_ROUTE.to_owned()
        } else {
            ids[0].clone()
        };
        Ok(Some(PlaygroundSpec {
            wire: "openai_chat".into(),
            models: ids,
            default_model: default,
            note: "Models are Switchyard route ids (auto-*/super-* route by tier, direct-* are fixed passthroughs). \
                   Switchyard uses forward_auth: it forwards the caller's bearer to LiteLLM, so the console sends \
                   its own key, resolved server-side from the env var named in secret_env['switchyard']."
                .into(),
        }))
    }

    fn playground_url(&self) -> Option<String> {
        Some(format!("{}/v1/chat/completions", self.base_url()))
    }
}
