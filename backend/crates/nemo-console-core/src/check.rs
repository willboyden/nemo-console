//! The browser's live config checks (ADR-0022), composed from the backend's own code. Contract:
//! `backend/PORTING.md` addendum A-10 ("WASM contract", version [`API`]). Every function takes strings and returns a
//! JSON document, so the WebAssembly wrapper (`crates/nemo-console-wasm`) is a one-line shim per function and the
//! native tests see exactly what the browser sees.
//!
//! * [`check_json`]`(system, file_id, format, text)`: what the backend's validate route would say about `text`,
//!   minus what needs a tool or the host (Switchyard's dry run, the evaluator wrapper and its prefetch state): the
//!   save-path refusals (NUL, [`find_secret`]), the real loader's syntax verdict, and the file's schema validator
//!   where the console has one.
//! * [`schema_constants_json`]`()`: the schema constants the Form view describes fields with.
//! * [`yaml_plain_type_json`]`(raw)`: how PyYAML types a plain (unquoted) scalar.
//! * [`roundtrip_json`]`(format, before, after, path, expected)`: after a Form edit, load both texts with the real
//!   loader and say whether the edited value reads back as typed and nothing else changed.
//!
//! The server stays the authority (ADR-0019 section 3): these verdicts are advisory and every Apply re-validates.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::adapter_error::AdapterError;
use crate::diagnostics::{Diagnostic, Severity};
use crate::schemas::{aiperf, evaluator, modelopt, relay, switchyard};
use crate::services::common::find_secret;
use crate::services::pyfmt::{float_repr, str_repr};
use crate::services::pytoml::{self, TomlVal};
use crate::services::pyyaml::{self, PyValue};

/// The contract version every answer carries as `api`; the frontend refuses an answer with another one.
pub const API: u32 = 1;

/// What the backend adds to Switchyard's check (it runs the binary).
const SWITCHYARD_ADDS: [&str; 1] = ["switchyard-server --dry-run"];
/// What the backend adds to the evaluator's check (the wrapper and the host's dataset cache).
const EVALUATOR_ADDS: [&str; 2] = [
    "evaluator-local suites --file",
    "the prefetched-dataset check",
];

/// The schema the console knows for one config file: its id (the frontend's label key) and what the backend runs on
/// top that the browser cannot.
#[must_use]
pub fn schema_of(system: &str, file_id: &str) -> Option<(&'static str, &'static [&'static str])> {
    match (system, file_id) {
        ("aiperf", "profiles") => Some(("aiperf_profiles", &[])),
        ("modelopt", "recipes") => Some(("modelopt_recipes", &[])),
        ("evaluator", "suites.yaml") => Some(("evaluator_suites", &EVALUATOR_ADDS)),
        ("switchyard", "routes") => Some(("switchyard_routes", &SWITCHYARD_ADDS)),
        ("relay", "routes") => Some(("relay_routes", &[])),
        _ => None,
    }
}

fn severity(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

/// One finding in the browser's report: the backend's [`Diagnostic`] plus where it came from.
fn finding(d: &Diagnostic, source: &str) -> Value {
    json!({ "severity": severity(d.severity), "line": d.line, "message": d.message, "source": source })
}

fn error(message: &str, line: Option<i64>, source: &str) -> Value {
    json!({ "severity": "error", "line": line, "message": message, "source": source })
}

/// 1-based line of a byte offset.
fn line_at(text: &str, byte: usize) -> i64 {
    let n = text.as_bytes()[..byte.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count();
    i64::try_from(n + 1).unwrap_or(i64::MAX)
}

/// The first line on which [`find_secret`] alone finds something, to place the marker. The verdict itself is the
/// whole-content scan (the one the save path runs); a secret that only shows across lines gets no line.
fn secret_line(text: &str) -> Option<i64> {
    text.split('\n')
        .position(|l| find_secret(l).is_some())
        .and_then(|i| i64::try_from(i + 1).ok())
}

/// The real loader's verdict on the file's syntax (`tomllib.loads`, PyYAML `compose`): `None` when it loads.
fn syntax_error(format: &str, text: &str) -> Option<(String, Option<i64>)> {
    match format {
        "toml" => pytoml::loads(text)
            .err()
            .map(|e| (format!("TOML syntax: {e}"), switchyard::line_of(&e.0))),
        "yaml" => pyyaml::compose(text).err().map(|e| {
            let line = e
                .problem_mark()
                .and_then(|m| i64::try_from(m.line + 1).ok());
            (format!("YAML syntax: {}", e.problem_or_str()), line)
        }),
        _ => None,
    }
}

/// The schema validator of one file as the backend runs it (`Err`: an exception it lets escape).
fn run_schema(system: &str, file_id: &str, text: &str) -> Result<Vec<Diagnostic>, AdapterError> {
    Ok(match (system, file_id) {
        ("aiperf", "profiles") => aiperf::parse_profiles(text)?.1,
        ("modelopt", "recipes") => modelopt::parse_recipes(text)?.1,
        ("evaluator", "suites.yaml") => match evaluator::check_suites(text)? {
            evaluator::SuitesCheck::Refused(d) | evaluator::SuitesCheck::Parsed(_, d) => d,
        },
        ("switchyard", "routes") => match switchyard::check_routes(text) {
            switchyard::RoutesCheck::Refused(d) | switchyard::RoutesCheck::Checked(d) => d,
        },
        ("relay", "routes") => relay::check_routes(text)?,
        _ => Vec::new(),
    })
}

/// Every live check for one draft, as a JSON value: `{api, schema, server_adds, syntax, diagnostics}`.
#[must_use]
pub fn check(system: &str, file_id: &str, format: &str, text: &str) -> Value {
    let mut out: Vec<Value> = Vec::new();
    // what the save path refuses before any validator runs (services/config_store.rs `check_content`)
    if let Some(i) = text.find('\0') {
        out.push(error(
            "content contains a NUL byte",
            Some(line_at(text, i)),
            "content",
        ));
    }
    if let Some(why) = find_secret(text) {
        out.push(error(
            &format!("refusing content that contains {why}"),
            secret_line(text),
            "secret",
        ));
    }
    let syntax = syntax_error(format, text);
    let schema = schema_of(system, file_id);
    if schema.is_some() {
        match run_schema(system, file_id, text) {
            // a file that does not load gets its validator's own syntax message, so it is tagged as syntax
            Ok(ds) => {
                let source = if syntax.is_some() { "syntax" } else { "schema" };
                out.extend(ds.iter().map(|d| finding(d, source)));
            }
            Err(e) => out.push(error(
                &format!("the backend's validator stops with {}", e.py_text()),
                None,
                "schema",
            )),
        }
    } else if let Some((message, line)) = &syntax {
        out.push(error(message, *line, "syntax"));
    }
    let syntax_state = match format {
        "toml" | "yaml" if syntax.is_some() => "error",
        "toml" | "yaml" => "ok",
        _ => "n/a",
    };
    json!({
        "api": API,
        "schema": schema.map(|s| s.0),
        "server_adds": schema.map_or(&[][..], |s| s.1),
        "syntax": syntax_state,
        "diagnostics": out,
    })
}

/// [`check`] as a JSON string (the WebAssembly export).
#[must_use]
pub fn check_json(system: &str, file_id: &str, format: &str, text: &str) -> String {
    check(system, file_id, format, text).to_string()
}

fn int_json(v: i128) -> Value {
    i64::try_from(v).map_or(Value::Null, Value::from)
}

/// The schema constants the Form view describes fields with, straight from the validators.
#[must_use]
pub fn schema_constants() -> Value {
    let int_keys: Vec<Value> = aiperf::INT_KEYS
        .iter()
        .map(|(k, lo, hi, req)| json!([k, int_json(*lo), int_json(*hi), req]))
        .collect();
    let refused: serde_json::Map<String, Value> = evaluator::REFUSED_BENCHMARKS
        .iter()
        .map(|(k, why)| ((*k).to_owned(), json!(why)))
        .collect();
    json!({
        "api": API,
        "aiperf": {
            "INT_KEYS": int_keys,
            "ENDPOINT_TYPES": aiperf::ENDPOINT_TYPES,
            "MAX_PROFILES": aiperf::MAX_PROFILES,
        },
        "modelopt": {
            "METHODS": modelopt::METHODS,
            "RECIPE_KEYS": modelopt::RECIPE_KEYS,
            "MAX_RECIPES": modelopt::MAX_RECIPES,
        },
        "evaluator": {
            "SUPPORTED": evaluator::SUPPORTED_BENCHMARKS,
            "REFUSED": refused,
            "SUITE_KEYS": evaluator::SUITE_KEYS,
            "RUN_KEYS": evaluator::RUN_KEYS,
            "MAX_SUITES": evaluator::MAX_SUITES,
            "MAX_BENCHES": evaluator::MAX_BENCHES,
            "MAX_PROBLEMS": int_json(evaluator::MAX_PROBLEMS),
            "MAX_WORK": int_json(evaluator::MAX_WORK),
        },
    })
}

/// [`schema_constants`] as a JSON string (the WebAssembly export).
#[must_use]
pub fn schema_constants_json() -> String {
    schema_constants().to_string()
}

/// How PyYAML 6 types a plain (unquoted) scalar: `{api, type, text}`. `type` is one of `null`, `bool`, `int`,
/// `float`, `date`, `string`, `special` (`=`, `<<` and words starting with `!`, `&`, `*`: tags the safe loader
/// refuses, or syntax); `text` is the value as Python prints it for bool/int/float (`True`, `493`, `inf`), else
/// the raw text. An int or float the resolver accepts but the constructor refuses (`0x_`) has `text: null`.
#[must_use]
pub fn yaml_plain_type(raw: &str) -> Value {
    let tag = pyyaml::resolver::resolve_scalar(raw, true);
    let ty = match tag.rsplit(':').next().unwrap_or("") {
        "null" => "null",
        "bool" => "bool",
        "int" => "int",
        "float" => "float",
        "timestamp" => "date",
        "str" => "string",
        _ => "special",
    };
    let text = match ty {
        "bool" | "int" | "float" => match pyyaml::safe_load_value(raw) {
            Ok(PyValue::Bool(b)) => json!(if b { "True" } else { "False" }),
            Ok(PyValue::Int(i)) => json!(i.to_string()),
            Ok(PyValue::BigInt(s)) => json!(s),
            Ok(PyValue::Float(f)) => json!(float_repr(f)),
            _ => Value::Null,
        },
        "null" => json!(""),
        _ => json!(raw),
    };
    json!({ "api": API, "type": ty, "text": text })
}

/// [`yaml_plain_type`] as a JSON string (the WebAssembly export).
#[must_use]
pub fn yaml_plain_type_json(raw: &str) -> String {
    yaml_plain_type(raw).to_string()
}

/// One step of a Form path: a mapping key or a list index.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Step {
    Index(usize),
    Key(String),
}

/// What the person typed, as the Form view encoded it: `type` one of `string`, `int`, `float`, `number` (an int or
/// a float), `bool`, `date`; `value` the typed text (`null` for a block string: any text), a boolean for `bool`.
#[derive(Debug, Clone, Deserialize)]
struct Expected {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    value: Value,
}

/// A loaded document of either format.
#[derive(Debug, Clone, PartialEq)]
enum Loaded {
    Toml(TomlVal),
    Yaml(PyValue),
}

fn load(format: &str, text: &str) -> Result<Loaded, String> {
    match format {
        "toml" => pytoml::loads(text)
            .map(|t| Loaded::Toml(TomlVal::Table(t)))
            .map_err(|e| format!("TOML syntax: {e}")),
        "yaml" => pyyaml::safe_load_value(text)
            .map(Loaded::Yaml)
            .map_err(|e| format!("YAML: {}", e.problem_or_str())),
        _ => Err(format!("format {format} has no Form view")),
    }
}

/// "Nothing else changed": the loaders' equality, except that a NaN equals a NaN in the same place. Python's `==`
/// says `nan != nan`, so a file holding `.nan` (YAML) or `nan` (TOML) anywhere would make every Form edit look like
/// it changed another value.
fn same_doc(a: &Loaded, b: &Loaded) -> bool {
    match (a, b) {
        (Loaded::Toml(x), Loaded::Toml(y)) => same_toml(x, y),
        (Loaded::Yaml(x), Loaded::Yaml(y)) => same_py(x, y),
        _ => false,
    }
}

fn same_toml(a: &TomlVal, b: &TomlVal) -> bool {
    match (a, b) {
        (TomlVal::Float(x), TomlVal::Float(y)) => x == y || (x.is_nan() && y.is_nan()),
        (TomlVal::Array(x), TomlVal::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_toml(p, q))
        }
        (TomlVal::Table(x), TomlVal::Table(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_toml(v, w)))
        }
        _ => a == b,
    }
}

fn same_py(a: &PyValue, b: &PyValue) -> bool {
    match (a, b) {
        (PyValue::Float(x), PyValue::Float(y)) => x == y || (x.is_nan() && y.is_nan()),
        (PyValue::List(x), PyValue::List(y)) | (PyValue::Tuple(x), PyValue::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_py(p, q))
        }
        (PyValue::Set(x), PyValue::Set(y)) => {
            x.len() == y.len() && x.iter().all(|p| y.iter().any(|q| same_py(p, q)))
        }
        (PyValue::Dict(x), PyValue::Dict(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, w)| same_py(k, k2) && same_py(v, w)))
        }
        _ => a == b,
    }
}

fn key_matches(k: &PyValue, key: &str) -> bool {
    match k {
        PyValue::Str(s) => s == key,
        other => other.py_str() == key,
    }
}

/// A value inside a [`Loaded`] document.
enum Slot<'a> {
    Toml(&'a mut TomlVal),
    Yaml(&'a mut PyValue),
}

/// The value at `path` (the last of duplicate keys, as both loaders keep it).
fn slot<'a>(doc: &'a mut Loaded, path: &[Step]) -> Option<Slot<'a>> {
    match doc {
        Loaded::Toml(v) => {
            let mut cur = v;
            for s in path {
                cur = match (cur, s) {
                    (TomlVal::Table(t), Step::Key(k)) => t.get_mut(k)?,
                    (TomlVal::Table(t), Step::Index(i)) => t.get_mut(&i.to_string())?,
                    (TomlVal::Array(a), Step::Index(i)) => a.get_mut(*i)?,
                    _ => return None,
                };
            }
            Some(Slot::Toml(cur))
        }
        Loaded::Yaml(v) => {
            let mut cur = v;
            for s in path {
                let key = match s {
                    Step::Key(k) => Some(k.clone()),
                    Step::Index(i) => Some(i.to_string()),
                };
                cur = match (cur, s) {
                    (PyValue::List(a), Step::Index(i)) => a.get_mut(*i)?,
                    (PyValue::Dict(d), _) => {
                        let key = key?;
                        &mut d.iter_mut().rev().find(|(k, _)| key_matches(k, &key))?.1
                    }
                    _ => return None,
                };
            }
            Some(Slot::Yaml(cur))
        }
    }
}

/// A loaded scalar, reduced to what the comparison needs.
enum Scalar {
    Str(String),
    Int(String),
    Float(f64),
    Bool(bool),
    Date,
    Null,
    Other(&'static str),
}

fn scalar_of(v: &Slot<'_>) -> Scalar {
    match v {
        Slot::Toml(t) => match &**t {
            TomlVal::Str(s) => Scalar::Str(s.clone()),
            TomlVal::Int(i) => Scalar::Int(i.to_string()),
            TomlVal::BigInt(s) => Scalar::Int(s.clone()),
            TomlVal::Float(f) => Scalar::Float(*f),
            TomlVal::Bool(b) => Scalar::Bool(*b),
            TomlVal::Date(_) | TomlVal::Time(_) | TomlVal::DateTime(..) => Scalar::Date,
            TomlVal::Array(_) => Scalar::Other("a list"),
            TomlVal::Table(_) => Scalar::Other("a table"),
        },
        Slot::Yaml(y) => match &**y {
            PyValue::Str(s) => Scalar::Str(s.clone()),
            PyValue::Int(i) => Scalar::Int(i.to_string()),
            PyValue::BigInt(s) => Scalar::Int(s.clone()),
            PyValue::Float(f) => Scalar::Float(*f),
            PyValue::Bool(b) => Scalar::Bool(*b),
            PyValue::Date { .. } | PyValue::DateTime(_) => Scalar::Date,
            PyValue::None => Scalar::Null,
            PyValue::Bytes(_) => Scalar::Other("binary data"),
            PyValue::List(_) | PyValue::Set(_) | PyValue::Tuple(_) => Scalar::Other("a list"),
            PyValue::Dict(_) => Scalar::Other("a mapping"),
        },
    }
}

fn describe(s: &Scalar) -> String {
    match s {
        Scalar::Str(v) => format!("the text {}", str_repr(v)),
        Scalar::Int(v) => format!("the whole number {v}"),
        Scalar::Float(f) => format!("the number {}", float_repr(*f)),
        Scalar::Bool(b) => (if *b { "true" } else { "false" }).to_owned(),
        Scalar::Date => "a date".to_owned(),
        Scalar::Null => "empty (null)".to_owned(),
        Scalar::Other(what) => (*what).to_owned(),
    }
}

/// Canonical decimal text of an integer the Form wrote (`+1_000` -> `1000`, `-0` -> `0`), or None.
fn canonical_int(s: &str) -> Option<String> {
    let t: String = s.trim().chars().filter(|c| *c != '_').collect();
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(&t)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    Some(match (digits.is_empty(), neg) {
        (true, _) => "0".to_owned(),
        (false, true) => format!("-{digits}"),
        (false, false) => digits.to_owned(),
    })
}

fn expected_matches(exp: &Expected, got: &Scalar) -> bool {
    let want_f64 = || {
        exp.value
            .as_str()
            .and_then(|s| s.trim().replace('_', "").parse::<f64>().ok())
    };
    let want_int = || exp.value.as_str().and_then(canonical_int);
    match (exp.ty.as_str(), got) {
        ("string", Scalar::Str(s)) => exp.value.as_str().is_none_or(|v| v == s),
        ("int" | "number", Scalar::Int(i)) => want_int().is_some_and(|v| &v == i),
        ("float" | "number", Scalar::Float(f)) => {
            want_f64().is_some_and(|w| w == *f || (w.is_nan() && f.is_nan()))
        }
        ("bool", Scalar::Bool(b)) => exp.value.as_bool() == Some(*b),
        ("date", Scalar::Date) => true,
        _ => false,
    }
}

fn verdict(ok: bool, problem: Option<&str>, got: Option<&str>) -> Value {
    json!({ "api": API, "ok": ok, "problem": problem, "got": got })
}

/// After a Form edit: does `after` load, does the value at `path` read back as `expected`, and is every other value
/// of the document what it was in `before`? `path_json` is a JSON array of keys and indexes; `expected_json` is
/// `{type, value}` (see [`Expected`]). Answer: `{api, ok, problem, got}`, `problem` one sentence or null.
#[must_use]
pub fn roundtrip(
    format: &str,
    before: &str,
    after: &str,
    path_json: &str,
    expected_json: &str,
) -> Value {
    let (Ok(path), Ok(exp)) = (
        serde_json::from_str::<Vec<Step>>(path_json),
        serde_json::from_str::<Expected>(expected_json),
    ) else {
        return verdict(
            false,
            Some("the edit could not be described to the checker"),
            None,
        );
    };
    let mut new_doc = match load(format, after) {
        Ok(d) => d,
        Err(e) => {
            return verdict(
                false,
                Some(&format!("the file would no longer load ({e})")),
                None,
            );
        }
    };
    let old_doc = load(format, before).ok();
    let Some(new_value) = slot(&mut new_doc, &path) else {
        return verdict(
            false,
            Some("the edited value is not where the Form wrote it"),
            None,
        );
    };
    let got = scalar_of(&new_value);
    let said = describe(&got);
    if !expected_matches(&exp, &got) {
        return verdict(
            false,
            Some(&format!("it reads back as {said}, not as typed")),
            Some(&said),
        );
    }
    // a file that did not load before has nothing to compare with: only the edited value is judged
    let Some(mut old_doc) = old_doc else {
        return verdict(true, None, Some(&said));
    };
    // put the new value into the old document: the two must then be equal (Python equality: the loaders' view)
    let replaced = match (new_value, slot(&mut old_doc, &path)) {
        (Slot::Toml(n), Some(Slot::Toml(o))) => {
            o.clone_from(n);
            true
        }
        (Slot::Yaml(n), Some(Slot::Yaml(o))) => {
            o.clone_from(n);
            true
        }
        _ => false,
    };
    if !replaced {
        return verdict(
            false,
            Some("the edited value was not in the file before the edit"),
            Some(&said),
        );
    }
    if !same_doc(&old_doc, &new_doc) {
        return verdict(
            false,
            Some("another value of the file changed too"),
            Some(&said),
        );
    }
    verdict(true, None, Some(&said))
}

/// [`roundtrip`] as a JSON string (the WebAssembly export).
#[must_use]
pub fn roundtrip_json(
    format: &str,
    before: &str,
    after: &str,
    path_json: &str,
    expected_json: &str,
) -> String {
    roundtrip(format, before, after, path_json, expected_json).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diags(v: &Value) -> Vec<(String, Option<i64>, String)> {
        v["diagnostics"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|d| {
                        (
                            d["source"].as_str().unwrap_or("").to_owned(),
                            d["line"].as_i64(),
                            d["message"].as_str().unwrap_or("").to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The drift ADR-0022 closes: the JS copy let `token: abc"defghijk` through; the backend refuses it, and now
    /// the browser runs the same scan.
    #[test]
    fn an_inner_quote_does_not_end_a_secret_value() {
        let text = "version: 1\ntoken: abc\"defghijk\n";
        assert_eq!(
            find_secret(text).as_deref(),
            Some("a literal value for 'token' (reference an env-var NAME instead)")
        );
        let d = diags(&check("aiperf", "profiles", "yaml", text));
        assert_eq!(d[0].0, "secret");
        assert_eq!(d[0].1, Some(2));
        assert!(
            d[0].2
                .starts_with("refusing content that contains a literal value for 'token'")
        );
    }

    #[test]
    fn nul_syntax_and_files_without_a_schema() {
        let v = check("dcgm", "counters", "text", "a\0b\n");
        assert_eq!(v["syntax"], "n/a");
        assert_eq!(v["schema"], Value::Null);
        assert_eq!(
            diags(&v)[0],
            (
                "content".to_owned(),
                Some(1),
                "content contains a NUL byte".to_owned()
            )
        );
        let v = check("guardrails", "x", "yaml", "a: [1,\n");
        assert_eq!(v["syntax"], "error");
        assert_eq!(diags(&v)[0].0, "syntax");
        let v = check("relay", "plugins", "toml", "a = 1\n");
        assert_eq!(v["syntax"], "ok");
        assert!(diags(&v).is_empty());
    }

    #[test]
    fn yaml_plain_types_follow_pyyaml() {
        let t = |s: &str| {
            let v = yaml_plain_type(s);
            (
                v["type"].as_str().unwrap_or("").to_owned(),
                v["text"].clone(),
            )
        };
        assert_eq!(t("yes"), ("bool".to_owned(), json!("True")));
        assert_eq!(t("0755"), ("int".to_owned(), json!("493")));
        assert_eq!(t("1e3"), ("string".to_owned(), json!("1e3")));
        assert_eq!(t("1.0e+3"), ("float".to_owned(), json!("1000.0")));
        assert_eq!(t("-.inf"), ("float".to_owned(), json!("-inf")));
        assert_eq!(t("2026-11-04"), ("date".to_owned(), json!("2026-11-04")));
        assert_eq!(t("~"), ("null".to_owned(), json!("")));
        assert_eq!(t("="), ("special".to_owned(), json!("=")));
        assert_eq!(t("<<"), ("special".to_owned(), json!("<<")));
        assert_eq!(t("1:30"), ("int".to_owned(), json!("90")));
    }

    #[test]
    fn roundtrip_reads_the_edit_back() {
        let before = "a: 1\nb: x\n";
        let ok = roundtrip(
            "yaml",
            before,
            "a: 2\nb: x\n",
            r#"["a"]"#,
            r#"{"type":"int","value":"2"}"#,
        );
        assert_eq!(ok["ok"], true, "{ok}");
        // a plain `yes` is a boolean, not the text the person typed
        let bad = roundtrip(
            "yaml",
            before,
            "a: 1\nb: yes\n",
            r#"["b"]"#,
            r#"{"type":"string","value":"yes"}"#,
        );
        assert_eq!(bad["ok"], false);
        assert_eq!(bad["problem"], "it reads back as true, not as typed");
        let other = roundtrip(
            "yaml",
            before,
            "a: 3\nb: y\n",
            r#"["a"]"#,
            r#"{"type":"int","value":"3"}"#,
        );
        assert_eq!(other["problem"], "another value of the file changed too");
        let t = roundtrip(
            "toml",
            "[x]\nn = 1\n",
            "[x]\nn = 1.5\n",
            r#"["x","n"]"#,
            r#"{"type":"number","value":"1.5"}"#,
        );
        assert_eq!(t["ok"], true, "{t}");
        let broken = roundtrip(
            "toml",
            "n = 1\n",
            "n = \n",
            r#"["n"]"#,
            r#"{"type":"int","value":"1"}"#,
        );
        assert!(
            broken["problem"]
                .as_str()
                .unwrap_or("")
                .starts_with("the file would no longer load")
        );
    }

    /// A NaN elsewhere in the file is not "another value changed" (Python's `nan != nan`); a real change still is.
    #[test]
    fn roundtrip_treats_an_untouched_nan_as_unchanged() {
        let int2 = r#"{"type":"int","value":"2"}"#;
        let y = roundtrip(
            "yaml",
            "a: 1\nb: .nan\n",
            "a: 2\nb: .nan\n",
            r#"["a"]"#,
            int2,
        );
        assert_eq!(y["ok"], true, "{y}");
        let t = roundtrip(
            "toml",
            "a = 1\nb = nan\n",
            "a = 2\nb = nan\n",
            r#"["a"]"#,
            int2,
        );
        assert_eq!(t["ok"], true, "{t}");
        let nested = roundtrip(
            "yaml",
            "a: 1\nb: {c: [.nan, 1]}\n",
            "a: 2\nb: {c: [.nan, 1]}\n",
            r#"["a"]"#,
            int2,
        );
        assert_eq!(nested["ok"], true, "{nested}");
        let other = roundtrip(
            "yaml",
            "a: 1\nb: .nan\n",
            "a: 2\nb: 1.0\n",
            r#"["a"]"#,
            int2,
        );
        assert_eq!(other["problem"], "another value of the file changed too");
        let self_nan = roundtrip(
            "yaml",
            "a: 1\nb: .nan\n",
            "a: 1\nb: .nan\n",
            r#"["b"]"#,
            r#"{"type":"float","value":"nan"}"#,
        );
        assert_eq!(self_nan["ok"], true, "{self_nan}");
    }
}
