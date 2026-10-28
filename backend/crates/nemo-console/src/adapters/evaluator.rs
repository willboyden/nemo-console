//! NeMo Evaluator adapter: NVIDIA NeMo Evaluator (`nel`, a CLI, not a service) as a managed system. OWNER: workstream C.
//! Port of `backend/src/nemo_console/adapters/evaluator.py` (its module docstring lists the facts it is built on):
//! the `evaluator-local` wrapper (`--version`, `cached --json`, `suites --file`, `run --suite NAME --output-dir DIR`
//! as ONE background run), the lab's `suites.yaml` with its own schema, and `summary.json` (numbers and names only).
//!
//! Submodules (workstream C, shared with the Data Designer and SkillEvaluator adapters): `pyobj` + `pyyaml` (PyYAML 6.0.3's
//! `safe_load`, ported), `support` (the adapters' common run/meta/format helpers).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, SystemAdapter,
};
use crate::config::Settings;
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, PostCheck,
    Problem, Severity, SystemEvent, SystemId, SystemStatus, ValidationResult,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::config_store::mtime_f64;
use crate::services::pyfmt::{char_prefix, str_repr};
use crate::services::pyobj::{Doc, Py, PyExc, PyInt, py_splitlines, py_strip};
use crate::services::pyyaml::{YamlError, safe_load};
use crate::services::support::{
    self as sp, Done, InterruptGuard, LiveSlot, dict, fixed, full, is_str, list, num, pystr, truthy,
};

/// `EvaluatorAdapter.label` in Python.
pub const LABEL: &str = "NeMo Evaluator";

pub const LAUNCHER: &str = "evaluator-local";
pub const RUN_TIMEOUT: f64 = 4800.0;
pub const QUICK_TIMEOUT: f64 = 60.0;
pub const RUN_MAX_BYTES: usize = 4_194_304;
pub const KEEP_RUNS: usize = 100;
pub const EVENT_KIND: &str = "evaluator.run";
pub const VERSION_TTL: f64 = 300.0;
pub const CACHED_TTL: f64 = 20.0;
pub const VERDICTS: [&str; 4] = ["succeeded", "failed", "error", "interrupted"];
pub const MAX_SCORE_PAIRS: usize = 50;
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;
pub const FILE_ID: &str = "suites.yaml";
pub const LAB_INSTALLER: &str = "clients/nemo-evaluator/install.sh";
pub const LAB_MINT_KEY: &str = "clients/nemo-evaluator/mint-key.sh";
pub const PINNED_VERSION: &str = "0.3.0";
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
const META_REDACT: &[&str] = &["error"];

static RUN_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"\d{8}T\d{6}-\d{3}-[a-z0-9][a-z0-9-]{0,39}"));
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

/// A valid suite: its model alias and its benchmarks in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suite {
    pub model: String,
    pub benches: Vec<String>,
}

/// (valid suites by name, error messages) — Python `suites()`.
pub type Suites = (IndexMap<String, Suite>, Vec<String>);

fn suites_of(doc: &Doc) -> Result<IndexMap<String, Suite>, PyExc> {
    let mut out = IndexMap::new();
    let top = doc.dict(&doc.root).unwrap_or(&[]);
    let suites_v = Doc::get(top, "suites").cloned().unwrap_or(Py::None);
    for (k, v) in doc.dict(&suites_v).unwrap_or(&[]) {
        let s = doc.dict(v).unwrap_or(&[]);
        let model = Doc::get(s, "model").map_or(Ok("None".to_owned()), |m| doc.str_of(m))?;
        out.insert(
            doc.str_of(k)?,
            Suite {
                model,
                benches: suite_benches(doc, s)?,
            },
        );
    }
    Ok(out)
}

/// `_suite_benches(suite)`: `str(_dict(b).get("bench"))` for every benchmark entry.
fn suite_benches(doc: &Doc, suite: &[(Py, Py)]) -> Result<Vec<String>, PyExc> {
    let benches_v = Doc::get(suite, "benchmarks").cloned().unwrap_or(Py::None);
    let mut out = Vec::new();
    for b in doc.list(&benches_v).unwrap_or(&[]) {
        let bd = doc.dict(b).unwrap_or(&[]);
        out.push(match Doc::get(bd, "bench") {
            Some(v) => doc.str_of(v)?,
            None => "None".to_owned(),
        });
    }
    Ok(out)
}

/// `list(dict.fromkeys(xs))`.
fn unique(xs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in xs {
        if !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

fn yaml_to_adapter(e: &YamlError) -> AdapterError {
    AdapterError::failed(e.class(), e.text())
}

/// The NeMo Evaluator adapter (Python `EvaluatorAdapter`).
pub struct EvaluatorAdapter {
    inner: Arc<Inner>,
}

type Cached = Option<IndexMap<String, bool>>;

struct Inner {
    ctx: AdapterContext,
    version_cache: Mutex<(Option<Instant>, Option<String>)>,
    cached_cache: Mutex<(Option<Instant>, Cached)>,
    follow_interval_bits: AtomicU64,
    live: LiveSlot,
}

impl EvaluatorAdapter {
    /// `EvaluatorAdapter(ctx)`.
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        EvaluatorAdapter {
            inner: Arc::new(Inner {
                ctx,
                version_cache: Mutex::new((None, None)),
                cached_cache: Mutex::new((None, None)),
                follow_interval_bits: AtomicU64::new(2.0f64.to_bits()),
                live: LiveSlot::default(),
            }),
        }
    }

    /// `a.follow_interval = s` (tests).
    pub fn set_follow_interval(&self, s: f64) {
        self.inner
            .follow_interval_bits
            .store(s.to_bits(), Ordering::Relaxed);
    }

    /// `a._runs_dir`.
    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.inner.runs_dir()
    }

    /// `a._suites_path`.
    #[must_use]
    pub fn suites_path(&self) -> Option<PathBuf> {
        self.inner.suites_path().cloned()
    }

    /// `a._live_run_id()`.
    #[must_use]
    pub fn live_run_id(&self) -> Option<String> {
        self.inner.live.run_id()
    }

    /// The live run slot (tests).
    #[must_use]
    pub fn live(&self) -> &LiveSlot {
        &self.inner.live
    }

    /// `a.suites()`.
    pub fn suites(&self) -> Result<Suites, AdapterError> {
        self.inner.suites()
    }

    /// `a._cached_cache = (ts, data)` (tests set the cached-datasets view).
    pub fn set_cached_view(&self, data: Option<IndexMap<String, bool>>) {
        let mut c = lock(&self.inner.cached_cache);
        c.1 = data;
    }

    /// The adapter context.
    #[must_use]
    pub fn ctx(&self) -> &AdapterContext {
        &self.inner.ctx
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

impl Inner {
    fn suites_path(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("evaluator_suites")
    }

    fn key_file(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("evaluator_key_file")
    }

    fn runs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("evaluator").join("runs")
    }

    fn litellm(&self) -> String {
        sp::litellm(&self.ctx)
    }

    fn install_fix(&self) -> String {
        match sp::lab_file(&self.ctx, "evaluator_dir", LAB_INSTALLER) {
            Some(lab) => format!(
                "{}   # pinned nemo-evaluator {PINNED_VERSION} + the evaluator-local wrapper",
                lab.display()
            ),
            None => format!(
                "run clients/nemo-evaluator/install.sh in the lab repository (pinned nemo-evaluator {PINNED_VERSION}; it links the evaluator-local wrapper onto PATH)"
            ),
        }
    }

    fn key_fix(&self) -> String {
        match sp::lab_file(&self.ctx, "evaluator_dir", LAB_MINT_KEY) {
            Some(lab) => format!(
                "{}   # mints the router key file evaluator-local reads",
                lab.display()
            ),
            None => "run clients/nemo-evaluator/mint-key.sh in the lab repository (it writes ~/.config/nemo-evaluator/litellm.key, 0600)".to_owned(),
        }
    }

    async fn run(
        &self,
        argv: &[&str],
        timeout: f64,
        max_bytes: usize,
        cwd: Option<&Path>,
    ) -> CommandResult {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
        sp::run_cmd(&self.ctx, &argv, timeout, max_bytes, cwd).await
    }

    async fn version(&self) -> Option<String> {
        {
            let c = lock(&self.version_cache);
            if let (Some(ts), Some(v)) = (c.0, &c.1)
                && ts.elapsed().as_secs_f64() < VERSION_TTL
            {
                return Some(v.clone());
            }
        }
        let r = self.run(&[LAUNCHER, "--version"], 20.0, 4096, None).await;
        let version = if r.returncode == 0 {
            py_splitlines(py_strip(&r.stdout))
                .first()
                .map(|l| char_prefix(l, 40).to_owned())
        } else {
            None
        };
        *lock(&self.version_cache) = (Some(Instant::now()), version.clone());
        version
    }

    async fn cached(&self, fresh: bool) -> Cached {
        {
            let c = lock(&self.cached_cache);
            if !fresh
                && let (Some(ts), Some(d)) = (c.0, &c.1)
                && ts.elapsed().as_secs_f64() < CACHED_TTL
            {
                return Some(d.clone());
            }
        }
        let r = self
            .run(&[LAUNCHER, "cached", "--json"], QUICK_TIMEOUT, 32_768, None)
            .await;
        let mut data = None;
        if r.returncode == 0
            && let Some(doc) = sp::json_loads_str(&r.stdout)
        {
            let mut m = IndexMap::new();
            for x in list(dict(Some(&doc)).get("benchmarks")).iter().take(50) {
                let x = dict(Some(x));
                if let Some(Value::String(b)) = x.get("bench") {
                    m.insert(b.clone(), truthy(x.get("cached")));
                }
            }
            data = Some(m);
        }
        *lock(&self.cached_cache) = (Some(Instant::now()), data.clone());
        data
    }

    fn reset_cached(&self) {
        *lock(&self.cached_cache) = (None, None);
    }

    // ── suites.yaml ─────────────────────────────────────────────────────────────────────────

    fn read_suites_text(&self) -> Result<Option<String>, AdapterError> {
        let Some(p) = self.suites_path() else {
            return Ok(None);
        };
        if p.is_symlink() || !p.is_file() {
            return Ok(None);
        }
        match std::fs::metadata(p) {
            Ok(m) if m.len() > MAX_CONFIG_BYTES => return Ok(None),
            Err(_) => return Ok(None),
            Ok(_) => {}
        }
        match sp::read_text(p) {
            Ok(t) => Ok(Some(t)),
            Err(sp::ReadErr::Os(_)) => Ok(None),
            Err(e) => Err(e.to_adapter(p)),
        }
    }

    fn suites(&self) -> Result<Suites, AdapterError> {
        let Some(text) = self.read_suites_text()? else {
            let p = self
                .suites_path()
                .map_or_else(|| "suites.yaml".to_owned(), |p| p.display().to_string());
            return Ok((
                IndexMap::new(),
                vec![format!("{p} is missing, a symlink or too large")],
            ));
        };
        let doc = match safe_load(&text) {
            Ok(d) => d,
            Err(e) if e.is_yaml() => {
                let t = e.text();
                let first = if t.is_empty() {
                    e.class().to_owned()
                } else {
                    char_prefix(py_splitlines(&t).first().copied().unwrap_or(""), 200).to_owned()
                };
                return Ok((IndexMap::new(), vec![format!("YAML error: {first}")]));
            }
            Err(e) => return Err(yaml_to_adapter(&e)),
        };
        let py = |e: PyExc| AdapterError::failed(e.class, e.message);
        let errs: Vec<String> = validate_suites(&doc)
            .map_err(py)?
            .into_iter()
            .map(|(m, _)| m)
            .collect();
        if !errs.is_empty() {
            return Ok((IndexMap::new(), errs.into_iter().take(10).collect()));
        }
        Ok((suites_of(&doc).map_err(py)?, Vec::new()))
    }

    // ── stored runs ─────────────────────────────────────────────────────────────────────────

    fn load_meta(&self, run_dir: &Path) -> Option<Map<String, Value>> {
        let mut meta = sp::load_json_dict(&run_dir.join("run.json"), None)?;
        let live = self.live.run_id();
        if is_str(meta.get("verdict"), "running")
            && meta.get("run_id") != live.map(Value::String).as_ref()
        {
            meta.insert("verdict".into(), Value::from("interrupted"));
            meta.insert("interrupted_by".into(), Value::from("restart"));
        }
        Some(meta)
    }

    fn scores_text(meta: &Map<String, Value>) -> String {
        let mut bits = Vec::new();
        for r in list(meta.get("results")).iter().take(8) {
            let rr = dict(Some(r));
            let bench = match rr.get("bench") {
                None => "?".to_owned(),
                v => pystr(v),
            };
            bits.push(match num(rr.get("score")) {
                Some(sc) => format!("{bench} {}", fixed(sc, 3, false)),
                None => format!(
                    "{bench} {}",
                    match rr.get("status") {
                        None => "?".to_owned(),
                        v => pystr(v),
                    }
                ),
            });
        }
        bits.join(", ")
    }

    fn event(meta: &Map<String, Value>) -> SystemEvent {
        let get_or = |k: &str| match meta.get(k) {
            None => "?".to_owned(),
            v => pystr(v),
        };
        let mut parts = vec![get_or("suite"), get_or("model")];
        let scores = Inner::scores_text(meta);
        if !scores.is_empty() {
            parts.push(scores);
        }
        parts.push(get_or("verdict"));
        if is_str(meta.get("verdict"), "running") {
            let ts = num(meta.get("ts"))
                .filter(|t| *t != 0.0)
                .unwrap_or_else(sp::now);
            let mins = ((sp::now() - ts) / 60.0).max(0.0);
            parts.push(format!("running for {} min", fixed(mins, 0, false)));
        }
        let mut data = Map::new();
        for k in ["run_id", "suite", "model", "verdict", "duration_ms"] {
            data.insert(k.into(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        let benches: Vec<Value> = list(meta.get("results"))
            .iter()
            .take(8)
            .map(|r| {
                let rr = dict(Some(r));
                let mut o = Map::new();
                for k in ["bench", "status", "metric", "score", "samples"] {
                    o.insert(k.into(), rr.get(k).cloned().unwrap_or(Value::Null));
                }
                Value::Object(o)
            })
            .collect();
        data.insert("benchmarks".into(), Value::Array(benches));
        SystemEvent {
            ts: num(meta.get("ts")).unwrap_or(0.0),
            system: SystemId::Evaluator,
            kind: EVENT_KIND.to_owned(),
            summary: char_prefix(&redact(&parts.join(" · ")), 300).to_owned(),
            data,
        }
    }

    fn run_dirs(&self) -> Vec<PathBuf> {
        sp::run_dirs(&self.runs_dir(), &RUN_ID_RE, true)
    }

    fn metas(&self, limit: usize) -> Vec<Map<String, Value>> {
        self.run_dirs()
            .into_iter()
            .take(limit)
            .filter_map(|d| self.load_meta(&d))
            .collect()
    }

    fn list_runs(&self, limit: usize) -> Vec<SystemEvent> {
        self.metas(limit).iter().map(Inner::event).collect()
    }

    fn prune(&self) {
        let live = self.live.run_id();
        sp::prune(&self.run_dirs(), KEEP_RUNS, live.as_deref());
    }

    fn read_summary(run_dir: &Path) -> Option<Map<String, Value>> {
        let p = run_dir.join("out").join("summary.json");
        if p.is_symlink() {
            return None;
        }
        sp::load_json_dict(&p, Some(1_000_000))
    }

    fn results(summary: &Map<String, Value>) -> Vec<Value> {
        let keep = [
            "bench",
            "status",
            "metric",
            "score",
            "ci_low",
            "ci_high",
            "samples",
            "max_problems",
            "tokens",
            "elapsed_s",
            "failure_rate",
        ];
        let mut out = Vec::new();
        for b in list(summary.get("benchmarks")).iter().take(MAX_BENCHES) {
            let bb = dict(Some(b));
            let mut row = Map::new();
            for k in keep {
                if let Some(v) = bb.get(k).filter(|v| !v.is_null()) {
                    row.insert(k.into(), v.clone());
                }
            }
            if let Some(Value::String(e)) = bb.get("error") {
                row.insert(
                    "error".into(),
                    Value::from(char_prefix(&redact(e), 300).to_owned()),
                );
            }
            if matches!(row.get("bench"), Some(Value::String(_))) {
                out.push(Value::Object(row));
            }
        }
        out
    }

    fn finish_run(
        &self,
        run_dir: &Path,
        meta: &mut Map<String, Value>,
        r: &CommandResult,
        ms: i64,
    ) -> std::io::Result<()> {
        meta.insert("duration_ms".into(), Value::from(ms));
        let summary = Inner::read_summary(run_dir);
        if let Some(s) = &summary {
            meta.insert("results".into(), Value::Array(Inner::results(s)));
        }
        let state = summary.as_ref().and_then(|s| s.get("state"));
        if r.timed_out {
            meta.insert("verdict".into(), Value::from("failed"));
            meta.insert("error".into(), Value::from("timed out"));
        } else if r.returncode == 130 || is_str(state, "interrupted") {
            meta.insert("verdict".into(), Value::from("interrupted"));
            meta.insert("interrupted_by".into(), Value::from("signal"));
        } else if r.returncode == 0 && is_str(state, "succeeded") {
            meta.insert("verdict".into(), Value::from("succeeded"));
        } else if r.returncode == 0 {
            meta.insert("verdict".into(), Value::from("error"));
            meta.insert(
                "error".into(),
                Value::from("the wrapper exited 0 but wrote no succeeded summary.json"),
            );
        } else {
            let errors: Vec<String> = list(meta.get("results"))
                .iter()
                .filter_map(|x| {
                    let e = dict(Some(x)).get("error").cloned();
                    truthy(e.as_ref()).then(|| pystr(e.as_ref()))
                })
                .collect();
            let text = match errors.last() {
                Some(e) => e.clone(),
                None => {
                    let src = if r.stderr.is_empty() {
                        &r.stdout
                    } else {
                        &r.stderr
                    };
                    sp::strip_tail(src, 1500)
                }
            };
            let text = if text.is_empty() {
                format!("exit {}", r.returncode)
            } else {
                text
            };
            meta.insert("verdict".into(), Value::from("failed"));
            meta.insert("error".into(), Value::from(text));
        }
        self.reset_cached();
        sp::write_meta(run_dir, meta, META_REDACT)?;
        self.prune();
        Ok(())
    }

    fn report_text(&self, run_id: &str) -> (bool, String) {
        let meta = if run_id.is_empty() {
            self.metas(1).into_iter().next()
        } else {
            if !RUN_ID_RE.is_match(run_id) {
                return (
                    false,
                    "run_id must be a run id from the runs list".to_owned(),
                );
            }
            let d = self.runs_dir().join(run_id);
            if d.is_dir() && !d.is_symlink() {
                self.load_meta(&d)
            } else {
                None
            }
        };
        let Some(meta) = meta else {
            return (
                false,
                if run_id.is_empty() {
                    "no runs yet: start one with the 'Run a suite' action"
                } else {
                    "no such run"
                }
                .to_owned(),
            );
        };
        let get_or = |k: &str| match meta.get(k) {
            None => "?".to_owned(),
            v => pystr(v),
        };
        let mut verdict_line = format!("verdict: {}", get_or("verdict"));
        if truthy(meta.get("duration_ms")) {
            let ms = num(meta.get("duration_ms"))
                .filter(|v| *v != 0.0)
                .unwrap_or(0.0);
            verdict_line.push_str(&format!(" · {} s", fixed(ms / 1000.0, 1, false)));
        }
        let mut lines = vec![
            format!("Run {}", get_or("run_id")),
            format!("suite: {} · model: {}", get_or("suite"), get_or("model")),
            verdict_line,
        ];
        if truthy(meta.get("error")) {
            lines.push(format!(
                "error: {}",
                char_prefix(&pystr(meta.get("error")), 600)
            ));
        }
        let rows: Vec<Value> = list(meta.get("results"))
            .into_iter()
            .take(MAX_BENCHES)
            .collect();
        if !rows.is_empty() {
            lines.push(String::new());
            lines.push(
                "| Benchmark | Status | Metric | Score | CI (95%) | Samples | Tokens |".to_owned(),
            );
            lines.push("|---|---|---|---|---|---|---|".to_owned());
            for r in rows {
                let rr = dict(Some(&r));
                let (sc, lo, hi) = (
                    num(rr.get("score")),
                    num(rr.get("ci_low")),
                    num(rr.get("ci_high")),
                );
                let ci = match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        format!("{}..{}", fixed(lo, 3, false), fixed(hi, 3, false))
                    }
                    _ => "-".to_owned(),
                };
                let g = |k: &str, d: &str| match rr.get(k) {
                    None => d.to_owned(),
                    v => pystr(v),
                };
                let metric = if truthy(rr.get("metric")) {
                    pystr(rr.get("metric"))
                } else {
                    "-".to_owned()
                };
                lines.push(format!(
                    "| {} | {} | {} | {} | {ci} | {} | {} |",
                    char_prefix(&g("bench", "?"), 40),
                    char_prefix(&g("status", "?"), 20),
                    char_prefix(&metric, 20),
                    sc.map_or_else(|| "-".to_owned(), |s| fixed(s, 4, false)),
                    g("samples", "-"),
                    g("tokens", "-"),
                ));
            }
        }
        (!is_str(meta.get("verdict"), "error"), lines.join("\n"))
    }

    fn line_of(text: &str, needle: Option<&str>) -> Option<i64> {
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

    async fn wrapper_validate(&self, content: &str) -> Result<Vec<Diagnostic>, AdapterError> {
        let tmp_dir = self.ctx.state_dir.join("evaluator").join("tmp");
        sp::mkdir_private(&tmp_dir).map_err(|e| sp::io_adapter_error(&e, Some(&tmp_dir)))?;
        let mut rnd = [0u8; 6];
        let _ = getrandom::fill(&mut rnd);
        let tmp = tmp_dir.join(format!("suites.{}.yaml", hex::encode(rnd)));
        let r = match write_private(&tmp, content) {
            Ok(()) => {
                let t = tmp.to_string_lossy().into_owned();
                let r = self
                    .run(
                        &[LAUNCHER, "suites", "--file", &t],
                        QUICK_TIMEOUT,
                        16_384,
                        None,
                    )
                    .await;
                let _ = std::fs::remove_file(&tmp);
                r
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(sp::io_adapter_error(&e, Some(&tmp)));
            }
        };
        if r.returncode == 0 {
            return Ok(Vec::new());
        }
        let tail = cli_tail(&r);
        let skip = tail.len().saturating_sub(6);
        Ok(tail
            .into_iter()
            .skip(skip)
            .map(|m| Diagnostic {
                severity: Severity::Error,
                message: format!("evaluator-local: {m}"),
                line: None,
            })
            .collect())
    }
}

/// `[redact(line.strip())[:300] for line in (r.stderr + "\n" + r.stdout).splitlines() if line.strip()]`.
pub(crate) fn cli_tail(r: &CommandResult) -> Vec<String> {
    let both = format!("{}\n{}", r.stderr, r.stdout);
    py_splitlines(&both)
        .into_iter()
        .map(py_strip)
        .filter(|l| !l.is_empty())
        .map(|l| char_prefix(&redact(l), 300).to_owned())
        .collect()
}

/// `tmp.write_text(content); tmp.chmod(0o600)`.
pub(crate) fn write_private(p: &Path, content: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, content)?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
}

fn param(key: &str, label: &str, type_: ParamType, help: &str) -> ParamSpec {
    let mut p = ParamSpec::new(key, label, help, type_);
    p.requires_restart = false;
    p
}

fn err_diag(message: &str) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        message: message.to_owned(),
        line: None,
    }
}

#[async_trait]
impl SystemAdapter for EvaluatorAdapter {
    fn id(&self) -> SystemId {
        SystemId::Evaluator
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    #[allow(clippy::too_many_lines)]
    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let me = &self.inner;
        let (version, (router_ok, router_detail), cached) =
            tokio::join!(me.version(), sp::router_up(&me.ctx), me.cached(false));
        let (suites, suite_errs) = me.suites()?;
        let last = me.list_runs(1);
        let litellm = me.litellm();
        let mut problems = Vec::new();
        let mut components = vec![
            Component {
                kind: "process".into(),
                name: LAUNCHER.into(),
                state: if version.is_some() {
                    "running"
                } else {
                    "absent"
                }
                .into(),
                image: None,
                detail: Some(match &version {
                    Some(v) => format!(
                        "{LAUNCHER} {v} (nel; runs are OFFLINE: Hugging Face access is switched off and the router key is read by the wrapper from its own key file)"
                    ),
                    None => format!(
                        "{LAUNCHER} is not on PATH or not installed (clients/nemo-evaluator/install.sh)"
                    ),
                }),
            },
            Component {
                kind: "http".into(),
                name: format!("router {litellm}"),
                state: if router_ok { "running" } else { "stopped" }.into(),
                image: None,
                detail: Some(format!(
                    "{router_detail} on GET /health/liveliness (every evaluation request goes through it)"
                )),
            },
        ];
        if let Some((rid, suite, started)) = me.live.info() {
            let mins = ((sp::now() - started) / 60.0).max(0.0);
            components.push(Component {
                kind: "process".into(),
                name: format!("run {suite}"),
                state: "running".into(),
                image: None,
                detail: Some(format!(
                    "evaluation {rid}, running for {} min (one at a time; it shows in the runs list)",
                    fixed(mins, 0, false)
                )),
            });
        }
        if version.is_none() {
            problems.push(Problem::new(
                Severity::Error,
                "ev.not_installed",
                "NeMo Evaluator is not installed (or not on the console's PATH).",
                Some(me.install_fix()),
            ));
        }
        if !router_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "ev.router_down",
                format!("The router ({litellm}) is not answering; evaluation runs will fail."),
                Some(format!(
                    "curl -s -o /dev/null -w '%{{http_code}}\\n' {litellm}/health/liveliness"
                )),
            ));
        }
        if version.is_some()
            && let Some(k) = me.key_file()
            && !k.exists()
        {
            problems.push(Problem::new(
                Severity::Warn,
                "ev.no_key",
                format!(
                    "The router key file {} is absent; evaluator-local run will refuse to start.",
                    k.display()
                ),
                Some(me.key_fix()),
            ));
        }
        if !suite_errs.is_empty() {
            let first3: Vec<&str> = suite_errs.iter().take(3).map(String::as_str).collect();
            problems.push(Problem::new(
                Severity::Error,
                "ev.suites_invalid",
                format!("suites.yaml is not valid: {}", first3.join("; ")),
                Some("edit it in the Configure tab (the validator names each problem), or restore clients/nemo-evaluator/suites.yaml from git".to_owned()),
            ));
        }
        if version.is_some()
            && let Some(c) = &cached
            && !suites.is_empty()
        {
            let mut missing: Vec<(String, Vec<String>)> = suites
                .iter()
                .map(|(n, s)| {
                    let bs: Vec<String> = unique(&s.benches)
                        .into_iter()
                        .filter(|b| !c.get(b).copied().unwrap_or(false))
                        .collect();
                    (n.clone(), bs)
                })
                .filter(|(_, bs)| !bs.is_empty())
                .collect();
            if !missing.is_empty() {
                let mut need: Vec<String> = missing.iter().flat_map(|(_, bs)| bs.clone()).collect();
                need.sort();
                need.dedup();
                missing.sort_by(|a, b| a.0.cmp(&b.0));
                let text: Vec<String> = missing
                    .iter()
                    .map(|(n, bs)| format!("{n}: {}", bs.join(", ")))
                    .collect();
                problems.push(Problem::new(
                    Severity::Warn,
                    "ev.dataset_missing",
                    format!(
                        "Datasets not prefetched (runs are offline and would fail): {}",
                        text.join("; ")
                    ),
                    Some(format!(
                        "evaluator-local prefetch {}   # the only step with internet; run it yourself",
                        need.join(" ")
                    )),
                ));
            }
        }
        if let Some(l) = last.first()
            && (is_str(l.data.get("verdict"), "failed") || is_str(l.data.get("verdict"), "error"))
        {
            problems.push(Problem::new(
                Severity::Info,
                "ev.last_run_failed",
                format!("The newest run failed: {}", l.summary),
                Some("run the 'Show a stored run' action for its error".to_owned()),
            ));
        }
        let health = if version.is_none() {
            Health::Down
        } else if !router_ok || !suite_errs.is_empty() {
            Health::Degraded
        } else {
            Health::Up
        };
        let cached_n = cached
            .as_ref()
            .map_or(0, |c| c.values().filter(|v| **v).count());
        let mut summary = format!(
            "{} suites · {cached_n}/{} datasets cached",
            suites.len(),
            cached.as_ref().map_or(0, IndexMap::len)
        );
        if let Some(l) = last.first() {
            summary.push_str(&format!(" · last run: {}", l.summary));
        }
        Ok(SystemStatus {
            system: SystemId::Evaluator,
            health,
            version,
            summary: char_prefix(&summary, 300).to_owned(),
            endpoints: vec![Endpoint {
                name: "router (evaluation)".into(),
                url: litellm,
                role: "api".into(),
                auth: "bearer".into(),
            }],
            components,
            problems,
            checked_at: sp::now(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let me = &self.inner;
        let now = sp::now();
        let (suites, _) = me.suites()?;
        let metas = me.metas(KEEP_RUNS);
        let pt = |k: &str, v: f64, unit: &str| {
            let mut p = MetricPoint::new(k, v);
            unit.clone_into(&mut p.unit);
            p
        };
        #[allow(clippy::cast_precision_loss)]
        let mut points = vec![
            pt("ev.suites", suites.len() as f64, "1"),
            pt("ev.runs", metas.len() as f64, "1"),
        ];
        if !metas.is_empty() {
            points.push(pt(
                "ev.live_running",
                if me.live.run_id().is_some() { 1.0 } else { 0.0 },
                "1",
            ));
            for verdict in VERDICTS {
                let n = metas
                    .iter()
                    .filter(|m| is_str(m.get("verdict"), verdict))
                    .count();
                #[allow(clippy::cast_precision_loss)]
                let mut p = pt("ev.runs_by_verdict", n as f64, "1");
                p.labels.insert("verdict".into(), verdict.into());
                points.push(p);
            }
        }
        let mut seen: Vec<(String, String)> = Vec::new();
        for m in &metas {
            let model = if truthy(m.get("model")) {
                pystr(m.get("model"))
            } else {
                String::new()
            };
            for r in list(m.get("results")) {
                let rr = dict(Some(&r));
                let (Some(Value::String(bench)), Some(score)) =
                    (rr.get("bench"), num(rr.get("score")))
                else {
                    continue;
                };
                if model.is_empty() || seen.contains(&(bench.clone(), model.clone())) {
                    continue;
                }
                if seen.len() >= MAX_SCORE_PAIRS {
                    break;
                }
                seen.push((bench.clone(), model.clone()));
                let mut p = pt("ev.score", score, "ratio");
                p.labels.insert("bench".into(), bench.clone());
                p.labels.insert("model".into(), model.clone());
                points.push(p);
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Evaluator,
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
        let me = Arc::clone(&self.inner);
        Box::pin(async_stream::stream! {
            let mut seen = std::collections::HashSet::new();
            let mut first = true;
            loop {
                let mut events = me.list_runs(50);
                events.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap_or(std::cmp::Ordering::Equal));
                for ev in events {
                    let key = (pystr(ev.data.get("run_id")), pystr(ev.data.get("verdict")));
                    if !seen.insert(key) {
                        continue;
                    }
                    if first && since_ts.is_none_or(|s| ev.ts <= s) {
                        continue;
                    }
                    yield Ok(ev);
                }
                first = false;
                let secs = f64::from_bits(me.follow_interval_bits.load(Ordering::Relaxed));
                tokio::time::sleep(Duration::from_secs_f64(secs.max(0.001))).await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        if self.inner.suites_path().is_none() {
            return Ok(Vec::new());
        }
        Ok(vec![ConfigFileSpec {
            id: FILE_ID.into(),
            label: "Evaluation suites".into(),
            description: "Named NeMo Evaluator runs: a router model alias plus 1-8 benchmarks, each with a REQUIRED max_problems bound. Benchmarks that need a Docker sandbox or an LLM judge are rejected. Datasets must be prefetched by hand (evaluator-local prefetch): runs are offline.".into(),
            display_path: "clients/nemo-evaluator/suites.yaml".into(),
            format: "yaml".into(),
            editable: true,
            why_readonly: None,
            validators: vec![
                "suites.yaml schema (console)".into(),
                "evaluator-local suites --file".into(),
            ],
            authoritative: true,
            apply_effect: "takes effect on the next run; nothing to restart".into(),
        }])
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        let unknown = || AdapterError::UnknownId(file_id.to_owned());
        let p = self.inner.suites_path().ok_or_else(unknown)?;
        if file_id != FILE_ID || p.is_symlink() || !p.is_file() {
            return Err(unknown());
        }
        Ok(sp::realpath(p))
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.config_path(file_id)?;
        let not_found = || AdapterError::FileNotFound(file_id.to_owned());
        let md = std::fs::metadata(&p).map_err(|_| not_found())?;
        if md.len() > MAX_CONFIG_BYTES {
            return Err(not_found());
        }
        let content = match sp::read_text(&p) {
            Ok(t) => t,
            Err(sp::ReadErr::Os(_)) => return Err(not_found()),
            Err(e) => return Err(e.to_adapter(&p)),
        };
        let mtime = std::fs::metadata(&p)
            .ok()
            .and_then(|m| mtime_f64(&m))
            .ok_or_else(not_found)?;
        let spec = self
            .config_files()?
            .into_iter()
            .next()
            .ok_or_else(not_found)?;
        Ok(ConfigFile {
            spec,
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
        let me = &self.inner;
        let name = "suites.yaml schema (console)";
        let result =
            |ok: bool, diagnostics: Vec<Diagnostic>, note: Option<String>| ValidationResult {
                ok,
                validator: name.into(),
                authoritative: true,
                diagnostics,
                note,
            };
        if content.len() as u64 > MAX_CONFIG_BYTES {
            return Ok(result(
                false,
                vec![err_diag("file is larger than 256 KiB")],
                None,
            ));
        }
        let doc = match safe_load(content) {
            Ok(d) => d,
            Err(e) if e.is_yaml() => {
                let problem = e
                    .problem()
                    .filter(|p| !p.is_empty())
                    .unwrap_or("invalid YAML");
                let message =
                    char_prefix(&redact(&format!("YAML error: {problem}")), 300).to_owned();
                let line = e
                    .problem_mark()
                    .and_then(|m| i64::try_from(m.line + 1).ok());
                return Ok(result(
                    false,
                    vec![Diagnostic {
                        severity: Severity::Error,
                        message,
                        line,
                    }],
                    None,
                ));
            }
            Err(e) => return Err(yaml_to_adapter(&e)),
        };
        let py = |e: PyExc| AdapterError::failed(e.class, e.message);
        let mut diags: Vec<Diagnostic> = validate_suites(&doc)
            .map_err(py)?
            .into_iter()
            .map(|(m, key)| Diagnostic {
                severity: Severity::Error,
                message: char_prefix(&m, 300).to_owned(),
                line: Inner::line_of(content, key.as_deref()),
            })
            .collect();
        let mut notes = vec![
            "Checked against the suite schema; model aliases are not checked against the router."
                .to_owned(),
        ];
        let cached_view = lock(&me.cached_cache).1.clone();
        if diags.is_empty()
            && let Some(cached) = cached_view
        {
            let top = doc.dict(&doc.root).unwrap_or(&[]);
            let suites_v = Doc::get(top, "suites").cloned().unwrap_or(Py::None);
            for (sname, suite) in doc.dict(&suites_v).unwrap_or(&[]) {
                let benches = suite_benches(&doc, doc.dict(suite).unwrap_or(&[])).map_err(py)?;
                let miss: Vec<String> = unique(&benches)
                    .into_iter()
                    .filter(|b| !cached.get(b).copied().unwrap_or(false))
                    .collect();
                if !miss.is_empty() {
                    let sn = doc.str_of(sname).map_err(py)?;
                    diags.push(Diagnostic {
                        severity: Severity::Warn,
                        line: Inner::line_of(content, Some(&sn)),
                        message: format!(
                            "suite {}: dataset(s) not prefetched: {} (evaluator-local prefetch {})",
                            doc.repr(sname).map_err(py)?,
                            miss.join(", "),
                            miss.join(" ")
                        ),
                    });
                }
            }
        }
        let has_error = |d: &[Diagnostic]| d.iter().any(|x| x.severity == Severity::Error);
        if !has_error(&diags) && me.version().await.is_some() {
            let wrapper = me.wrapper_validate(content).await?;
            notes.push(
                if wrapper.is_empty() {
                    "The wrapper's own validator (evaluator-local suites --file) agrees."
                } else {
                    "The wrapper's own validator disagreed."
                }
                .to_owned(),
            );
            diags.extend(wrapper);
        }
        let ok = !has_error(&diags);
        Ok(result(ok, diags, Some(notes.join(" "))))
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        self.config_path(file_id)?;
        self.inner.reset_cached();
        let mut r = ApplyResult::new(
            true,
            "nothing to restart: suites.yaml is read by the next run",
        );
        r.post_check = PostCheck::Healthy;
        Ok(r)
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let names: Vec<String> = self.inner.suites()?.0.keys().cloned().collect();
        let mut suite = param(
            "suite",
            "Suite",
            ParamType::Enum,
            "A suite from suites.yaml.",
        );
        suite.default = names
            .first()
            .map_or(Value::Null, |n| Value::from(n.as_str()));
        suite.choices = (!names.is_empty()).then_some(names);
        let mut run = ActionSpec::new(
            "run",
            "Run a suite",
            "Runs `evaluator-local run --suite NAME` in the background (ONE at a time): each benchmark's sampled problems go through the router to the suite's model (spends router tokens; minutes for large suites). The run is OFFLINE: datasets must already be prefetched. Scores appear under metrics (ev.score) and in the runs list.",
            Danger::Disruptive,
        );
        run.params = vec![suite];
        let mut report = ActionSpec::new(
            "report",
            "Show a stored run",
            "The report of a stored run (newest first): verdict and a table of per-benchmark scores. Numbers only: no questions or model answers are shown.",
            Danger::Safe,
        );
        report.params = vec![param(
            "run_id",
            "Run",
            ParamType::String,
            "A run id from the runs list; empty = the newest run.",
        )];
        Ok(vec![run, report])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let done = Done::start();
        match action_id {
            "run" => self.start_run(params, done).await,
            "report" => {
                let rid = if truthy(params.get("run_id")) {
                    pystr(params.get("run_id"))
                } else {
                    String::new()
                };
                let (ok, text) = self.inner.report_text(&rid);
                Ok(done.call(ok, &text, vec![]))
            }
            _ => Err(AdapterError::UnknownId(action_id.to_owned())),
        }
    }
}

impl EvaluatorAdapter {
    fn busy_result(&self, done: Done) -> Option<ActionResult> {
        let busy = self.inner.live.run_id()?;
        Some(done.call(
            false,
            &format!(
                "An evaluation is already running ({busy}); one runs at a time. Wait for it to finish: it shows in the runs list."
            ),
            vec![Problem::new(
                Severity::Warn,
                "ev.run.busy",
                format!("evaluation {busy} is still running"),
                Some(format!("'Show a stored run' with run id {busy} shows its progress")),
            )],
        ))
    }

    async fn start_run(
        &self,
        params: &Map<String, Value>,
        done: Done,
    ) -> Result<ActionResult, AdapterError> {
        let me = &self.inner;
        if let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        let (suites, errs) = me.suites()?;
        if !errs.is_empty() {
            let first3: Vec<&str> = errs.iter().take(3).map(String::as_str).collect();
            return Ok(done.call(
                false,
                &format!("suites.yaml is not valid: {}", first3.join("; ")),
                vec![Problem::new(
                    Severity::Error,
                    "ev.suites_invalid",
                    "suites.yaml is not valid",
                    Some("fix it in the Configure tab".to_owned()),
                )],
            ));
        }
        let suite_name = if truthy(params.get("suite")) {
            pystr(params.get("suite"))
        } else {
            String::new()
        };
        let Some(suite) = suites.get(&suite_name).cloned() else {
            let have: Vec<&str> = suites.keys().map(String::as_str).collect();
            let have = if have.is_empty() {
                "none".to_owned()
            } else {
                have.join(", ")
            };
            return Ok(done.call(
                false,
                &format!("unknown suite {} (have: {have})", str_repr(&suite_name)),
                vec![],
            ));
        };
        if me.version().await.is_none() {
            return Ok(done.call(
                false,
                "NeMo Evaluator is not installed.",
                vec![Problem::new(
                    Severity::Error,
                    "ev.not_installed",
                    "evaluator-local is not on PATH.",
                    Some(me.install_fix()),
                )],
            ));
        }
        if let Some(k) = me.key_file()
            && !k.exists()
        {
            return Ok(done.call(
                false,
                &format!("The router key file {} is absent.", k.display()),
                vec![Problem::new(
                    Severity::Warn,
                    "ev.no_key",
                    "no router key for evaluator-local",
                    Some(me.key_fix()),
                )],
            ));
        }
        if let Some(cached) = me.cached(true).await {
            let miss: Vec<String> = unique(&suite.benches)
                .into_iter()
                .filter(|b| !cached.get(b).copied().unwrap_or(false))
                .collect();
            if !miss.is_empty() {
                let fix = format!("evaluator-local prefetch {}", miss.join(" "));
                return Ok(done.call(
                    false,
                    &format!(
                        "Datasets not prefetched: {}. Runs are offline; fetch them once yourself:\n  {fix}",
                        miss.join(", ")
                    ),
                    vec![Problem::new(
                        Severity::Warn,
                        "ev.dataset_missing",
                        format!("datasets not prefetched: {}", miss.join(", ")),
                        Some(fix),
                    )],
                ));
            }
        }
        if let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        let runs = me.runs_dir();
        let (run_id, run_dir) = sp::new_run_dir(&runs, &suite_name)
            .map_err(|e| sp::io_adapter_error(&e, Some(&runs)))?;
        let out_dir = run_dir.join("out");
        let argv: Vec<String> = vec![
            LAUNCHER.into(),
            "run".into(),
            "--suite".into(),
            suite_name.clone(),
            "--output-dir".into(),
            out_dir.to_string_lossy().into_owned(),
        ];
        let started = sp::now();
        let mut meta = Map::new();
        meta.insert("run_id".into(), Value::from(run_id.as_str()));
        meta.insert("suite".into(), Value::from(suite_name.as_str()));
        meta.insert("model".into(), Value::from(suite.model.as_str()));
        meta.insert("ts".into(), Value::from(started));
        meta.insert("verdict".into(), Value::from("running"));
        meta.insert("mode".into(), Value::from("run"));
        meta.insert("benchmarks".into(), Value::from(suite.benches.clone()));
        sp::write_meta(&run_dir, &meta, META_REDACT)
            .map_err(|e| sp::io_adapter_error(&e, Some(&run_dir.join(".run.json.tmp"))))?;
        let task_inner = Arc::clone(me);
        let started_ok = me.live.start(&run_id, &suite_name, started, async move {
            run_task(task_inner, run_dir, argv, meta).await;
        });
        if !started_ok && let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        Ok(done.call(
            true,
            &format!(
                "Started evaluation {run_id} (suite {suite_name}, model {}; {}). It runs in the background: watch the runs list, then 'Show a stored run' with this run id.",
                suite.model,
                suite.benches.join(", ")
            ),
            vec![],
        ))
    }
}

/// `_run_task`: run the wrapper, then record the outcome; dropped early = interrupted by console stop.
async fn run_task(me: Arc<Inner>, run_dir: PathBuf, argv: Vec<String>, meta: Map<String, Value>) {
    let t0 = Instant::now();
    let shared = Arc::new(Mutex::new(meta));
    let mut guard = InterruptGuard {
        run_dir: run_dir.clone(),
        meta: Arc::clone(&shared),
        t0,
        armed: true,
        redact_keys: META_REDACT,
    };
    let r = sp::run_cmd(&me.ctx, &argv, RUN_TIMEOUT, RUN_MAX_BYTES, Some(&run_dir)).await;
    guard.armed = false;
    let mut meta = lock(&shared).clone();
    if let Err(e) = me.finish_run(&run_dir, &mut meta, &r, sp::ms_since(t0)) {
        let (class, text) = sp::os_error(&e, Some(&run_dir.join(".run.json.tmp")));
        meta.insert("verdict".into(), Value::from("error"));
        meta.insert("error".into(), Value::from(sp::exc_text(class, &text)));
        let _ = sp::write_meta(&run_dir, &meta, META_REDACT);
    }
}

/// Build the adapter (Python: registry `build_adapter`; the evaluator has no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(EvaluatorAdapter::new(ctx.clone())))
}
