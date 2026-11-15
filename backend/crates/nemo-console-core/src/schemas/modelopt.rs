//! Model Optimizer's `recipes.yaml` schema: [`parse_recipes`] and its constants (the lab tool's
//! `validate_recipes`). Moved unchanged from the backend's `adapters/modelopt.rs` (ADR-0022), which re-exports it;
//! the browser runs it through `crate::check`.

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

/// The compression methods the wrapper knows.
pub const METHODS: [&str; 3] = ["reap", "quantize", "minitron"];
pub const MAX_RECIPES_BYTES: usize = 256 * 1024;
/// The tool's `MAX_RECIPES`.
pub const MAX_RECIPES: usize = 64;

static RECIPE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9-]{0,39}\z").ok());
/// A Hugging Face repo id, as the tool's `MODEL_RE` fullmatches it (org/name).
static REPO_ID_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9._-]+\z").ok());

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

/// (path...) -> 1-based line, for top-level keys, recipe names and recipe fields. Never fails.
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
        if kv == "recipes"
            && let NodeKind::Map(recipes) = &vn.kind
        {
            for (pk, pv) in recipes {
                let (Some(pkn), Some(pvn)) = (doc.node(*pk), doc.node(*pv)) else {
                    continue;
                };
                let NodeKind::Scalar(pname) = &pkn.kind else {
                    continue;
                };
                out.insert(
                    vec!["recipes".to_owned(), pname.clone()],
                    pkn.start.line + 1,
                );
                if let NodeKind::Map(fields) = &pvn.kind {
                    for (fk, _) in fields {
                        if let Some(fkn) = doc.node(*fk)
                            && let NodeKind::Scalar(fname) = &fkn.kind
                        {
                            out.insert(
                                vec!["recipes".to_owned(), pname.clone(), fname.clone()],
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

/// One validated recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub name: String,
    pub method: String,
    pub base_model: String,
    pub output: String,
    pub preset: String,
    pub sparsity: Option<f64>,
    pub config: String,
    pub description: String,
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

/// recipes.yaml schema: the tool's REQUIRED_KEYS + EXTRA_KEYS; any other key is a hard error.
pub const RECIPE_KEYS: [&str; 7] = [
    "method",
    "base_model",
    "output",
    "description",
    "preset",
    "sparsity",
    "config",
];

fn recipe_keys() -> Vec<&'static str> {
    RECIPE_KEYS.to_vec()
}

/// `parse_recipes(text)`: (runnable recipes, diagnostics). The rules are the lab tool's `validate_recipes`, and
/// like the tool's `load_recipes` a file with ANY error runs nothing: the recipes come back empty while the
/// diagnostics name every error with its line. A YAML error is a diagnostic; a non-YAML exception PyYAML lets
/// through is returned as an error, as Python raises it.
pub fn parse_recipes(
    text: &str,
) -> Result<(IndexMap<String, Recipe>, Vec<Diagnostic>), AdapterError> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut recipes: IndexMap<String, Recipe> = IndexMap::new();
    if text.len() > MAX_RECIPES_BYTES {
        return Ok((
            recipes,
            vec![diag(
                Severity::Error,
                format!("recipes.yaml is larger than {MAX_RECIPES_BYTES} bytes"),
                None,
            )],
        ));
    }
    let doc = match pyyaml::safe_load_value(text) {
        Ok(d) => d,
        Err(e) if e.is_yaml() => {
            let msg = format!("YAML syntax: {}", pystr::head(&e.problem_or_str(), 200));
            return Ok((
                recipes,
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
            recipes,
            vec![diag(
                Severity::Error,
                "the document must be a mapping with 'version' and 'recipes'",
                None,
            )],
        ));
    };
    for (k, _) in top {
        if !matches!(k, Yv::Str(s) if s == "version" || s == "recipes") {
            let ks = k.py_str();
            diags.push(diag(
                Severity::Error,
                format!(
                    "unknown top-level key {}{}",
                    k.repr(),
                    close(&ks, &["version", "recipes"])
                ),
                line(&[&ks]),
            ));
        }
    }
    let version = doc.get("version");
    // the tool tests `doc.get("version") != 1`, and in Python 1 == 1.0 == True, so all three pass there too
    let version_ok = matches!(version, Some(Yv::Int(1)) | Some(Yv::Bool(true)))
        || matches!(version, Some(Yv::Float(f)) if *f == 1.0);
    if !version_ok {
        diags.push(diag(
            Severity::Error,
            "'version' must be 1",
            line(&["version"]),
        ));
    }
    let pairs = match doc.get("recipes") {
        Some(Yv::Dict(p)) if !p.is_empty() => p,
        _ => {
            diags.push(diag(
                Severity::Error,
                "'recipes' must be a mapping with at least one recipe",
                line(&["recipes"]),
            ));
            return Ok((recipes, diags));
        }
    };
    if pairs.len() > MAX_RECIPES {
        diags.push(diag(
            Severity::Error,
            format!(
                "at most {MAX_RECIPES} recipes are supported (found {})",
                pairs.len()
            ),
            line(&["recipes"]),
        ));
    }
    let keys = recipe_keys();
    let mut allowed = keys.clone();
    allowed.sort_unstable();
    let allowed = allowed.join(", ");
    for (name_v, body) in pairs.iter().take(MAX_RECIPES) {
        let pname = name_v.py_str();
        let name = match name_v {
            Yv::Str(s) if is(&RECIPE_RE, s) => s.clone(),
            _ => {
                diags.push(diag(
                    Severity::Error,
                    format!(
                        "recipe name {} must match [a-z0-9][a-z0-9-]{{0,39}}",
                        str_repr(&pname)
                    ),
                    line(&["recipes", &pname]),
                ));
                continue;
            }
        };
        let Yv::Dict(fields) = body else {
            diags.push(diag(
                Severity::Error,
                format!("recipe {} must be a mapping", str_repr(&name)),
                line(&["recipes", &pname]),
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
                    format!(
                        "{name}: unknown key {} (allowed: {allowed}){}",
                        k.repr(),
                        close(&ks, &keys)
                    ),
                    line(&["recipes", &pname, &ks]),
                ));
            }
        }
        let at = |key: &str| line(&["recipes", &pname, key]).or_else(|| line(&["recipes", &pname]));
        let text_of = |key: &str| match body.get(key) {
            Some(Yv::Str(v)) => Some(v.clone()),
            _ => None,
        };
        let mut err = |msg: String, key: &str| diags.push(diag(Severity::Error, msg, at(key)));
        let method = text_of("method");
        match &method {
            None => err(format!("{name}: 'method' is required"), "method"),
            Some(m) if !METHODS.contains(&m.as_str()) => err(
                format!("{name}: 'method' must be one of {}", METHODS.join(", ")),
                "method",
            ),
            Some(_) => {}
        }
        let base_model = text_of("base_model");
        match &base_model {
            None => err(format!("{name}: 'base_model' is required"), "base_model"),
            Some(m) if !is(&REPO_ID_RE, m) => err(
                format!("{name}: 'base_model' must be a Hugging Face repo id (org/name)"),
                "base_model",
            ),
            Some(_) => {}
        }
        let output = text_of("output");
        match &output {
            None => err(format!("{name}: 'output' is required"), "output"),
            Some(o) if !o.starts_with("/runs/") || o.chars().count() < 7 => err(
                format!("{name}: 'output' must be a path under /runs (the job's run dir)"),
                "output",
            ),
            Some(_) => {}
        }
        for (key, what) in [
            ("description", "a string"),
            ("preset", "a string"),
            ("config", "a string (a file name in the run dir)"),
        ] {
            if body.get(key).is_some() && text_of(key).is_none() {
                err(format!("{name}: '{key}' must be {what}"), key);
            }
        }
        // a number (not a bool) from 0 inclusive to 1 exclusive; optional for every method (REAP defaults it)
        #[allow(clippy::cast_precision_loss)]
        let sparsity = match body.get("sparsity") {
            None => None,
            Some(Yv::Int(i)) => Some(*i as f64),
            Some(Yv::Float(f)) => Some(*f),
            Some(_) => Some(f64::NAN),
        };
        if let Some(v) = sparsity
            && !(0.0..1.0).contains(&v)
        {
            err(
                format!("{name}: 'sparsity' must be a number from 0 (inclusive) to 1 (exclusive)"),
                "sparsity",
            );
        }
        if diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
            > before
        {
            continue;
        }
        recipes.insert(
            name.clone(),
            Recipe {
                name: name.clone(),
                method: method.unwrap_or_default(),
                base_model: base_model.unwrap_or_default(),
                output: output.unwrap_or_default(),
                preset: text_of("preset").unwrap_or_default(),
                sparsity,
                config: text_of("config").unwrap_or_default(),
                description: text_of("description").unwrap_or_default(),
            },
        );
    }
    // the tool's load_recipes refuses the whole file on any error, so nothing in it is runnable here either
    if diags.iter().any(|d| d.severity == Severity::Error) {
        recipes.clear();
    }
    Ok((recipes, diags))
}
