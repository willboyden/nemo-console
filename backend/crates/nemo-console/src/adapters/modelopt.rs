//! Model Optimizer adapter: NVIDIA Model Optimizer (an offline model-compression CLI, not a service) as a
//! managed system. OWNER: workstream B.
//!
//! Facts this adapter is built on (the lab wrapper, `clients/modelopt/`):
//!   * `modelopt-local` is the lab wrapper (NVIDIA Model Optimizer 0.47.0); `--version` prints "0.47.0". The
//!     wrapper owns the model store, the recipe file and the offline policy; the console passes no env and
//!     never sees a key.
//!   * `modelopt-local run --job-id ID --recipe NAME --run-dir DIR` starts ONE compression job and returns
//!     at once: the job itself runs in the background and writes DIR/run.json (verdict "running", later the
//!     final verdict). The console owns DIR (under its own state dir) and reads that file; it never loads a
//!     model and never writes a checkpoint itself.
//!   * A finished job writes the verdict, the duration and the size numbers. A job that dies keeps
//!     "running": the wrapper cannot know either, so the console does not guess.
//!   * recipes.yaml is this console's own schema: validated here, authoritative.
//!   * Compression is slow (hours) and takes a GPU, so a job is a disruptive action.
//!
//! Contract with the wrapper (the run.json the wrapper writes and the console reads):
//!   { "job_id": str, "recipe": str, "method": str, "base_model": str, "output": str,
//!     "verdict": "running"|"succeeded"|"failed"|"error", "ts": float, "duration_ms": int,
//!     "metrics": { "output_bytes": int, "base_bytes": int, "ratio": float },
//!     "error": str }
//! A missing key is simply absent; the console never fabricates a number.

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
    Diagnostic, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, PostCheck, Problem,
    Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::pyfmt::str_repr;
use crate::services::pyyaml::{self, NodeKind, PyValue as Yv, YamlError};
use crate::services::{plumbing as wsb, pystr};

/// `ModelOptAdapter.label` in Python.
pub const LABEL: &str = "Model Optimizer";
pub const LAUNCHER: &str = "modelopt-local";
/// The NVIDIA Model Optimizer version the lab wrapper pins.
pub const PINNED_VERSION: &str = "0.47.0";
pub const VERSION_TTL: f64 = 300.0;
/// The runner caps `modelopt-local run` at this; the wrapper returns at once, the job runs on its own.
pub const RUN_TIMEOUT: f64 = 600.0;
pub const RUN_MAX_BYTES: usize = 65_536;
pub const KEEP_JOBS: usize = 50;
pub const EVENT_KIND: &str = "modelopt.job";
pub const RECIPES_ID: &str = "recipes";
pub const VERDICTS: [&str; 4] = ["running", "succeeded", "failed", "error"];
/// The compression methods the wrapper knows.
pub const METHODS: [&str; 3] = ["reap", "quantize", "minitron"];
pub const MAX_RECIPES_BYTES: usize = 256 * 1024;
pub const MAX_RECIPES: usize = 32;
/// The cap for the run.json the console reads (PORTING D-27 style cap).
pub const MAX_RUN_BYTES: u64 = 64 * 1024;
pub const LAB_INSTALLER: &str = "clients/modelopt/install.sh";
pub const LAB_RECIPES: &str = "clients/modelopt/recipes.yaml";

static RUN_ID_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A\d{8}T\d{6}-\d{3}-[a-z0-9][a-z0-9-]{0,39}\z").ok());
static RECIPE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9-]{0,39}\z").ok());
static MODEL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._:/@+\-]{0,127}\z").ok());

fn is(re: &LazyLock<Option<Regex>>, s: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(s))
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

/// The recipes file: the console's own schema, and the only thing that reads it.
fn recipes_spec(has_path: bool) -> ConfigFileSpec {
    ConfigFileSpec {
        id: RECIPES_ID.into(),
        label: "Compression recipes".into(),
        description: "Named compression recipes: the method (reap expert pruning, quantize or minitron), the base model it reads, the calibration data and the checkpoint it writes. Each recipe drives one offline job; a job takes a GPU for hours.".into(),
        display_path: if has_path { "clients/modelopt/recipes.yaml".into() } else { RECIPES_ID.into() },
        format: "yaml".into(),
        editable: true,
        why_readonly: None,
        validators: vec!["nemo-console modelopt recipes schema".into()],
        authoritative: true,
        apply_effect: "takes effect on the next job; nothing to restart".into(),
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
    pub calibration: String,
    pub scheme: String,
    pub sparsity: Option<f64>,
    pub description: String,
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

/// recipes.yaml schema: the keys a recipe may carry.
pub const RECIPE_KEYS: [&str; 8] = [
    "description",
    "method",
    "base_model",
    "output",
    "calibration",
    "scheme",
    "sparsity",
    "version",
];

fn recipe_keys() -> Vec<&'static str> {
    RECIPE_KEYS.to_vec()
}

/// `parse_recipes(text)`: (valid recipes, diagnostics); a recipe with an error is dropped. A YAML error is a
/// diagnostic; a non-YAML exception PyYAML lets through is returned as an error, as Python raises it.
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
    let version_ok =
        matches!(version, Some(Yv::Int(1))) || matches!(version, Some(Yv::Float(f)) if *f == 1.0);
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
                recipes.len()
            ),
            line(&["recipes"]),
        ));
    }
    let keys = recipe_keys();
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
                    format!("{name}: unknown key {}{}", k.repr(), close(&ks, &keys)),
                    line(&["recipes", &pname, &ks]),
                ));
            }
        }
        let method = body
            .get("method")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        let method_ok = matches!(&method, Yv::Str(m) if METHODS.contains(&m.as_str()));
        if !method_ok {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'method' must be one of {}", METHODS.join(", ")),
                line(&["recipes", &pname, "method"]),
            ));
        }
        let base_model = body
            .get("base_model")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        if !matches!(&base_model, Yv::Str(m) if is(&MODEL_RE, m)) {
            diags.push(diag(
                Severity::Error,
                format!(
                    "{name}: 'base_model' is required: a model name (letters, digits and . _ : / @ + -)"
                ),
                line(&["recipes", &pname, "base_model"]),
            ));
        }
        let output = body
            .get("output")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        if !matches!(&output, Yv::Str(o) if is(&MODEL_RE, o)) {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'output' is required: the name of the checkpoint it writes"),
                line(&["recipes", &pname, "output"]),
            ));
        }
        let scheme = body
            .get("scheme")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        let calibration = body
            .get("calibration")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        let desc = body
            .get("description")
            .cloned()
            .unwrap_or_else(|| Yv::Str(String::new()));
        let desc_ok = matches!(&desc, Yv::Str(s) if s.chars().count() <= 300 && !s.contains('\n'));
        if !desc_ok {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'description' must be a single line of at most 300 characters"),
                line(&["recipes", &pname, "description"]),
            ));
        }
        let sparsity = body.get("sparsity").cloned();
        let sparsity_ok = match sparsity {
            None => true,
            Some(Yv::Float(f)) if (0.0..=1.0).contains(&f) => true,
            Some(Yv::Int(i)) if (0..=1).contains(&i) => true,
            _ => false,
        };
        if !sparsity_ok {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'sparsity' must be a number from 0 to 1"),
                line(&["recipes", &pname, "sparsity"]),
            ));
        }
        let method_str = method.py_str();
        if method_str == "reap" && sparsity.is_none() {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'sparsity' is required for the reap method"),
                line(&["recipes", &pname, "sparsity"]),
            ));
        }
        if method_str == "quantize" && !matches!(&scheme, Yv::Str(s) if !s.is_empty()) {
            diags.push(diag(
                Severity::Error,
                format!("{name}: 'scheme' is required for the quantize method"),
                line(&["recipes", &pname, "scheme"]),
            ));
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
                method: method_str,
                base_model: base_model.py_str(),
                output: output.py_str(),
                calibration: calibration.py_str(),
                scheme: scheme.py_str(),
                sparsity: sparsity.and_then(|v| match v {
                    Yv::Float(f) => Some(f),
                    Yv::Int(i) => Some(i as f64),
                    _ => None,
                }),
                description: desc.py_str(),
            },
        );
    }
    Ok((recipes, diags))
}

/// The exact modelopt-local argv for a job. The wrapper owns the model store, the calibration data and the
/// offline policy; nothing secret is, or can be, in this list.
#[must_use]
pub fn render_argv(job_id: &str, recipe: &str, run_dir: &Path) -> Vec<String> {
    wsb::argv(&[
        LAUNCHER,
        "run",
        "--job-id",
        job_id,
        "--recipe",
        recipe,
        "--run-dir",
        &run_dir.to_string_lossy(),
    ])
}

/// The few numbers the console keeps from a finished job. Missing keys are simply absent.
#[must_use]
pub fn summarize(doc: &Map<String, Value>) -> IndexMap<String, f64> {
    let mut out = IndexMap::new();
    let m = doc.get("metrics").and_then(Value::as_object);
    if let Some(m) = m {
        for key in ["output_bytes", "base_bytes", "ratio"] {
            if let Some(v) = wsb::json_num(m.get(key)) {
                out.insert(key.to_owned(), v);
            }
        }
    }
    if let Some(v) = wsb::json_num(doc.get("duration_ms")) {
        out.insert("duration_ms".to_owned(), v);
    }
    out
}

/// One line about a job, for the summary and the event stream.
#[must_use]
pub fn headline(meta: &Map<String, Value>) -> String {
    let mut parts = vec![get_or(meta, "recipe", "?"), get_or(meta, "method", "?")];
    parts.push(get_or(meta, "verdict", "?"));
    if let Some(c) = wsb::json_num(meta.get("duration_ms")) {
        parts.push(format!("{:.1} min", c / 60_000.0));
    }
    let m = meta.get("metrics").and_then(Value::as_object);
    if let Some(m) = m
        && let Some(v) = wsb::json_num(m.get("output_bytes"))
    {
        parts.push(format!("{:.2} GiB", v / 1_073_741_824.0));
    }
    if let Some(m) = m
        && let Some(v) = wsb::json_num(m.get("ratio"))
    {
        parts.push(format!("{:.1}% of the base", v * 100.0));
    }
    pystr::head(&parts.join(" · "), 300).to_owned()
}

fn get_or(meta: &Map<String, Value>, k: &str, default: &str) -> String {
    meta.get(k)
        .map_or_else(|| default.to_owned(), wsb::json_str)
}

/// The report of one job: what it was, how it ended and what it produced.
#[must_use]
pub fn report_text(meta: Option<&Map<String, Value>>, job_id: &str) -> (bool, String) {
    let Some(meta) = meta else {
        return (false, "unknown job id".to_owned());
    };
    let mut lines = vec![format!("Job {job_id} · {}", headline(meta)), String::new()];
    let verdict = get_or(meta, "verdict", "?");
    if verdict == "running" {
        lines.push("Running. This report fills in when the job ends.".to_owned());
        lines.push(String::new());
    }
    if verdict == "failed" || verdict == "error" {
        lines.push("The job did not finish:".to_owned());
        lines.push(get_or(meta, "error", ""));
        lines.push(String::new());
    }
    lines.push(format!(
        "Recipe {} ({}): base model {}, writes {}.",
        get_or(meta, "recipe", "?"),
        get_or(meta, "method", "?"),
        get_or(meta, "base_model", "?"),
        get_or(meta, "output", "?")
    ));
    if let Some(m) = meta
        .get("metrics")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
    {
        let cell = |key: &str| match wsb::json_num(m.get(key)) {
            Some(v) => format!("{v:.3}"),
            None => "-".to_owned(),
        };
        lines.push(String::new());
        lines.push(format!(
            "Output {} bytes · base {} bytes · size {} of the base.",
            cell("output_bytes"),
            cell("base_bytes"),
            cell("ratio")
        ));
    }
    (true, lines.join("\n"))
}

/// (checked at, launcher, version)
type VersionCache = (Instant, Option<String>, Option<String>);
/// (recipes, diagnostics, file found)
type Recipes = (IndexMap<String, Recipe>, Vec<Diagnostic>, bool);

struct Inner {
    ctx: AdapterContext,
    version_cache: Mutex<Option<VersionCache>>,
    follow_interval: Mutex<f64>,
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

fn problem(code: &str, message: &str, fix: &str) -> Vec<Problem> {
    vec![Problem::new(
        Severity::Error,
        code,
        message,
        Some(fix.to_owned()),
    )]
}

/// The Model Optimizer adapter. Cheap to clone handles share one state.
pub struct ModelOptAdapter {
    inner: Arc<Inner>,
}

impl Inner {
    fn recipes_path(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("modelopt_recipes")
    }

    fn dir(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("modelopt_dir")
    }

    fn jobs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("modelopt").join("jobs")
    }

    /// A lab helper: `rel`'s file under the `modelopt_dir` layout dir, else `rel` under the repo root.
    fn lab_file(&self, rel: &str) -> Option<PathBuf> {
        let name = Path::new(rel)
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_default();
        let cands = [
            self.dir().map(|b| b.join(&name)),
            self.ctx.paths.get("repo").map(|r| r.join(rel)),
        ];
        cands.into_iter().flatten().find(|p| p.exists())
    }

    fn install_fix(&self) -> String {
        match self.lab_file(LAB_INSTALLER) {
            Some(lab) => format!(
                "{}   # pinned NVIDIA Model Optimizer {PINNED_VERSION} + the modelopt-local wrapper",
                lab.display()
            ),
            None => format!(
                "run clients/modelopt/install.sh in the lab repository (pinned Model Optimizer {PINNED_VERSION}; it links the modelopt-local wrapper onto PATH)"
            ),
        }
    }

    fn recipes_fix(&self) -> String {
        match self.lab_file(LAB_RECIPES) {
            Some(lab) => format!("{}   # the recipes this console runs", lab.display()),
            None => "restore clients/modelopt/recipes.yaml from the lab repository (git checkout -- clients/modelopt/recipes.yaml)".to_owned(),
        }
    }

    async fn run(&self, argv: &[String], opts: RunOpts) -> CommandResult {
        wsb::run(&self.ctx, argv, opts).await
    }

    /// The launcher and its version, TTL-cached: the version probe is one process.
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
            let first = pystr::splitlines(pystr::strip(&r.stdout))
                .first()
                .map(|l| pystr::head(l, 40).to_owned());
            launcher = Some(LAUNCHER.to_owned());
            version = first;
        }
        *lock(&self.version_cache) = Some((Instant::now(), launcher.clone(), version.clone()));
        (launcher, version)
    }

    /// (recipes, diagnostics, file_found). A missing or unreadable file is zero recipes.
    fn read_recipes(&self) -> Result<Recipes, AdapterError> {
        let Some(path) = self.recipes_path() else {
            return Ok((IndexMap::new(), Vec::new(), false));
        };
        if path.is_symlink() || !path.is_file() {
            return Ok((IndexMap::new(), Vec::new(), false));
        }
        let too_large = std::fs::metadata(path).map(|m| m.len() > MAX_RECIPES_BYTES as u64);
        match too_large {
            Ok(true) => {
                return Ok((
                    IndexMap::new(),
                    vec![diag(Severity::Error, "recipes.yaml is too large", None)],
                    true,
                ));
            }
            Ok(false) => {}
            Err(_) => {
                return Ok((
                    IndexMap::new(),
                    vec![diag(Severity::Error, "recipes.yaml cannot be read", None)],
                    true,
                ));
            }
        }
        let Ok(Ok(text)) = std::fs::read(path).map(String::from_utf8) else {
            return Ok((
                IndexMap::new(),
                vec![diag(Severity::Error, "recipes.yaml cannot be read", None)],
                true,
            ));
        };
        let (r, d) = parse_recipes(&text)?;
        Ok((r, d, true))
    }

    // ── stored jobs ─────────────────────────────────────────────────────────────────────────────
    fn job_dirs(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.jobs_dir()) else {
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

    fn load_meta(&self, job_dir: &Path) -> Option<Map<String, Value>> {
        let p = job_dir.join("run.json");
        if p.is_symlink() {
            return None;
        }
        if std::fs::metadata(&p).ok()?.len() > MAX_RUN_BYTES {
            return None;
        }
        wsb::load_json_dict(&p)
    }

    fn metas(&self, limit: usize) -> Vec<Map<String, Value>> {
        self.job_dirs()
            .iter()
            .take(limit)
            .filter_map(|d| self.load_meta(d))
            .collect()
    }

    fn event(&self, meta: &Map<String, Value>) -> SystemEvent {
        let mut data = Map::new();
        for k in [
            "job_id",
            "recipe",
            "method",
            "base_model",
            "output",
            "verdict",
            "duration_ms",
        ] {
            data.insert(k.to_owned(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        let m = meta.get("metrics").and_then(Value::as_object);
        for k in ["output_bytes", "base_bytes", "ratio"] {
            if let Some(v) = m.and_then(|m| wsb::json_num(m.get(k))) {
                data.insert(k.to_owned(), json!(v));
            }
        }
        SystemEvent {
            ts: wsb::json_num(meta.get("ts")).unwrap_or(0.0),
            system: SystemId::Modelopt,
            kind: EVENT_KIND.to_owned(),
            summary: headline(meta),
            data,
        }
    }

    fn list_jobs(&self, limit: usize) -> Vec<SystemEvent> {
        self.metas(limit).iter().map(|m| self.event(m)).collect()
    }

    /// The newest job that finished well, with its numbers.
    fn finished(&self) -> Option<Map<String, Value>> {
        self.metas(KEEP_JOBS)
            .into_iter()
            .find(|m| get_or(m, "verdict", "") == "succeeded")
    }
}

#[async_trait]
impl SystemAdapter for ModelOptAdapter {
    fn id(&self) -> SystemId {
        SystemId::Modelopt
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let inner = &self.inner;
        let (launcher, version) = inner.launcher().await;
        let (recipes, diags, found) = inner.read_recipes()?;
        let jobs = inner.metas(1);
        let mut problems: Vec<Problem> = Vec::new();
        let mut components = vec![Component {
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
                    "{l} {}: offline compression of a pinned checkpoint; the console starts jobs and reads their results",
                    version.clone().unwrap_or_default()
                ),
                None => "modelopt-local is not on PATH (installed by clients/modelopt/install.sh)"
                    .to_owned(),
            }),
        }];
        let running = inner
            .metas(KEEP_JOBS)
            .iter()
            .filter(|m| get_or(m, "verdict", "") == "running")
            .count();
        if running > 0 {
            components.push(Component {
                kind: "process".into(),
                name: format!("{running} running job(s)"),
                state: "running".into(),
                image: None,
                detail: Some(
                    "a compression job writes its verdict into its own run dir".to_owned(),
                ),
            });
        }
        if launcher.is_none() {
            problems.push(Problem::new(
                Severity::Error,
                "modelopt.not_installed",
                "Model Optimizer is not installed (or modelopt-local is not on the console's PATH).",
                Some(inner.install_fix()),
            ));
        }
        if !found {
            problems.push(Problem::new(
                Severity::Warn,
                "modelopt.no_recipes",
                format!(
                    "No recipes file at {}: there is nothing to run.",
                    inner
                        .recipes_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "None".into())
                ),
                Some(inner.recipes_fix()),
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
                "modelopt.recipes_invalid",
                format!("recipes.yaml has {} error(s); first: {}{at}", bad.len(), first.message),
                Some("fix it on the Configure tab (the validator names each line) or restore clients/modelopt/recipes.yaml with git".to_owned()),
            ));
        }
        if let Some(last) = jobs.first()
            && get_or(last, "verdict", "") == "failed"
        {
            problems.push(Problem::new(
                Severity::Warn,
                "modelopt.last_job_failed",
                format!(
                    "The last job failed: {}",
                    get_or(last, "error", "no reason recorded")
                ),
                Some("the job report has the wrapper's own error text".to_owned()),
            ));
        }
        let health = if launcher.is_none() {
            Health::Down
        } else if !found || !bad.is_empty() {
            Health::Degraded
        } else {
            Health::Up
        };
        let mut summary = format!("{} recipe(s)", recipes.len());
        if let Some(m) = jobs.first() {
            summary.push_str(&format!(" · last job: {}", headline(m)));
        }
        Ok(SystemStatus {
            system: SystemId::Modelopt,
            health,
            version,
            summary: pystr::head(&summary, 300).to_owned(),
            endpoints: vec![],
            components,
            problems,
            checked_at: now_ts(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let now = now_ts();
        let inner = &self.inner;
        let metas = inner.metas(KEEP_JOBS);
        #[allow(clippy::cast_precision_loss)]
        let mut points = vec![MetricPoint {
            key: "mo.jobs".into(),
            value: metas.len() as f64,
            unit: "1".into(),
            labels: IndexMap::new(),
        }];
        if !metas.is_empty() {
            #[allow(clippy::cast_precision_loss)]
            let running = metas
                .iter()
                .filter(|m| get_or(m, "verdict", "") == "running")
                .count();
            points.push(MetricPoint {
                key: "mo.live_running".into(),
                value: running as f64,
                unit: "1".into(),
                labels: IndexMap::new(),
            });
            for verdict in VERDICTS {
                let n = metas
                    .iter()
                    .filter(|m| get_or(m, "verdict", "") == verdict)
                    .count();
                let mut labels = IndexMap::new();
                labels.insert("verdict".to_owned(), verdict.to_owned());
                #[allow(clippy::cast_precision_loss)]
                points.push(MetricPoint {
                    key: "mo.jobs_by_verdict".into(),
                    value: n as f64,
                    unit: "1".into(),
                    labels,
                });
            }
        }
        if let Some(m) = inner.finished()
            && let Some(metrics) = m.get("metrics").and_then(Value::as_object)
        {
            for (key, name, unit) in [
                ("output_bytes", "mo.output_bytes", "B"),
                ("ratio", "mo.compression_ratio", "ratio"),
            ] {
                if let Some(v) = wsb::json_num(metrics.get(key)) {
                    points.push(MetricPoint {
                        key: name.into(),
                        value: v,
                        unit: unit.into(),
                        labels: IndexMap::new(),
                    });
                }
            }
            if let Some(v) = wsb::json_num(m.get("duration_ms")) {
                points.push(MetricPoint {
                    key: "mo.duration_s".into(),
                    value: v / 1000.0,
                    unit: "s".into(),
                    labels: IndexMap::new(),
                });
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Modelopt,
            ts: now,
            available: true,
            points,
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(self.inner.list_jobs(limit.min(KEEP_JOBS)))
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        let inner = Arc::clone(&self.inner);
        Box::pin(async_stream::stream! {
            // keyed by (job, verdict): a job is announced when it starts AND when it finishes
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            let mut first = true;
            loop {
                let mut events = inner.list_jobs(50);
                events.sort_by(|a, b| a.ts.total_cmp(&b.ts));
                for ev in events {
                    let key = (
                        wsb::json_get_str(ev.data.get("job_id")),
                        wsb::json_get_str(ev.data.get("verdict")),
                    );
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
        Ok(vec![recipes_spec(self.inner.recipes_path().is_some())])
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        match self.inner.recipes_path() {
            Some(p) if file_id == RECIPES_ID && !p.is_symlink() => Ok(p.clone()),
            _ => Err(AdapterError::UnknownId(file_id.to_owned())),
        }
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.config_path(file_id)?;
        let nf = || AdapterError::FileNotFound(file_id.to_owned());
        let md = std::fs::metadata(&p).map_err(|_| nf())?;
        if md.len() > MAX_RECIPES_BYTES as u64 {
            return Err(nf());
        }
        let bytes = std::fs::read(&p).map_err(|_| nf())?;
        let content = String::from_utf8(bytes).map_err(|_| {
            AdapterError::failed(
                "UnicodeDecodeError",
                "'utf-8' codec can't decode recipes.yaml",
            )
        })?;
        let mtime = std::fs::metadata(&p)
            .ok()
            .and_then(|m| crate::services::config_store::mtime_f64(&m))
            .ok_or_else(nf)?;
        Ok(ConfigFile {
            spec: recipes_spec(true),
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
        let (_, diags) = parse_recipes(content)?;
        Ok(ValidationResult {
            ok: !diags.iter().any(|d| d.severity == Severity::Error),
            validator: "nemo-console modelopt recipes schema".into(),
            authoritative: true,
            diagnostics: diags,
            note: Some(
                "This is the console's own schema, which is the only thing that reads the file (modelopt-local gets plain flags rendered from it). Model names are not checked against the model store."
                    .into(),
            ),
        })
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        self.config_path(file_id)?;
        let mut r = ApplyResult::new(true, "nothing to restart: recipes are read by the next job");
        r.post_check = PostCheck::Healthy;
        Ok(r)
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let (recipes, _, _) = self.inner.read_recipes()?;
        let mut names: Vec<String> = recipes.keys().cloned().collect();
        names.sort();
        let mut recipe = ParamSpec::new(
            "recipe",
            "Recipe",
            "A recipe from the recipes config file.",
            ParamType::Enum,
        );
        recipe.default = names.first().map_or(Value::Null, |n| json!(n));
        recipe.choices = if names.is_empty() { None } else { Some(names) };
        recipe.requires_restart = false;
        let mut job_id = ParamSpec::new(
            "job_id",
            "Job",
            "A job id from the jobs list; empty = the newest job.",
            ParamType::String,
        );
        job_id.requires_restart = false;
        let mut run = ActionSpec::new(
            "run",
            "Run a compression job",
            "Starts `modelopt-local run` for a recipe: the job compresses a pinned checkpoint (quantization, REAP expert pruning or Minitron) and writes a new one. disruptive: it takes a GPU for hours and writes a checkpoint. The job runs in the background; watch the jobs list, then 'Show a job' with this job id.",
            Danger::Disruptive,
        );
        run.params = vec![recipe];
        let mut report = ActionSpec::new(
            "report",
            "Show a job",
            "The report of a stored job (newest first): verdict, the recipe it ran and the size numbers.",
            Danger::Safe,
        );
        report.params = vec![job_id];
        Ok(vec![run, report])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        if action_id == "report" {
            let job_id = match params.get("job_id") {
                Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
                _ => String::new(),
            };
            let meta = self.job_meta(&job_id);
            let (ok, text) = report_text(meta.as_ref(), &job_id);
            return Ok(done(t0, ok, &text, Vec::new()));
        }
        if action_id != "run" {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        self.start_job(params, t0).await
    }
}

impl ModelOptAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        ModelOptAdapter {
            inner: Arc::new(Inner {
                ctx,
                version_cache: Mutex::new(None),
                follow_interval: Mutex::new(2.0),
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
    pub fn jobs_dir(&self) -> PathBuf {
        self.inner.jobs_dir()
    }

    /// The meta of one job (by id, or the newest one when the id is empty).
    pub fn job_meta(&self, job_id: &str) -> Option<Map<String, Value>> {
        let inner = &self.inner;
        if job_id.is_empty() {
            return inner.metas(1).into_iter().next();
        }
        if !is(&RUN_ID_RE, job_id) {
            return None;
        }
        inner.load_meta(&inner.jobs_dir().join(job_id))
    }
}

impl Inner {
    /// The job id the console hands to the wrapper: the stamp and the recipe name.
    fn new_job_id(&self, recipe: &str) -> String {
        format!("{}-{}", wsb::run_stamp(), recipe)
    }

    /// The job dir the console owns (0700, under its state dir).
    fn new_job_dir(&self, recipe: &str) -> std::io::Result<(String, PathBuf)> {
        let job_id = self.new_job_id(recipe);
        let dir = self.jobs_dir().join(&job_id);
        wsb::mkdir_leaf(&dir, 0o700)?;
        Ok((job_id, dir))
    }
}

impl ModelOptAdapter {
    async fn start_job(
        &self,
        params: &Map<String, Value>,
        t0: Instant,
    ) -> Result<ActionResult, AdapterError> {
        let inner = &self.inner;
        let (recipes, diags, _) = inner.read_recipes()?;
        let name = match params.get("recipe") {
            Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
            _ => String::new(),
        };
        let Some(recipe) = recipes.get(&name).cloned() else {
            let bad: Vec<&str> = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect();
            let extra = bad.first().map(|b| format!(" ({b})")).unwrap_or_default();
            return Ok(done(
                t0,
                false,
                &format!("unknown or invalid recipe {}{extra}", str_repr(&name)),
                Vec::new(),
            ));
        };
        let (launcher, _) = inner.launcher().await;
        if launcher.is_none() {
            return Ok(done(
                t0,
                false,
                "Model Optimizer is not installed.",
                problem(
                    "modelopt.not_installed",
                    "modelopt-local is not on PATH.",
                    &inner.install_fix(),
                ),
            ));
        }
        let (job_id, job_dir) = inner
            .new_job_dir(&name)
            .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let argv = render_argv(&job_id, &name, &job_dir);
        let r = inner
            .run(
                &argv,
                RunOpts::timeout(RUN_TIMEOUT)
                    .max_bytes(RUN_MAX_BYTES)
                    .cwd(job_dir.clone()),
            )
            .await;
        if r.returncode != 0 {
            return Ok(done(
                t0,
                false,
                &format!(
                    "modelopt-local refused the job ({}{}): {}",
                    job_id,
                    if r.timed_out { ", timed out" } else { "" },
                    pystr::head(&redact(&r.stderr), 500)
                ),
                problem(
                    "modelopt.start_failed",
                    "the wrapper did not start the job",
                    "run modelopt-local --version in the lab repository to see why",
                ),
            ));
        }
        Ok(done(
            t0,
            true,
            &format!(
                "Started compression job {job_id} ({name}: {} on {}, writing {}). It runs in the background: watch the jobs list, then 'Show a job' with this job id.",
                recipe.method, recipe.base_model, recipe.output
            ),
            Vec::new(),
        ))
    }
}

/// Build the adapter (Python: registry `build_adapter`; Model Optimizer has no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(ModelOptAdapter::new(ctx.clone())))
}
