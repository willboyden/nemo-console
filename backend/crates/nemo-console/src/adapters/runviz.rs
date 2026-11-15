//! Per-run visualisation answers (addendum 9.15, Rust-only, PORTING.md D-40): the AIPerf and SkillEvaluator run
//! summaries, the AIPerf per-request rows and the Data Designer column profile. Pure functions over files the
//! adapters already own; each adapter validates the run id, resolves its run directory and calls these on the
//! blocking pool through [`blocking`].
//!
//! Bounds (wave-1 and wave-2 audits, applied from the start):
//!   * files are opened with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, must be regular files and are read with a byte
//!     cap ([`open_regular`]); a run directory or the AIPerf `artifacts` directory that is a symlink is not read;
//!   * every string that reaches an answer is a name or identifier that passed a full-match check with a length
//!     bound, or a unit; nothing else (no message, criterion, note, id or error text) is ever copied;
//!   * non-finite numbers become null; per-file, per-row and per-list caps; maps, never searches (linear work);
//!   * at most [`MAX_PARALLEL`] of these reads run at once (each holds a file of up to 16 MiB in memory).

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::base::AdapterError;
use crate::services::pyfmt::char_prefix;
use crate::services::support::{self as sp, full};

/// How many run-visualisation reads may run on the blocking pool at once (each may hold a file of up to 16 MiB).
pub const MAX_PARALLEL: usize = 4;

static PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MAX_PARALLEL);

/// Run `f` on the blocking pool, at most [`MAX_PARALLEL`] at a time. A panic in `f` is a 500, never a crash. The
/// permit moves into the blocking task, so a request that is dropped while `f` runs (the client went away) still
/// holds its slot until `f` ends.
pub async fn blocking<T, F>(f: F) -> Result<T, AdapterError>
where
    F: FnOnce() -> Result<T, AdapterError> + Send + 'static,
    T: Send + 'static,
{
    let permit = PERMITS
        .acquire()
        .await
        .map_err(|_| AdapterError::failed("RuntimeError", "run reader closed"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|_| AdapterError::failed("RuntimeError", "run reader failed"))?
}

// ── shared checks ───────────────────────────────────────────────────────────────────────────────

/// A metric, dimension, rubric criterion id or Tier 3 metric name.
static KEY_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z][a-z0-9_]{0,63}"));
/// A unit as AIPerf writes them (`ms`, `tokens/sec/user`, `%`, `KB`, `requests`).
static UNIT_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z%][A-Za-z0-9/%._ -]{0,31}"));
/// A SkillEvaluator validator name (`Schema & Repository Governance`, `QUALITY`, `SCRIPT_LINT`).
static VALIDATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9 &._/()+:-]{0,79}"));
/// A SkillEvaluator status word.
static WORD_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z][a-z0-9_]{0,31}"));
/// A Data Designer column name (it comes from the operator's script).
static COLUMN_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9_][A-Za-z0-9_ .:-]{0,127}"));
/// A Tier 3 agent name (= the adapter's `NAME_RE`).
static AGENT_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}"));
/// A router model alias (= the adapters' `MODEL_RE`).
static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}"));

/// `s` when it is a string of at most `max` bytes that fully matches `re`.
fn checked(v: Option<&Value>, re: &Regex, max: usize) -> Option<String> {
    match v {
        Some(Value::String(s)) if s.len() <= max && re.is_match(s) => Some(s.clone()),
        _ => None,
    }
}

fn opt(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::String)
}

/// A finite JSON number (bools are not numbers here).
#[must_use]
pub fn finite(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn num(x: Option<f64>) -> Value {
    x.and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// A JSON number kept as it is (integer stays integer) when finite, else null.
fn number_value(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Number(n)) if n.as_f64().is_some_and(f64::is_finite) => {
            Value::Number(n.clone())
        }
        _ => Value::Null,
    }
}

fn bool_value(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Bool(b)) => Value::Bool(*b),
        _ => Value::Null,
    }
}

fn obj(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v.and_then(Value::as_object)
}

/// A verdict word of the adapter's list (or `running`), else null.
fn verdict(meta: &Map<String, Value>, verdicts: &[&str]) -> Value {
    match meta.get("verdict") {
        Some(Value::String(s)) if s == "running" || verdicts.contains(&s.as_str()) => {
            Value::String(s.clone())
        }
        _ => Value::Null,
    }
}

// ── files ───────────────────────────────────────────────────────────────────────────────────────

/// A directory that is not a symlink (the run directory, the AIPerf `artifacts` directory).
#[must_use]
pub fn real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
}

/// Open `p` for reading without following a final symlink and without blocking on a FIFO; only a regular file is
/// accepted. Returns the file and its size.
pub fn open_regular(p: &Path) -> std::io::Result<(std::fs::File, u64)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(p)?;
    let md = f.metadata()?;
    if !md.file_type().is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok((f, md.len()))
}

/// At most `cap` bytes of a regular file and whether it had more. `None` when it cannot be opened or read.
#[must_use]
pub fn read_capped(p: &Path, cap: u64) -> Option<(Vec<u8>, bool)> {
    let (f, len) = open_regular(p).ok()?;
    let mut buf = Vec::new();
    f.take(cap).read_to_end(&mut buf).ok()?;
    Some((buf, len > cap))
}

/// A JSON object from a regular file of at most `cap` bytes (`None`: absent, a symlink, too large, not an object).
#[must_use]
pub fn read_json_object(p: &Path, cap: u64) -> Option<Map<String, Value>> {
    let (bytes, more) = read_capped(p, cap)?;
    if more {
        return None;
    }
    match sp::json_loads_bytes(&bytes)? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// The largest `run.json` these routes read (the adapters write a few KiB).
pub const MAX_META_BYTES: u64 = 4 * 1024 * 1024;

/// A stored run's `run.json` as the adapters' `load_meta` reads it (a run left at `running` by a process that no
/// longer runs it reads as interrupted), size-capped and never through a symlinked run directory. `None` = unknown.
#[must_use]
pub fn load_meta(run_dir: &Path, live: Option<&str>) -> Option<Map<String, Value>> {
    if !real_dir(run_dir) {
        return None;
    }
    let mut meta = read_json_object(&run_dir.join("run.json"), MAX_META_BYTES)?;
    if meta.get("verdict").and_then(Value::as_str) == Some("running") {
        let same =
            matches!((meta.get("run_id"), live), (Some(Value::String(r)), Some(l)) if r == l);
        if !same {
            meta.insert("verdict".into(), json!("interrupted"));
        }
    }
    Some(meta)
}

// ── AIPerf summary ──────────────────────────────────────────────────────────────────────────────

/// The statistics kept per AIPerf metric, in answer order.
pub const AIPERF_STATS: [&str; 13] = [
    "avg", "min", "max", "std", "p1", "p5", "p10", "p25", "p50", "p75", "p90", "p95", "p99",
];
/// At most this many metrics in a summary.
pub const AIPERF_MAX_METRICS: usize = 100;
/// The HTTP phases of the summary's stacked bar: answer key -> AIPerf metric (its `avg`, in ms).
pub const HTTP_PHASES: [(&str, &str); 5] = [
    ("dns", "http_req_dns_lookup"),
    ("connecting", "http_req_connecting"),
    ("sending", "http_req_sending"),
    ("waiting", "http_req_waiting"),
    ("receiving", "http_req_receiving"),
];
const AIPERF_PRESET_MAX: usize = 40;

/// `{unit, avg, ..., p99}` of one metric object, or `None` when it has no finite statistic.
fn aiperf_metric(m: &Map<String, Value>) -> Option<Value> {
    let mut out = Map::new();
    out.insert("unit".into(), opt(checked(m.get("unit"), &UNIT_RE, 32)));
    let mut any = false;
    for s in AIPERF_STATS {
        let v = finite(m.get(s));
        any |= v.is_some();
        out.insert(s.into(), num(v));
    }
    any.then_some(Value::Object(out))
}

/// A value in ms from an AIPerf `{value|avg, unit}`: ms as is, ns / us / s converted, any other unit -> None.
fn to_ms(v: f64, unit: Option<&str>) -> Option<f64> {
    let x = match unit {
        Some("ms") => v,
        Some("ns") => v / 1e6,
        Some("us") => v / 1e3,
        Some("s" | "sec") => v * 1e3,
        _ => return None,
    };
    x.is_finite().then_some(x)
}

/// `GET /systems/aiperf/runs/{run_id}/summary` from `run.json` and `artifacts/profile_export_aiperf.json` (None when
/// the run wrote none: `metrics` is then `{}`).
#[must_use]
pub fn aiperf_summary(
    run_id: &str,
    meta: &Map<String, Value>,
    profile: Option<&Map<String, Value>>,
    verdicts: &[&str],
) -> Value {
    let preset = checked(meta.get("preset"), &KEY_PRESET_RE, AIPERF_PRESET_MAX);
    let mut metrics = Map::new();
    let mut phases: Option<Map<String, Value>> = None;
    let mut duration_s = None;
    if let Some(doc) = profile {
        for (name, v) in doc {
            if metrics.len() >= AIPERF_MAX_METRICS {
                break;
            }
            let Some(m) = v.as_object() else { continue };
            if name.len() > 64 || !KEY_RE.is_match(name) {
                continue;
            }
            if let Some(row) = aiperf_metric(m) {
                metrics.insert(name.clone(), row);
            }
        }
        let mut ph = Map::new();
        let mut any = false;
        for (key, family) in HTTP_PHASES {
            let m = obj(doc.get(family));
            let v =
                m.and_then(|m| to_ms(finite(m.get("avg"))?, m.get("unit").and_then(Value::as_str)));
            any |= v.is_some();
            ph.insert(key.into(), num(v));
        }
        if any {
            phases = Some(ph);
        }
        duration_s = obj(doc.get("benchmark_duration")).and_then(|m| {
            let ms = to_ms(finite(m.get("avg"))?, m.get("unit").and_then(Value::as_str))?;
            Some(ms / 1e3)
        });
    }
    if duration_s.is_none() {
        duration_s = finite(obj(meta.get("metrics")).and_then(|m| m.get("duration_s")));
    }
    json!({
        "run_id": run_id,
        "preset": opt(preset),
        "model": opt(checked(meta.get("model"), &MODEL_RE, 128)),
        "verdict": verdict(meta, verdicts),
        "concurrency": number_value(meta.get("concurrency")),
        "requests": number_value(meta.get("requests")),
        "isl": number_value(meta.get("isl")),
        "osl": number_value(meta.get("osl")),
        "duration_s": num(duration_s),
        "metrics": Value::Object(metrics),
        "http_phases_ms": phases.map_or(Value::Null, Value::Object),
    })
}

/// An AIPerf preset name (= the adapter's `PRESET_RE`).
static KEY_PRESET_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z0-9][a-z0-9-]{0,39}"));

// ── AIPerf per-request rows ─────────────────────────────────────────────────────────────────────

/// At most this many rows in an answer (evenly sampled when the file has more).
pub const REQUESTS_MAX_ROWS: usize = 5000;
/// At most this many lines of `profile_export.jsonl` are looked at (a run has at most 2,100 requests; the rest of a
/// larger file is not read and the answer says `truncated`).
pub const REQUESTS_MAX_LINES: usize = 200_000;
/// At most this many distinct workers get an index (later ones are null).
pub const REQUESTS_MAX_WORKERS: usize = 1024;
const WORKER_ID_MAX: usize = 128;

/// `request_start_ns` / `request_end_ns` as an integer number of ns (an int, or a float within i64).
fn ns(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            #[allow(clippy::cast_possible_truncation)]
            (f.is_finite() && f.abs() < 9.0e18).then_some(f as i64)
        }),
        _ => None,
    }
}

/// One record metric `{value, unit}` in ms.
fn rec_ms(metrics: Option<&Map<String, Value>>, key: &str) -> Option<f64> {
    let m = obj(metrics?.get(key))?;
    to_ms(
        finite(m.get("value"))?,
        m.get("unit").and_then(Value::as_str),
    )
}

/// One record metric `{value, unit}` counting tokens (the first of `keys` that has one).
fn rec_tokens(metrics: Option<&Map<String, Value>>, keys: &[&str]) -> Option<f64> {
    let metrics = metrics?;
    keys.iter().find_map(|k| {
        let m = obj(metrics.get(*k))?;
        match m.get("unit").and_then(Value::as_str) {
            None | Some("tokens") => finite(m.get("value")).filter(|v| *v >= 0.0),
            Some(_) => None,
        }
    })
}

/// The error of a record as an integer code: its HTTP code (an integer, or a string of 1..6 digits), 0 when the
/// record failed without a usable code, None when it did not fail. The error text is never read.
fn error_code(rec: &Map<String, Value>) -> Value {
    let Some(err) = obj(rec.get("error")).filter(|e| !e.is_empty()) else {
        return Value::Null;
    };
    let code = match err.get("code") {
        Some(Value::Number(n)) => n.as_i64().filter(|c| (0..=999_999).contains(c)),
        Some(Value::String(s))
            if (1..=6).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            s.parse::<i64>().ok()
        }
        _ => None,
    };
    Value::from(code.unwrap_or(0))
}

/// `GET /systems/aiperf/runs/{run_id}/requests` from the bytes of `profile_export.jsonl` (`cut`: the file had more
/// than was read). Two passes over the bytes: the first parses every line once to count the requests, find the
/// first start and number the workers by first appearance; the second re-reads only the sampled lines.
#[must_use]
pub fn aiperf_requests(run_id: &str, bytes: &[u8], cut: bool) -> Value {
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut origin: Option<i64> = None;
    let mut workers: HashMap<String, u32> = HashMap::new();
    let mut truncated = cut;
    let mut pos = 0usize;
    let mut parts = bytes.split_inclusive(|b| *b == b'\n').peekable();
    while let Some(line) = parts.next() {
        let start = pos;
        pos += line.len();
        // a cut file ends in a partial line: not a request
        if cut && parts.peek().is_none() && !line.ends_with(b"\n") {
            break;
        }
        if lines.len() >= REQUESTS_MAX_LINES {
            truncated = true;
            break;
        }
        let Some(Value::Object(rec)) = sp::json_loads_bytes(line) else {
            continue;
        };
        let md = obj(rec.get("metadata"));
        if let Some(s) = ns(md.and_then(|m| m.get("request_start_ns"))) {
            origin = Some(origin.map_or(s, |o| o.min(s)));
        }
        if let Some(Value::String(w)) = md.and_then(|m| m.get("worker_id"))
            && !w.is_empty()
            && w.len() <= WORKER_ID_MAX
            && workers.len() < REQUESTS_MAX_WORKERS
            && !workers.contains_key(w)
        {
            let n = u32::try_from(workers.len()).unwrap_or(u32::MAX);
            workers.insert(w.clone(), n);
        }
        lines.push((start, line.len()));
    }
    let total = lines.len();
    let picks: Vec<usize> = if total <= REQUESTS_MAX_ROWS {
        (0..total).collect()
    } else {
        // evenly spaced, the first and the last request included
        (0..REQUESTS_MAX_ROWS)
            .map(|k| k * (total - 1) / (REQUESTS_MAX_ROWS - 1))
            .collect()
    };
    let rel = |v: Option<i64>| -> Value {
        match (v, origin) {
            #[allow(clippy::cast_precision_loss)]
            (Some(t), Some(o)) => num(Some((i128::from(t) - i128::from(o)) as f64 / 1e6)),
            _ => Value::Null,
        }
    };
    let mut rows = Vec::with_capacity(picks.len());
    for i in picks {
        let (start, len) = lines[i];
        let Some(Value::Object(rec)) = sp::json_loads_bytes(&bytes[start..start + len]) else {
            continue;
        };
        let md = obj(rec.get("metadata"));
        let metrics = obj(rec.get("metrics"));
        let worker = match md.and_then(|m| m.get("worker_id")) {
            Some(Value::String(w)) => workers.get(w).map_or(Value::Null, |n| Value::from(*n)),
            _ => Value::Null,
        };
        rows.push(json!({
            "i": i,
            "start_ms": rel(ns(md.and_then(|m| m.get("request_start_ns")))),
            "end_ms": rel(ns(md.and_then(|m| m.get("request_end_ns")))),
            "latency_ms": num(rec_ms(metrics, "request_latency")),
            "ttft_ms": num(rec_ms(metrics, "time_to_first_token")),
            "itl_avg_ms": num(rec_ms(metrics, "inter_token_latency")),
            "input_tokens": num(rec_tokens(metrics, &["input_sequence_length", "usage_prompt_tokens", "error_isl"])),
            "output_tokens": num(rec_tokens(metrics, &["output_sequence_length", "output_token_count", "usage_completion_tokens"])),
            "worker": worker,
            "error_code": error_code(&rec),
        }));
    }
    let returned = rows.len();
    json!({
        "run_id": run_id,
        "total": total,
        "returned": returned,
        "truncated": truncated || returned < total,
        "rows": rows,
    })
}

// ── SkillEvaluator summary ──────────────────────────────────────────────────────────────────────

/// Finding severities counted per validator (a finding without one counts as `info`; an unknown one is not counted).
pub const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "info"];
pub const SE_MAX_VALIDATORS: usize = 64;
pub const SE_MAX_RUBRIC: usize = 32;
pub const SE_MAX_DIMENSIONS: usize = 16;
pub const SE_MAX_AGENTS: usize = 8;
pub const SE_MAX_AGENT_METRICS: usize = 16;
const SE_TIERS: [&str; 4] = ["static", "tier1", "tier1+2", "tier3"];

/// The Tier 1 part from `skillevaluator-tier1.json`: validators with their finding counts by severity, the rubric's
/// scores and the quality dimensions. Never a finding message, criterion text or note.
#[must_use]
pub fn se_tier1(d: &Map<String, Value>) -> Value {
    let mut validators = Vec::new();
    for r in d
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if validators.len() >= SE_MAX_VALIDATORS {
            break;
        }
        let Some(r) = r.as_object() else { continue };
        let Some(name) = checked(r.get("validator"), &VALIDATOR_RE, 80) else {
            continue;
        };
        let mut counts: Map<String, Value> =
            SEVERITIES.iter().map(|s| ((*s).into(), json!(0))).collect();
        let mut tally = [0u64; 5];
        for f in r
            .get("findings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(f) = f.as_object() else { continue };
            let idx = match f.get("severity") {
                None | Some(Value::Null) => Some(4),
                Some(Value::String(s)) if s.len() <= 16 => {
                    let s = s.to_ascii_lowercase();
                    SEVERITIES.iter().position(|x| *x == s)
                }
                _ => None,
            };
            if let Some(i) = idx {
                tally[i] += 1;
            }
        }
        for (i, s) in SEVERITIES.iter().enumerate() {
            counts.insert((*s).into(), json!(tally[i]));
        }
        validators.push(json!({
            "name": name,
            "passed": bool_value(r.get("passed")),
            "status": opt(checked(r.get("status"), &WORD_RE, 32)),
            "findings": counts,
        }));
    }
    let mut rubric = Vec::new();
    for c in obj(d.get("rubric_eval"))
        .and_then(|r| r.get("checks"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if rubric.len() >= SE_MAX_RUBRIC {
            break;
        }
        let Some(c) = c.as_object() else { continue };
        let Some(id) = checked(c.get("id"), &KEY_RE, 64) else {
            continue;
        };
        let importance = match c.get("importance") {
            Some(Value::String(s)) if ["high", "medium", "low"].contains(&s.as_str()) => {
                Value::String(s.clone())
            }
            _ => Value::Null,
        };
        rubric.push(json!({
            "id": id,
            "importance": importance,
            "score": number_value(c.get("score")),
            "pass": bool_value(c.get("pass")),
            "judge_pass": bool_value(c.get("judge_pass")),
        }));
    }
    let mut dimensions = Map::new();
    let q0 = d
        .get("quality_summary")
        .and_then(Value::as_array)
        .and_then(|q| q.first())
        .and_then(Value::as_object);
    for (name, v) in obj(q0.and_then(|q| q.get("dimensions")))
        .into_iter()
        .flatten()
    {
        if dimensions.len() >= SE_MAX_DIMENSIONS {
            break;
        }
        if name.len() > 64 || !KEY_RE.is_match(name) {
            continue;
        }
        let score = match v {
            Value::Object(m) => finite(m.get("score")),
            other => finite(Some(other)),
        };
        if let Some(s) = score {
            dimensions.insert(name.clone(), num(Some(s)));
        }
    }
    json!({"validators": validators, "rubric": rubric, "dimensions": dimensions})
}

/// The Tier 3 part from the numbers the adapter already keeps in `run.json` (`tier3.agents`).
#[must_use]
pub fn se_tier3(t3: &Map<String, Value>) -> Value {
    let mut agents = Vec::new();
    for (name, a) in obj(t3.get("agents")).into_iter().flatten() {
        if agents.len() >= SE_MAX_AGENTS {
            break;
        }
        if name.len() > 128 || !AGENT_RE.is_match(name) {
            continue;
        }
        let Some(a) = a.as_object() else { continue };
        let mut metrics = Vec::new();
        for (key, m) in obj(a.get("metrics")).into_iter().flatten() {
            if metrics.len() >= SE_MAX_AGENT_METRICS {
                break;
            }
            if key.len() > 64 || !KEY_RE.is_match(key) {
                continue;
            }
            let m = m.as_object();
            metrics.push(json!({
                "name": key,
                "with_skill": num(finite(m.and_then(|m| m.get("with_skill")))),
                "without_skill": num(finite(m.and_then(|m| m.get("without_skill")))),
                "delta": num(finite(m.and_then(|m| m.get("delta")))),
            }));
        }
        agents.push(json!({
            "name": name,
            "model": opt(checked(a.get("model"), &MODEL_RE, 128)),
            "skill_lift": num(finite(a.get("skill_lift"))),
            "with_skill": num(finite(a.get("with_skill"))),
            "without_skill": num(finite(a.get("without_skill"))),
            "metrics": metrics,
        }));
    }
    json!({"agents": agents})
}

/// `GET /systems/skillevaluator/runs/{run_id}/summary`: `tier1` from the run's `skillevaluator-tier1.json` (None:
/// absent or unreadable), `tier3` from `run.json`.
#[must_use]
pub fn se_summary(
    run_id: &str,
    meta: &Map<String, Value>,
    tier1: Option<&Map<String, Value>>,
    verdicts: &[&str],
) -> Value {
    let tiers = match meta.get("tiers") {
        Some(Value::String(s)) if SE_TIERS.contains(&s.as_str()) => Value::String(s.clone()),
        _ => Value::Null,
    };
    json!({
        "run_id": run_id,
        "skill": opt(checked(meta.get("skill"), &AGENT_RE, 128)),
        "tiers": tiers,
        "verdict": verdict(meta, verdicts),
        "tier1": tier1.map_or(Value::Null, se_tier1),
        "tier3": obj(meta.get("tier3")).map_or(Value::Null, se_tier3),
    })
}

// ── Data Designer column profile ────────────────────────────────────────────────────────────────

pub const PROFILE_MAX_COLUMNS: usize = 200;
/// Distinct values are counted up to this many per column (`distinct_capped` past it).
pub const PROFILE_DISTINCT_CAP: usize = 1000;
/// `top` is given for a column with at most this many distinct values.
pub const PROFILE_TOP_DISTINCT: usize = 50;
pub const PROFILE_TOP_N: usize = 10;
/// Each top value is cut to this many characters.
pub const PROFILE_TOP_CHARS: usize = 64;
/// At most this many bytes of `records.jsonl` are read for a profile.
pub const PROFILE_MAX_BYTES: u64 = 64 * 1024 * 1024;

const TYPES: [&str; 6] = ["string", "number", "bool", "null", "object", "array"];

#[derive(Default)]
struct Col {
    name: String,
    /// string, number, bool, (explicit) null, object, array
    types: [u64; 6],
    seen: HashSet<u64>,
    capped: bool,
    lengths: Vec<u32>,
    numbers: Vec<f64>,
    /// hash -> (count, first row, shown value); dropped once the column has more than `PROFILE_TOP_DISTINCT`.
    top: Option<HashMap<u64, (u64, usize, Value)>>,
}

fn type_index(v: &Value) -> usize {
    match v {
        Value::String(_) => 0,
        Value::Number(_) => 1,
        Value::Bool(_) => 2,
        Value::Null => 3,
        Value::Object(_) => 4,
        Value::Array(_) => 5,
    }
}

/// The value as a top entry shows it: a string or the JSON text of an object / array cut to 64 characters, numbers
/// and booleans as they are.
fn shown(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(char_prefix(s, PROFILE_TOP_CHARS).to_owned()),
        Value::Number(_) | Value::Bool(_) => v.clone(),
        other => {
            let text = serde_json::to_string(other).unwrap_or_default();
            Value::String(char_prefix(&text, PROFILE_TOP_CHARS).to_owned())
        }
    }
}

/// `[min, p50, p95, max]` (nearest rank) of a sorted list, or None when empty.
fn quantiles<T: Copy + Into<f64>>(sorted: &[T]) -> Value {
    let n = sorted.len();
    if n == 0 {
        return Value::Null;
    }
    let rank = |q: f64| -> f64 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let i = ((q * n as f64).ceil() as usize).clamp(1, n) - 1;
        sorted[i].into()
    };
    json!({
        "min": num(Some(sorted[0].into())),
        "p50": num(Some(rank(0.5))),
        "p95": num(Some(rank(0.95))),
        "max": num(Some(sorted[n - 1].into())),
    })
}

/// `GET /systems/datadesigner/runs/{run_id}/profile` over at most `max_rows` lines of `records.jsonl` (read through
/// `next_line`, which gives each line and None at the end). `with_values` (admin) fills `top`; for a viewer every
/// `top` is null and `values_hidden` is true. Values are measured as the records route shows them (strings cut to
/// `cell_cap` characters, `files::cap_value`).
pub fn dd_profile(
    run_id: &str,
    mut next_line: impl FnMut() -> Option<Vec<u8>>,
    max_rows: usize,
    cell_cap: usize,
    with_values: bool,
    cap_value: impl Fn(&Value) -> Value,
) -> Value {
    let state = RandomState::new();
    let mut cols: Vec<Col> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut rows = 0usize;
    let mut truncated = false;
    while let Some(line) = next_line() {
        if rows >= max_rows {
            truncated = true;
            break;
        }
        let Some(Value::Object(rec)) = sp::json_loads_bytes(&line) else {
            continue;
        };
        let row = rows;
        rows += 1;
        for (k, raw) in &rec {
            let ci = match index.get(k) {
                Some(i) => *i,
                None => {
                    if cols.len() >= PROFILE_MAX_COLUMNS || k.len() > 128 || !COLUMN_RE.is_match(k)
                    {
                        truncated = true;
                        continue;
                    }
                    index.insert(k.clone(), cols.len());
                    cols.push(Col {
                        name: k.clone(),
                        top: with_values.then(HashMap::new),
                        ..Col::default()
                    });
                    cols.len() - 1
                }
            };
            let col = &mut cols[ci];
            let v = cap_value(raw);
            col.types[type_index(&v)] += 1;
            match &v {
                Value::Null => continue,
                Value::String(s) => {
                    let n = s.chars().count().min(cell_cap);
                    col.lengths.push(u32::try_from(n).unwrap_or(u32::MAX));
                }
                Value::Number(n) => {
                    if let Some(x) = n.as_f64().filter(|x| x.is_finite()) {
                        col.numbers.push(x);
                    }
                }
                _ => {}
            }
            let mut h = state.build_hasher();
            type_index(&v).hash(&mut h);
            match &v {
                Value::String(s) => s.hash(&mut h),
                other => serde_json::to_string(other)
                    .unwrap_or_default()
                    .hash(&mut h),
            }
            let key = h.finish();
            if !col.seen.contains(&key) {
                if col.seen.len() >= PROFILE_DISTINCT_CAP {
                    col.capped = true;
                } else {
                    col.seen.insert(key);
                }
            }
            if col.capped || col.seen.len() > PROFILE_TOP_DISTINCT {
                col.top = None;
            }
            if let Some(top) = col.top.as_mut() {
                let e = top.entry(key).or_insert_with(|| (0, row, shown(&v)));
                e.0 += 1;
            }
        }
    }
    let rows_u64 = u64::try_from(rows).unwrap_or(u64::MAX);
    let columns: Vec<Value> = cols
        .into_iter()
        .map(|mut c| {
            let non_null: u64 = c.types.iter().sum::<u64>() - c.types[3];
            let mut types = Map::new();
            for (i, t) in TYPES.iter().enumerate() {
                let n = if i == 3 {
                    rows_u64.saturating_sub(non_null)
                } else {
                    c.types[i]
                };
                types.insert((*t).into(), json!(n));
            }
            c.lengths.sort_unstable();
            c.numbers.sort_unstable_by(f64::total_cmp);
            let top = match c.top.take() {
                Some(m) if with_values => {
                    let mut entries: Vec<(u64, usize, Value)> = m.into_values().collect();
                    entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                    Value::Array(
                        entries
                            .into_iter()
                            .take(PROFILE_TOP_N)
                            .map(|(count, _, value)| json!({"value": value, "count": count}))
                            .collect(),
                    )
                }
                _ => Value::Null,
            };
            json!({
                "name": c.name,
                "types": types,
                "non_null": non_null,
                "distinct": c.seen.len(),
                "distinct_capped": c.capped,
                "length": quantiles(&c.lengths),
                "numeric": quantiles(&c.numbers),
                "top": top,
            })
        })
        .collect();
    json!({
        "run_id": run_id,
        "rows": rows,
        "columns": columns,
        "values_hidden": !with_values,
        "truncated": truncated,
    })
}

/// A line reader over a regular file: at most `total` bytes, lines of at most `line_cap` bytes (a longer line ends
/// the reading, as `files::for_each_line` does).
pub struct Lines {
    r: BufReader<std::io::Take<std::fs::File>>,
    line_cap: u64,
    done: bool,
}

impl Lines {
    pub fn open(p: &Path, total: u64, line_cap: u64) -> std::io::Result<Lines> {
        let (f, _) = open_regular(p)?;
        Ok(Lines {
            r: BufReader::new(f.take(total)),
            line_cap,
            done: false,
        })
    }

    /// The next line (with its newline), None at the end, on an error or past the line cap.
    pub fn next_line(&mut self) -> Option<Vec<u8>> {
        if self.done {
            return None;
        }
        let mut buf = Vec::new();
        match (&mut self.r)
            .take(self.line_cap + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => {
                self.done = true;
                None
            }
            Ok(n) if n as u64 > self.line_cap => {
                self.done = true;
                None
            }
            Ok(_) => Some(buf),
        }
    }
}
