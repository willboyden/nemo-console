//! NeMo Evaluator's `suites.yaml` schema: [`validate_suites`], its constants, and [`check_suites`], the part of the
//! adapter's `validate_config` that needs no tool (size, YAML, schema, line numbers). Moved unchanged from the
//! backend's `adapters/evaluator.rs` (ADR-0022), which re-exports it; the browser runs it through `crate::check`.

use std::sync::LazyLock;

use regex::Regex;

use crate::adapter_error::AdapterError;
use crate::diagnostics::{Diagnostic, Severity};
use crate::services::common::redact;
use crate::services::pyfmt::{char_prefix, str_repr};
use crate::services::pyobj::{Doc, Py, PyExc, PyInt, py_splitlines};
use crate::services::pyyaml::safe_load;

/// `re.fullmatch(pattern)` as a compiled anchored regex (the backend's `support::full`).
#[allow(clippy::expect_used)] // static patterns: a typo is a programming error caught by every test run
fn full(pattern: &str) -> Regex {
    Regex::new(&format!(r"\A(?:{pattern})\z")).expect("static regex")
}

pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;

pub const SUPPORTED_BENCHMARKS: [&str; 6] =
    ["drop", "gsm8k", "math500", "mmlu", "mmlu_pro", "triviaqa"];
pub const REFUSED_BENCHMARKS: [(&str, &str); 11] = [
    (
        "humaneval",
        "needs a Docker sandbox to execute generated code",
    ),
    ("pinchbench", "needs a Docker sandbox"),
    ("terminal-bench-hard", "needs a Docker sandbox"),
    ("terminal-bench-hard-aa-split", "needs a Docker sandbox"),
    ("terminal-bench-v1", "needs a Docker sandbox"),
    ("nmp_harbor", "needs a Docker sandbox"),
    ("simpleqa", "needs an LLM judge"),
    ("healthbench", "needs an LLM judge"),
    (
        "xstest",
        "needs an LLM judge and downloads its CSV from github.com at run time",
    ),
    (
        "gpqa",
        "the dataset is gated on Hugging Face (needs an access token)",
    ),
    (
        "mgsm",
        "its dataset loader runs remote code (trust_remote_code)",
    ),
];
pub const RUN_KEYS: [&str; 5] = [
    "bench",
    "max_problems",
    "max_tokens",
    "temperature",
    "repeats",
];
pub const SUITE_KEYS: [&str; 3] = ["description", "model", "benchmarks"];
pub const MAX_SUITES: usize = 20;
pub const MAX_BENCHES: usize = 8;
pub const MAX_PROBLEMS: i128 = 5000;
pub const MAX_WORK: i128 = 20000;

static NAME_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z0-9][a-z0-9-]{0,39}"));
static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._/:@-]{0,127}"));

/// Python's Unicode `\s` (str.isspace) as a regex class body.
const PY_WS: &str = r"\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}";

fn small_int(v: Option<&Py>) -> Option<i128> {
    match v {
        Some(Py::Int(PyInt::Small(i))) => Some(*i),
        _ => None,
    }
}

/// `isinstance(v, int) and not isinstance(v, bool)`.
fn is_py_int(v: Option<&Py>) -> bool {
    matches!(v, Some(Py::Int(_)))
}

/// `_num(v)` on a YAML object: `float(v)` for an int or float (OverflowError for a huge int).
fn py_num(v: Option<&Py>) -> Result<Option<f64>, PyExc> {
    match v {
        Some(Py::Float(f)) => Ok(Some(*f)),
        Some(Py::Int(i)) => i
            .to_f64()
            .map(Some)
            .ok_or_else(|| PyExc::new("OverflowError", "int too large to convert to float")),
        _ => Ok(None),
    }
}

/// Sorted `str(k)` of the keys of `items` that are not in `allowed`.
fn unknown_keys(doc: &Doc, items: &[(Py, Py)], allowed: &[&str]) -> Result<Vec<String>, PyExc> {
    let mut out = Vec::new();
    for (k, _) in items {
        if let Py::Str(s) = k
            && allowed.contains(&s.as_str())
        {
            continue;
        }
        out.push(doc.str_of(k)?);
    }
    out.sort();
    Ok(out)
}

/// `validate_suites(doc)`: (message, key for the line lookup) for every schema error of a parsed suites.yaml.
pub fn validate_suites(doc: &Doc) -> Result<Vec<(String, Option<String>)>, PyExc> {
    let mut errs: Vec<(String, Option<String>)> = Vec::new();
    let Some(top) = doc.dict(&doc.root) else {
        return Ok(vec![(
            "top level must be a mapping with `version` and `suites`".to_owned(),
            None,
        )]);
    };
    for k in unknown_keys(doc, top, &["version", "suites"])? {
        errs.push((format!("unknown top-level key {}", str_repr(&k)), Some(k)));
    }
    let version = Doc::get(top, "version").cloned().unwrap_or(Py::None);
    if !doc.eq(&version, &Py::Int(PyInt::Small(1))) {
        errs.push(("`version` must be 1".to_owned(), Some("version".to_owned())));
    }
    let suites_v = Doc::get(top, "suites").cloned().unwrap_or(Py::None);
    let suites = match doc.dict(&suites_v) {
        Some(s) if !s.is_empty() => s,
        _ => {
            errs.push((
                "`suites` must be a non-empty mapping of suite name -> suite".to_owned(),
                Some("suites".to_owned()),
            ));
            return Ok(errs);
        }
    };
    if suites.len() > MAX_SUITES {
        errs.push((
            format!("at most {MAX_SUITES} suites"),
            Some("suites".to_owned()),
        ));
    }
    for (name, suite) in suites {
        let where_ = format!("suite {}", doc.repr(name)?);
        let key = doc.str_of(name)?;
        let name_ok = matches!(name, Py::Str(s) if NAME_RE.is_match(s));
        if !name_ok {
            errs.push((
                format!("{where_}: name must match [a-z0-9][a-z0-9-]{{0,39}}"),
                Some(key.clone()),
            ));
        }
        let Some(s) = doc.dict(suite) else {
            errs.push((format!("{where_}: must be a mapping"), Some(key.clone())));
            continue;
        };
        for k in unknown_keys(doc, s, &SUITE_KEYS)? {
            errs.push((
                format!(
                    "{where_}: unknown key {} (allowed: {})",
                    str_repr(&k),
                    SUITE_KEYS.join(", ")
                ),
                Some(k),
            ));
        }
        let desc_ok = match Doc::get(s, "description") {
            None => true,
            Some(Py::Str(d)) => d.chars().count() <= 200,
            Some(_) => false,
        };
        if !desc_ok {
            errs.push((
                format!("{where_}: description must be a string of at most 200 characters"),
                Some("description".to_owned()),
            ));
        }
        let model_ok = matches!(Doc::get(s, "model"), Some(Py::Str(m)) if MODEL_RE.is_match(m));
        if !model_ok {
            errs.push((
                format!(
                    "{where_}: `model` must be a router model alias (letters, digits and . _ / : @ -)"
                ),
                Some("model".to_owned()),
            ));
        }
        let benches_v = Doc::get(s, "benchmarks").cloned().unwrap_or(Py::None);
        let benches = match doc.list(&benches_v) {
            Some(b) if !b.is_empty() => b,
            _ => {
                errs.push((
                    format!("{where_}: `benchmarks` must be a non-empty list"),
                    Some("benchmarks".to_owned()),
                ));
                continue;
            }
        };
        if benches.len() > MAX_BENCHES {
            errs.push((
                format!("{where_}: at most {MAX_BENCHES} benchmarks"),
                Some("benchmarks".to_owned()),
            ));
        }
        let mut seen: Vec<String> = Vec::new();
        let mut work: i128 = 0;
        for (i, b) in benches.iter().take(MAX_BENCHES).enumerate() {
            let bw = format!("{where_} benchmark #{}", i + 1);
            let Some(bd) = doc.dict(b) else {
                errs.push((format!("{bw}: must be a mapping"), Some(key.clone())));
                continue;
            };
            for k in unknown_keys(doc, bd, &RUN_KEYS)? {
                errs.push((
                    format!(
                        "{bw}: unknown key {} (allowed: {})",
                        str_repr(&k),
                        RUN_KEYS.join(", ")
                    ),
                    Some(k),
                ));
            }
            match Doc::get(bd, "bench") {
                Some(Py::Str(bench)) => {
                    if let Some((_, why)) = REFUSED_BENCHMARKS.iter().find(|(n, _)| n == bench) {
                        errs.push((
                            format!("{bw}: {} is not supported — {why}", str_repr(bench)),
                            Some(bench.clone()),
                        ));
                    } else if !SUPPORTED_BENCHMARKS.contains(&bench.as_str()) {
                        errs.push((
                            format!(
                                "{bw}: unknown benchmark {} (supported: {})",
                                str_repr(bench),
                                SUPPORTED_BENCHMARKS.join(", ")
                            ),
                            Some(bench.clone()),
                        ));
                    } else if seen.contains(bench) {
                        errs.push((
                            format!("{bw}: {} is listed twice in this suite", str_repr(bench)),
                            Some(bench.clone()),
                        ));
                    } else {
                        seen.push(bench.clone());
                    }
                }
                _ => errs.push((format!("{bw}: `bench` is required"), Some(key.clone()))),
            }
            let mp_raw = Doc::get(bd, "max_problems");
            let rp_raw = Doc::get(bd, "repeats");
            let mut mp = if is_py_int(mp_raw) {
                small_int(mp_raw)
            } else {
                Some(0)
            };
            let mut rp = match rp_raw {
                None => Some(1),
                v if is_py_int(v) => small_int(v),
                _ => Some(1),
            };
            if !mp.is_some_and(|m| (1..=MAX_PROBLEMS).contains(&m)) {
                errs.push((
                    format!(
                        "{bw}: `max_problems` is required, an integer from 1 to {MAX_PROBLEMS}"
                    ),
                    Some(key.clone()),
                ));
                mp = Some(0);
            }
            if !rp.is_some_and(|r| (1..=8).contains(&r)) {
                errs.push((
                    format!("{bw}: `repeats` must be an integer from 1 to 8"),
                    Some("repeats".to_owned()),
                ));
                rp = Some(1);
            }
            let mt = Doc::get(bd, "max_tokens");
            if !matches!(mt, None | Some(Py::None))
                && !(is_py_int(mt) && small_int(mt).is_some_and(|m| (16..=32_768).contains(&m)))
            {
                errs.push((
                    format!("{bw}: `max_tokens` must be an integer from 16 to 32768"),
                    Some("max_tokens".to_owned()),
                ));
            }
            let t = Doc::get(bd, "temperature");
            if !matches!(t, None | Some(Py::None)) {
                let tv = match t {
                    Some(Py::Bool(_)) => None,
                    other => py_num(other)?,
                };
                if !tv.is_some_and(|x| (0.0..=2.0).contains(&x)) {
                    errs.push((
                        format!("{bw}: `temperature` must be a number from 0 to 2"),
                        Some("temperature".to_owned()),
                    ));
                }
            }
            work += mp.unwrap_or(0) * rp.unwrap_or(1);
        }
        if work > MAX_WORK {
            errs.push((
                format!("{where_}: max_problems x repeats sums to {work}; the limit is {MAX_WORK}"),
                Some(key.clone()),
            ));
        }
    }
    Ok(errs)
}

/// The line of the first `needle:` key (or `- bench: needle`) in `text`, 1-based (the adapter's `_line_of`).
#[must_use]
pub fn line_of(text: &str, needle: Option<&str>) -> Option<i64> {
    let needle = needle.filter(|n| !n.is_empty())?;
    let e = regex::escape(needle);
    let pat = Regex::new(&format!(
        r"^[{PY_WS}]*(?:-[{PY_WS}]+)?{e}[{PY_WS}]*:|^[{PY_WS}]*-[{PY_WS}]+bench:[{PY_WS}]*{e}\b"
    ))
    .ok()?;
    for (i, line) in py_splitlines(text).iter().enumerate() {
        if pat.is_match(line) {
            return i64::try_from(i + 1).ok();
        }
    }
    None
}

/// What [`check_suites`] found.
#[derive(Debug)]
pub enum SuitesCheck {
    /// The file was refused before the schema ran (too large, or a YAML error): the validation result is these
    /// diagnostics and nothing else.
    Refused(Vec<Diagnostic>),
    /// The parsed document and its schema errors (each with its line); the adapter adds its tool checks.
    Parsed(Doc, Vec<Diagnostic>),
}

/// The tool-free part of the evaluator adapter's `validate_config`: the size cap, the YAML load and
/// [`validate_suites`] with line numbers. An `Err` is a Python exception PyYAML or the schema lets escape.
pub fn check_suites(content: &str) -> Result<SuitesCheck, AdapterError> {
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Ok(SuitesCheck::Refused(vec![Diagnostic {
            severity: Severity::Error,
            message: "file is larger than 256 KiB".to_owned(),
            line: None,
        }]));
    }
    let doc = match safe_load(content) {
        Ok(d) => d,
        Err(e) if e.is_yaml() => {
            let problem = e
                .problem()
                .filter(|p| !p.is_empty())
                .unwrap_or("invalid YAML");
            let message = char_prefix(&redact(&format!("YAML error: {problem}")), 300).to_owned();
            let line = e
                .problem_mark()
                .and_then(|m| i64::try_from(m.line + 1).ok());
            return Ok(SuitesCheck::Refused(vec![Diagnostic {
                severity: Severity::Error,
                message,
                line,
            }]));
        }
        Err(e) => return Err(AdapterError::failed(e.class(), e.text())),
    };
    let py = |e: PyExc| AdapterError::failed(e.class, e.message);
    let diags: Vec<Diagnostic> = validate_suites(&doc)
        .map_err(py)?
        .into_iter()
        .map(|(m, key)| Diagnostic {
            severity: Severity::Error,
            message: char_prefix(&m, 300).to_owned(),
            line: line_of(content, key.as_deref()),
        })
        .collect();
    Ok(SuitesCheck::Parsed(doc, diags))
}
