//! Plumbing the four workstream B adapters (dcgm, aiperf, guardrails, openshell) share, each a port of an idiom their Python
//! sources repeat: the `_run` wrapper that turns a refused/failed spawn into data (rc 127), Python's `float()` /
//! `str()` / truthiness on JSON-loaded values, `time.strftime` on UTC, and the private atomic writes of run.json and
//! checks.jsonl.
//!
//! Shared by the DCGM, AIPerf, Guardrails and OpenShell adapters; folded into shared services.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::adapters::base::{AdapterContext, AdapterError, CommandResult, RunOpts};
use crate::services::pyfmt::{float_repr, str_repr};
use crate::services::pyyaml::{PyValue, YamlError};

use super::pystr;

/// `argv` as owned strings.
#[must_use]
pub fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// The adapters' `_run`: `ctx.runner.run(...)`, and any exception (not allowlisted, not on PATH, spawn failure) as
/// `CommandResult(returncode=127, stderr=f"{type(e).__name__}: {e}"[:300])`.
pub async fn run(ctx: &AdapterContext, argv: &[String], opts: RunOpts) -> CommandResult {
    match ctx.runner.run(argv, opts).await {
        Ok(r) => r,
        Err(e) => CommandResult::new(127, "", pystr::head(&format!("{}: {e}", e.py_class()), 300)),
    }
}

/// `RunOpts` with a timeout and an output cap.
#[must_use]
pub fn opts(timeout: f64, max_bytes: usize) -> RunOpts {
    RunOpts::timeout(timeout).max_bytes(max_bytes)
}

static PY_FLOAT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\A[+-]?(?:(?:[0-9](?:_?[0-9])*(?:\.(?:[0-9](?:_?[0-9])*)?)?|\.[0-9](?:_?[0-9])*)(?:e[+-]?[0-9](?:_?[0-9])*)?|inf|infinity|nan)\z",
    )
    .ok()
});

/// Python `float(s)` for a str (None = ValueError).
#[must_use]
pub fn py_float(s: &str) -> Option<f64> {
    let t = pystr::strip(s);
    if !PY_FLOAT.as_ref().is_some_and(|r| r.is_match(t)) {
        return None;
    }
    let clean = t.replace('_', "").to_ascii_lowercase();
    let (neg, body) = match clean.as_bytes().first() {
        Some(b'-') => (true, &clean[1..]),
        Some(b'+') => (false, &clean[1..]),
        _ => (false, clean.as_str()),
    };
    let v = match body {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        b => b.parse::<f64>().ok()?,
    };
    Some(if neg { -v } else { v })
}

/// Python `float(v)` for a JSON-loaded value (None = TypeError/ValueError).
#[must_use]
pub fn json_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float(s),
        _ => None,
    }
}

/// `isinstance(v, int | float) and not isinstance(v, bool)` -> `float(v)` (aiperf `_num`).
#[must_use]
pub fn json_num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `repr(v)` of a JSON-loaded value.
#[must_use]
pub fn json_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => {
            if n.is_f64() {
                float_repr(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        Value::String(s) => str_repr(s),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(json_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, x)| format!("{}: {}", str_repr(k), json_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(v)` of a JSON-loaded value.
#[must_use]
pub fn json_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => json_repr(other),
    }
}

/// `str(d.get(k))` (absent = `None`).
#[must_use]
pub fn json_get_str(v: Option<&Value>) -> String {
    v.map_or_else(|| "None".to_owned(), json_str)
}

/// Python truthiness of a JSON-loaded value.
#[must_use]
pub fn json_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// Python truthiness of a `yaml.safe_load`ed value (`services::pyyaml`).
#[must_use]
pub fn yaml_truthy(v: &PyValue) -> bool {
    v.truthy()
}

/// A Python exception that escapes `yaml.safe_load` unwrapped (a `ValueError` for `2024-02-30`, a `KeyError` for
/// `!!bool maybe`): not a `yaml.YAMLError`, so the adapters' `except yaml.YAMLError` does not catch it and it
/// propagates with its class. `None` for a YAML error (the caller handles it).
#[must_use]
pub fn yaml_escaped(e: &YamlError) -> Option<AdapterError> {
    (!e.is_yaml()).then(|| AdapterError::failed(e.class(), e.text()))
}

/// `time.strftime(fmt, time.gmtime(epoch))` for the two formats the adapters use: `%Y%m%dT%H%M%S` (run ids) and
/// `%Y-%m-%dT%H:%M:%SZ` (docker logs --since).
#[must_use]
pub fn gmtime(epoch: f64, compact: bool) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let secs = epoch.floor() as i64;
    let t =
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    if compact {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        )
    } else {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        )
    }
}

/// The run-id stamp: `time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"`,
/// from one clock reading (PORTING D-30).
#[must_use]
pub fn run_stamp() -> String {
    run_stamp_at(crate::domain::models::now_ts())
}

/// [`run_stamp`] for a given `time.time()`: the seconds and the milliseconds come from the same reading.
#[must_use]
pub fn run_stamp_at(now: f64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let ms = ((now * 1000.0) as i64).rem_euclid(1000);
    format!("{}-{ms:03}", gmtime(now, true))
}

/// `path.mkdir(mode=mode, parents=True, exist_ok=True)`: missing parents get the default mode, the leaf `mode`.
pub fn mkdir_leaf(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Python: `tmp = path.with_name("." + path.name + ".tmp"); tmp.write_text(text); tmp.chmod(0o600); tmp.replace(path)`.
/// Written with the core's atomic write instead (audit finding 9): a random temp name created exclusively with mode
/// 0600 (a planted symlink at a predictable temp name is never written through), fsync, rename over `path`.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    crate::services::config_store::atomic_write(path, text.as_bytes(), 0o600)
}

/// `json.dumps(doc, indent=1)`.
#[must_use]
pub fn json_indent1(doc: &Value) -> String {
    crate::services::pyfmt::json_dumps_with(
        doc,
        crate::services::pyfmt::JsonOpts {
            indent: Some(1),
            ..crate::services::pyfmt::JsonOpts::default()
        },
    )
}

/// `json.loads(path.read_text())` that must be a dict (`None` for OSError/ValueError or another type).
#[must_use]
pub fn load_json_dict(path: &Path) -> Option<serde_json::Map<String, Value>> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// `shutil.rmtree(d, ignore_errors=True)`.
pub fn rmtree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Python's `int(x)` of a finite float (truncation toward zero).
#[must_use]
pub fn py_int_of(x: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let v = x.trunc() as i64;
    v
}

/// Seconds as a tokio sleep (negative/NaN = no wait).
pub async fn sleep_s(s: f64) {
    if s.is_finite() && s > 0.0 {
        tokio::time::sleep(std::time::Duration::from_secs_f64(s)).await;
    } else {
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_stamp_reads_the_clock_once() {
        // just before a second boundary: the milliseconds belong to the same second (Python could print 34-000)
        assert_eq!(run_stamp_at(1_793_015_134.999_9), "20261026T114534-999");
        assert_eq!(run_stamp_at(1_793_015_135.0), "20261026T114535-000");
    }

    #[test]
    fn python_float_and_str() {
        assert_eq!(py_float(" 2.5e+3 "), Some(2500.0));
        assert_eq!(py_float("1_000"), Some(1000.0));
        assert_eq!(py_float("+Inf"), Some(f64::INFINITY));
        assert!(py_float("1__0").is_none() && py_float("").is_none() && py_float("0x1").is_none());
        assert_eq!(
            json_str(&serde_json::json!({"a": [1, 2.0, null, true]})),
            "{'a': [1, 2.0, None, True]}"
        );
        assert_eq!(gmtime(0.5, false), "1970-01-01T00:00:00Z");
        assert_eq!(gmtime(1_792_841_080.0, true), "20261024T112440");
    }
}
