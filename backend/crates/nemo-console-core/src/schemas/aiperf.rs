//! AIPerf's `profiles.yaml` schema: [`parse_profiles`] and its constants. Moved unchanged from the backend's
//! `adapters/aiperf.rs` (ADR-0022), which re-exports it; the browser runs it through `crate::check`.

use std::collections::HashMap;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::adapter_error::AdapterError;
use crate::diagnostics::{Diagnostic, Severity};
use crate::services::pyfmt::str_repr;
use crate::services::pystr;
use crate::services::pyyaml::{self, NodeKind, PyValue as Yv, YamlError};

fn is(re: &LazyLock<Option<Regex>>, s: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(s))
}

pub const MAX_PROFILES_BYTES: usize = 256 * 1024;
pub const MAX_PROFILES: usize = 32;

pub const ENDPOINT_TYPES: [&str; 2] = ["chat", "completions"];

static PRESET_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9-]{0,39}\z").ok());
static MODEL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._:/@+\-]{0,127}\z").ok());

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

pub fn diag(severity: Severity, message: impl Into<String>, line: Option<usize>) -> Diagnostic {
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
