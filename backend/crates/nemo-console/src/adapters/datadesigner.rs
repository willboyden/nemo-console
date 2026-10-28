//! Data Designer adapter: NVIDIA NeMo Data Designer (a CLI, not a service) as a managed system. OWNER: workstream C.
//! Port of `backend/src/nemo_console/adapters/datadesigner.py` (its module docstring lists the facts it is built on):
//! the `data-designer-local` wrapper (`--version`, `validate`, `preview ... --save-results -o DIR`,
//! `create ... --artifact-path DIR --no-tui` as ONE background run, `check-models`), the fixed parquet converter
//! `data-designer-export IN OUT.ext` (the console itself never reads parquet: it reads only `records.jsonl`), the
//! tool's own `~/.data-designer` yaml config (read-only, api_key never surfaced), config scripts as editable config
//! files, and the user's dataset copies under the datasets root.

pub mod files;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Map, Value};

use self::files::{cap_value, read_records};
use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, SystemAdapter,
};
use crate::adapters::evaluator::{cli_tail, write_private};
use crate::config::Settings;
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, PostCheck,
    Problem, Severity, SystemEvent, SystemId, SystemStatus, ValidationResult,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::config_store::mtime_f64;
use crate::services::pyfmt::{char_prefix, float_repr, str_repr};
use crate::services::pyobj::{Doc, Py, PyExc, py_strip};
use crate::services::pyyaml::safe_load;
use crate::services::support::{
    self as sp, Done, InterruptGuard, LiveSlot, fixed, full, is_str, num, pystr, truthy,
};

/// `DataDesignerAdapter.label` in Python.
pub const LABEL: &str = "Data Designer";

pub const LAUNCHERS: [&str; 1] = ["data-designer-local"];
pub const EXPORTER: &str = "data-designer-export";
pub const RUN_TIMEOUT: f64 = 540.0;
pub const CREATE_TIMEOUT: f64 = 3600.0;
pub const EXPORT_TIMEOUT: f64 = 300.0;
pub const KEEP_RUNS: usize = 100;
pub const EVENT_KIND: &str = "dd.run";
pub const VERSION_TTL: f64 = 300.0;
pub const FORMATS: [&str; 4] = ["parquet", "csv", "jsonl", "json"];
pub const VERDICTS: [&str; 4] = ["succeeded", "failed", "error", "interrupted"];
pub const CREATE_MAX_BYTES: usize = 4_194_304;
pub const MAX_CONFIG_BYTES: u64 = 512 * 1024;
pub const REPORT_CELL: usize = 120;
pub const MAX_REPORT_ROWS: usize = 5;
pub const MAX_YAML_BYTES: u64 = 1024 * 1024;
pub const MAX_DEST_SEGMENTS: usize = 8;
pub const LAB_INSTALLER: &str = "clients/data-designer/install.sh";
pub const LAB_MINT_KEY: &str = "clients/data-designer/mint-key.sh";
pub const PINNED_VERSION: &str = "0.9.3";
pub use self::files::{CELL_CAP, MAX_FILES, MAX_PREVIEW_RECORDS, MAX_RECORDS_ROWS};
const META_REDACT: &[&str] = &["error"];

static NAME_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}"));
static RUN_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"\d{8}T\d{6}-\d{3}-[A-Za-z0-9._-]{1,128}"));
/// The script allowlist: NAME_RE narrowed by the core's file-id shape (lowercase, <= 64 chars, one of
/// .py/.yaml/.yml/.json).
static SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[a-z0-9][a-z0-9._-]{0,52}\.(?:py|yaml|yml|json)"));

/// One `model_providers.yaml` entry as the console shows it (never the api_key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub name: String,
    pub endpoint: String,
    pub provider_type: String,
}

/// One `model_configs.yaml` alias as the console shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Alias {
    pub alias: String,
    pub model: String,
    pub provider: String,
    pub generation_type: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<f64>,
}

fn py_exc(e: PyExc) -> AdapterError {
    AdapterError::failed(e.class, e.message)
}

/// `_num(v)` on a YAML object (OverflowError for an int beyond float range).
fn yaml_num(v: Option<&Py>) -> Result<Option<f64>, AdapterError> {
    match v {
        Some(Py::Float(f)) => Ok(Some(*f)),
        Some(Py::Int(i)) => i.to_f64().map(Some).ok_or_else(|| {
            AdapterError::failed("OverflowError", "int too large to convert to float")
        }),
        _ => Ok(None),
    }
}

/// `str(v or "")` of a YAML object.
fn yaml_str_or_empty(doc: &Doc, v: Option<&Py>) -> Result<String, AdapterError> {
    match v {
        Some(x) if doc.truthy(x) => doc.str_of(x).map_err(py_exc),
        _ => Ok(String::new()),
    }
}

/// `str(int(x))` of a Python float (`int()` truncates; inf / nan raise like Python).
fn int_of_float(x: f64) -> Result<String, AdapterError> {
    if x.is_nan() {
        return Err(AdapterError::failed(
            "ValueError",
            "cannot convert float NaN to integer",
        ));
    }
    if x.is_infinite() {
        return Err(AdapterError::failed(
            "OverflowError",
            "cannot convert float infinity to integer",
        ));
    }
    let s = format!("{:.0}", x.trunc());
    Ok(if s == "-0" { "0".to_owned() } else { s })
}

/// `int(total)` as a JSON integer (exact up to u64; beyond that a float, PORTING.md workstream C deviations).
fn int_value(x: f64) -> Result<Value, AdapterError> {
    let s = int_of_float(x)?;
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Value::from(i));
    }
    if let Ok(u) = s.parse::<u64>() {
        return Ok(Value::from(u));
    }
    Ok(serde_json::Number::from_f64(x.trunc()).map_or(Value::Null, Value::Number))
}

/// The Data Designer adapter (Python `DataDesignerAdapter`).
pub struct DataDesignerAdapter {
    inner: Arc<Inner>,
}

type VersionCache = (Option<Instant>, Option<String>, Option<String>);

/// The parsed yaml document and its entries (each a copied mapping's items).
type YamlEntries = (Doc, Vec<Vec<(Py, Py)>>);

struct Inner {
    ctx: AdapterContext,
    version_cache: Mutex<VersionCache>,
    follow_interval_bits: AtomicU64,
    live: LiveSlot,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

impl DataDesignerAdapter {
    /// `DataDesignerAdapter(ctx)`.
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        DataDesignerAdapter {
            inner: Arc::new(Inner {
                ctx,
                version_cache: Mutex::new((None, None, None)),
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

    /// `a.scripts()`.
    #[must_use]
    pub fn scripts(&self) -> Vec<String> {
        self.inner.scripts()
    }

    /// `a._script_path(name)`.
    pub fn script_path(&self, name: &str) -> Result<PathBuf, AdapterError> {
        self.inner.script_path(name)
    }

    /// `a.providers()`.
    pub fn providers(&self) -> Result<Vec<Provider>, AdapterError> {
        self.inner.providers()
    }

    /// `a.aliases()`.
    pub fn aliases(&self) -> Result<Vec<Alias>, AdapterError> {
        self.inner.aliases()
    }

    /// `a._config_dump()` (the show_config text, as lines).
    pub fn config_dump(&self) -> Result<Vec<String>, AdapterError> {
        self.inner.config_dump()
    }

    /// `a._new_run_dir(script)`.
    pub fn new_run_dir(&self, script: &str) -> std::io::Result<(String, PathBuf)> {
        sp::new_run_dir(&self.inner.runs_dir(), script)
    }

    /// The adapter context.
    #[must_use]
    pub fn ctx(&self) -> &AdapterContext {
        &self.inner.ctx
    }
}

impl Inner {
    fn scripts_root(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("scripts_dir")
    }

    fn datasets_root(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("datasets_root")
    }

    fn config_dir(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("dd_config_dir")
    }

    fn key_file(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("dd_key_file")
    }

    fn runs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("datadesigner").join("runs")
    }

    fn litellm(&self) -> String {
        sp::litellm(&self.ctx)
    }

    fn install_fix(&self) -> String {
        match sp::lab_file(&self.ctx, "data_designer_dir", LAB_INSTALLER) {
            Some(lab) => format!(
                "{}   # pinned data-designer {PINNED_VERSION} + the data-designer-local wrapper",
                lab.display()
            ),
            None => format!(
                "run clients/data-designer/install.sh in the lab repository (pinned data-designer {PINNED_VERSION}; it links the data-designer-local wrapper onto PATH)"
            ),
        }
    }

    fn key_fix(&self) -> String {
        match sp::lab_file(&self.ctx, "data_designer_dir", LAB_MINT_KEY) {
            Some(lab) => format!(
                "{}   # mints the router key file the OpenShell provider (lab-router-dd) stores",
                lab.display()
            ),
            None => "run clients/data-designer/mint-key.sh in the lab repository (it writes ~/.config/data-designer/litellm.key, 0600; the OpenShell provider lab-router-dd stores its value)".to_owned(),
        }
    }

    fn not_installed_problem(&self) -> Problem {
        Problem::new(
            Severity::Error,
            "dd.not_installed",
            "data-designer-local is not on PATH.",
            Some(self.install_fix()),
        )
    }

    // ── scripts ─────────────────────────────────────────────────────────────────────────────

    fn scripts(&self) -> Vec<String> {
        let Some(root) = self.scripts_root() else {
            return Vec::new();
        };
        let Ok(rd) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        names
            .into_iter()
            .filter(|n| {
                let p = root.join(n);
                SCRIPT_RE.is_match(n) && !p.is_symlink() && p.is_file()
            })
            .take(200)
            .collect()
    }

    /// `_script_path` / `config_path`: a script name to its file, refusing anything outside scripts_dir.
    fn script_path(&self, name: &str) -> Result<PathBuf, AdapterError> {
        let unknown = || AdapterError::UnknownId(name.to_owned());
        let root = self.scripts_root().ok_or_else(unknown)?;
        if !SCRIPT_RE.is_match(name) {
            return Err(unknown());
        }
        let raw = root.join(name);
        let base = sp::realpath(root);
        let p = sp::realpath(&raw);
        if raw.is_symlink() || p.parent() != Some(base.as_path()) || !p.is_file() {
            return Err(unknown());
        }
        Ok(p)
    }

    // ── launcher probe ──────────────────────────────────────────────────────────────────────

    async fn launcher(&self) -> (Option<String>, Option<String>) {
        {
            let c = lock(&self.version_cache);
            if let (Some(ts), Some(l)) = (c.0, &c.1)
                && ts.elapsed().as_secs_f64() < VERSION_TTL
            {
                return (Some(l.clone()), c.2.clone());
            }
        }
        let (mut launcher, mut version) = (None, None);
        for name in LAUNCHERS {
            let r = self.run(&[name, "--version"], 20.0, 4096, None).await;
            if r.returncode == 0 {
                launcher = Some(name.to_owned());
                version = crate::services::pyobj::py_splitlines(py_strip(&r.stdout))
                    .first()
                    .map(|l| char_prefix(l, 40).to_owned());
                break;
            }
        }
        *lock(&self.version_cache) = (Some(Instant::now()), launcher.clone(), version.clone());
        (launcher, version)
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

    // ── the tool's own yaml config ──────────────────────────────────────────────────────────

    /// `_read_yaml_entries(filename)`: a list of mappings, or a mapping name -> mapping. A missing, oversized,
    /// symlinked or broken file is zero entries (OSError / YAMLError / ValueError); any other exception the YAML
    /// constructors raise propagates, like in Python.
    fn read_yaml_entries(&self, filename: &str) -> Result<YamlEntries, AdapterError> {
        let empty = || Ok((Doc::default(), Vec::new()));
        let Some(dir) = self.config_dir() else {
            return empty();
        };
        let path = dir.join(filename);
        if path.is_symlink() || !path.is_file() {
            return empty();
        }
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_YAML_BYTES => return empty(),
            Err(_) => return empty(),
            Ok(_) => {}
        }
        let Ok(text) = sp::read_text(&path) else {
            return empty();
        };
        let doc = match safe_load(&text) {
            Ok(d) => d,
            Err(e) if e.is_yaml() || e.class() == "ValueError" => return empty(),
            Err(e) => return Err(AdapterError::failed(e.class(), e.text())),
        };
        let key = if filename == "model_providers.yaml" {
            "name"
        } else {
            "alias"
        };
        let mut root = doc.root.clone();
        if let Some(items) = doc.dict(&root)
            && let Some((_, nested)) = items.iter().find(|(_, v)| doc.list(v).is_some())
        {
            root = nested.clone();
        }
        let mut out = Vec::new();
        if let Some(items) = doc.list(&root) {
            for e in items.iter().take(200) {
                if let Some(d) = doc.dict(e) {
                    out.push(d.to_vec());
                }
            }
        } else if let Some(items) = doc.dict(&root) {
            for (k, v) in items.iter().take(200) {
                if let Some(d) = doc.dict(v) {
                    let mut e = d.to_vec();
                    if !Doc::has(&e, key) {
                        e.push((Py::Str(key.to_owned()), k.clone()));
                    }
                    out.push(e);
                }
            }
        }
        Ok((doc, out))
    }

    fn providers(&self) -> Result<Vec<Provider>, AdapterError> {
        let (doc, entries) = self.read_yaml_entries("model_providers.yaml")?;
        let mut out = Vec::new();
        for e in &entries {
            let Some(Py::Str(name)) = Doc::get(e, "name") else {
                continue;
            };
            if !NAME_RE.is_match(name) {
                continue;
            }
            out.push(Provider {
                name: char_prefix(name, 80).to_owned(),
                endpoint: char_prefix(&yaml_str_or_empty(&doc, Doc::get(e, "endpoint"))?, 160)
                    .to_owned(),
                provider_type: char_prefix(
                    &yaml_str_or_empty(&doc, Doc::get(e, "provider_type"))?,
                    40,
                )
                .to_owned(),
            });
        }
        out.truncate(100);
        Ok(out)
    }

    fn aliases(&self) -> Result<Vec<Alias>, AdapterError> {
        let (doc, entries) = self.read_yaml_entries("model_configs.yaml")?;
        let mut out = Vec::new();
        for e in &entries {
            let Some(Py::Str(alias)) = Doc::get(e, "alias") else {
                continue;
            };
            if !NAME_RE.is_match(alias) {
                continue;
            }
            let ip: &[(Py, Py)] = Doc::get(e, "inference_parameters")
                .and_then(|v| doc.dict(v))
                .unwrap_or(&[]);
            // dict.get(k, default): the entry's own key wins even when its value is None
            let pick = |k: &str| -> Option<&Py> {
                if Doc::has(ip, k) {
                    Doc::get(ip, k)
                } else {
                    Doc::get(e, k)
                }
            };
            let gen_v = if Doc::has(e, "generation_type") {
                Doc::get(e, "generation_type")
            } else {
                Doc::get(ip, "generation_type")
            };
            out.push(Alias {
                alias: char_prefix(alias, 80).to_owned(),
                model: char_prefix(&yaml_str_or_empty(&doc, Doc::get(e, "model"))?, 160).to_owned(),
                provider: char_prefix(&yaml_str_or_empty(&doc, Doc::get(e, "provider"))?, 80)
                    .to_owned(),
                generation_type: match gen_v {
                    Some(Py::Str(g)) => Some(g.clone()),
                    _ => None,
                },
                temperature: yaml_num(pick("temperature"))?,
                top_p: yaml_num(pick("top_p"))?,
                max_tokens: yaml_num(pick("max_tokens"))?,
            });
        }
        out.truncate(100);
        Ok(out)
    }

    fn config_dump(&self) -> Result<Vec<String>, AdapterError> {
        let providers = self.providers()?;
        let aliases = self.aliases()?;
        let shown = |f: &str| match self.config_dir() {
            Some(d) => d.join(f).display().to_string(),
            None => f.to_owned(),
        };
        let mut lines = vec![
            format!(
                "Providers ({}) — key values are never shown:",
                shown("model_providers.yaml")
            ),
            format!("  {:<20} {:<16} endpoint", "name", "type"),
        ];
        for p in &providers {
            lines.push(format!(
                "  {:<20} {:<16} {}",
                char_prefix(&p.name, 20),
                char_prefix(&p.provider_type, 16),
                p.endpoint
            ));
        }
        lines.push(String::new());
        lines.push(format!("Aliases ({}):", shown("model_configs.yaml")));
        lines.push(format!(
            "  {:<16} {:<36} {:<16} {:<10} {:>5} {:>6} {:>8}",
            "alias", "model", "provider", "gen", "temp", "top_p", "max_tok"
        ));
        for a in &aliases {
            let t = a.temperature.map_or_else(|| "-".to_owned(), float_repr);
            let tp = a.top_p.map_or_else(|| "-".to_owned(), float_repr);
            let mt = match a.max_tokens {
                Some(m) => int_of_float(m)?,
                None => "-".to_owned(),
            };
            let gen_s = a.generation_type.clone().filter(|g| !g.is_empty());
            lines.push(format!(
                "  {:<16} {:<36} {:<16} {:<10} {t:>5} {tp:>6} {mt:>8}",
                char_prefix(&a.alias, 16),
                char_prefix(&a.model, 36),
                char_prefix(&a.provider, 16),
                char_prefix(gen_s.as_deref().unwrap_or("-"), 10),
            ));
        }
        lines.truncate(100);
        Ok(lines)
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

    fn event(meta: &Map<String, Value>) -> SystemEvent {
        let get_or = |k: &str| match meta.get(k) {
            None => "?".to_owned(),
            v => pystr(v),
        };
        let mut parts = vec![get_or("script"), get_or("mode")];
        if let Some(records) = num(meta.get("records")) {
            parts.push(format!("{} record(s)", fixed(records, 0, false)));
        }
        if is_str(meta.get("mode"), "create") {
            parts.push(format!(
                "{} → {} ({})",
                get_or("dataset_name"),
                get_or("destination"),
                get_or("format")
            ));
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
        for k in [
            "run_id",
            "script",
            "mode",
            "verdict",
            "num_records_requested",
            "dataset_name",
            "destination",
            "format",
            "batches",
            "duration_ms",
        ] {
            data.insert(k.into(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        data.insert(
            "num_records".into(),
            meta.get("records").cloned().unwrap_or(Value::Null),
        );
        SystemEvent {
            ts: num(meta.get("ts")).unwrap_or(0.0),
            system: SystemId::Datadesigner,
            kind: EVENT_KIND.to_owned(),
            summary: char_prefix(&parts.join(" · "), 300).to_owned(),
            data,
        }
    }

    fn run_dirs(&self) -> Vec<PathBuf> {
        sp::run_dirs(&self.runs_dir(), &RUN_ID_RE, false)
    }

    fn list_runs(&self, limit: usize) -> Vec<SystemEvent> {
        self.run_dirs()
            .into_iter()
            .take(limit)
            .filter_map(|d| self.load_meta(&d))
            .map(|m| Inner::event(&m))
            .collect()
    }

    fn prune(&self) {
        let live = self.live.run_id();
        sp::prune(&self.run_dirs(), KEEP_RUNS, live.as_deref());
    }

    // ── preview ─────────────────────────────────────────────────────────────────────────────

    /// `_export_records(parquet, out)`: (records kept, the exporter's redacted stderr tail on failure).
    async fn export_records(&self, parquet: &Path, out: &Path) -> (Option<usize>, String) {
        if parquet.is_symlink() || !parquet.is_file() {
            let name = parquet
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return (None, format!("{name} is missing"));
        }
        let mut rnd = [0u8; 6];
        let _ = getrandom::fill(&mut rnd);
        let parent = out.parent().unwrap_or(Path::new("/"));
        let tmp = parent.join(format!(".records.{}.jsonl", hex::encode(rnd)));
        let tmp_s = tmp.to_string_lossy().into_owned();
        let pq = parquet.to_string_lossy().into_owned();
        let r = self
            .run(&[EXPORTER, &pq, &tmp_s], EXPORT_TIMEOUT, 256_000, None)
            .await;
        let result = if r.returncode != 0 {
            let src = if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            };
            let tail = redact(&sp::strip_tail(src, 300));
            let err = if tail.is_empty() {
                format!("export exit {}", r.returncode)
            } else {
                tail
            };
            (None, err)
        } else {
            match copy_capped(&tmp, out) {
                Ok(n) => (Some(n), String::new()),
                Err(_) => (None, String::new()),
            }
        };
        let _ = std::fs::remove_file(&tmp);
        result
    }

    async fn preview(
        &self,
        launcher: &str,
        script: &str,
        path: &Path,
        n: i64,
        done: Done,
    ) -> Result<ActionResult, AdapterError> {
        let runs = self.runs_dir();
        let (run_id, run_dir) =
            sp::new_run_dir(&runs, script).map_err(|e| sp::io_adapter_error(&e, Some(&runs)))?;
        let t0 = Instant::now();
        let path_s = path.to_string_lossy().into_owned();
        let dir_s = run_dir.to_string_lossy().into_owned();
        let n_s = n.to_string();
        let argv = [
            launcher,
            "preview",
            &path_s,
            "-n",
            &n_s,
            "--save-results",
            "-o",
            &dir_s,
        ];
        let r = self.run(&argv, RUN_TIMEOUT, 256_000, Some(&run_dir)).await;
        let mut meta = Map::new();
        meta.insert("run_id".into(), Value::from(run_id.as_str()));
        meta.insert("script".into(), Value::from(script));
        meta.insert("mode".into(), Value::from("preview"));
        meta.insert("ts".into(), Value::from(sp::now()));
        meta.insert("num_records_requested".into(), Value::from(n));
        meta.insert("launcher".into(), Value::from(launcher));
        let preview_dir = files::newest_preview_dir(&run_dir);
        let (mut records, mut export_error) = (None, String::new());
        if let Some(pd) = &preview_dir {
            (records, export_error) = self
                .export_records(&pd.join("dataset.parquet"), &run_dir.join("records.jsonl"))
                .await;
        }
        let mut set = |verdict: &str, recs: Value, key: &str, val: String| {
            meta.insert("verdict".into(), Value::from(verdict));
            meta.insert("records".into(), recs);
            meta.insert(key.into(), Value::from(val));
        };
        if r.timed_out {
            set("error", Value::from(0), "error", "timed out".into());
        } else if r.returncode != 0 {
            let src = if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            };
            let t = sp::strip_tail(src, 1500);
            let t = if t.is_empty() {
                format!("exit {}", r.returncode)
            } else {
                t
            };
            set("failed", Value::from(0), "error", t);
        } else if preview_dir.is_none() {
            set(
                "error",
                Value::from(0),
                "error",
                "no preview_results_* directory was written".into(),
            );
        } else if let (Some(recs), Some(pd)) = (records, &preview_dir) {
            let rel = pd
                .strip_prefix(&run_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            set("succeeded", Value::from(recs), "preview_dir", rel);
        } else {
            let detail = if export_error.is_empty() {
                String::new()
            } else {
                format!(": {export_error}")
            };
            set(
                "error",
                Value::from(0),
                "error",
                format!("dataset.parquet could not be converted to records.jsonl{detail}"),
            );
        }
        meta.insert("duration_ms".into(), Value::from(sp::ms_since(t0)));
        sp::write_meta(&run_dir, &meta, META_REDACT)
            .map_err(|e| sp::io_adapter_error(&e, Some(&run_dir.join(".run.json.tmp"))))?;
        self.prune();
        let ok = is_str(meta.get("verdict"), "succeeded");
        let mut lines = vec![Inner::event(&meta).summary, String::new()];
        if ok {
            let pd = pystr(meta.get("preview_dir"));
            let browser = sp::pure_join(&run_dir, &pd)
                .join("sample_records")
                .join("sample_records_browser.html");
            if browser.is_file() && !browser.is_symlink() {
                lines.push(format!(
                    "Per-record browser (HTML, open it locally): {}",
                    browser.display()
                ));
            }
            lines.push(format!(
                "records.jsonl: {} (first {MAX_PREVIEW_RECORDS} records)",
                run_dir.join("records.jsonl").display()
            ));
        } else {
            lines.push("Data Designer did not complete the preview:".to_owned());
            lines.push(match meta.get("error") {
                None => String::new(),
                v => pystr(v),
            });
            lines.push(String::new());
        }
        Ok(done.call(ok, &lines.join("\n"), vec![]))
    }

    // ── create ──────────────────────────────────────────────────────────────────────────────

    async fn finish_create(
        &self,
        run_dir: &Path,
        meta: &mut Map<String, Value>,
        r: &CommandResult,
        dest: &Path,
        target: Option<&Path>,
        ms: i64,
    ) -> std::io::Result<()> {
        meta.insert("duration_ms".into(), Value::from(ms));
        if r.timed_out || r.returncode != 0 {
            let src = if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            };
            let t = sp::strip_tail(src, 1500);
            let t = if !t.is_empty() {
                t
            } else if r.timed_out {
                "timed out".to_owned()
            } else {
                format!("exit {}", r.returncode)
            };
            meta.insert("verdict".into(), Value::from("failed"));
            meta.insert("error".into(), Value::from(t));
        } else {
            let data_dir = run_dir.join(pystr(meta.get("dataset_name")));
            let md = sp::load_json_dict(&data_dir.join("metadata.json"), Some(8 * 1024 * 1024));
            let batches = md
                .as_ref()
                .map(|m| files::batch_files(&data_dir, m))
                .unwrap_or_default();
            match md {
                None => {
                    meta.insert("verdict".into(), Value::from("error"));
                    meta.insert(
                        "error".into(),
                        Value::from("the CLI exited 0 but no metadata.json was written"),
                    );
                }
                Some(_) if batches.is_empty() => {
                    meta.insert("verdict".into(), Value::from("error"));
                    meta.insert(
                        "error".into(),
                        Value::from("metadata.json lists no parquet batch files"),
                    );
                }
                Some(md) => {
                    meta.insert(
                        "records".into(),
                        sp::num_value(md.get("actual_num_records")),
                    );
                    meta.insert("batches".into(), Value::from(batches.len()));
                    let fmt = pystr(meta.get("format"));
                    let kept = self
                        .export_batches(run_dir, &batches, target, dest, &fmt)
                        .await;
                    if kept.is_none() {
                        meta.insert("verdict".into(), Value::from("failed"));
                        meta.insert(
                            "error".into(),
                            Value::from(
                                "converting the parquet batches failed (partial copies may remain)",
                            ),
                        );
                    } else {
                        meta.insert("verdict".into(), Value::from("succeeded"));
                    }
                }
            }
        }
        sp::write_meta(run_dir, meta, META_REDACT)?;
        self.prune();
        Ok(())
    }

    /// `_export_batches`: records.jsonl in the run dir (capped) + the user's copy under dest. `None` when a
    /// conversion failed (partial copies stay on disk).
    async fn export_batches(
        &self,
        run_dir: &Path,
        batches: &[PathBuf],
        target: Option<&Path>,
        dest: &Path,
        fmt: &str,
    ) -> Option<usize> {
        if sp::mkdir_private(dest).is_err() {
            return None;
        }
        let records_path = run_dir.join("records.jsonl");
        let mut kept = 0usize;
        let mut wrote_any = false;
        for (i, batch) in batches.iter().enumerate() {
            if target.is_none()
                && let Some(name) = batch.file_name()
            {
                let _ = files::copy2(batch, &dest.join(name));
            }
            if kept >= MAX_PREVIEW_RECORDS {
                continue; // records.jsonl is full; finish copying the parquet batches
            }
            let mut rnd = [0u8; 6];
            let _ = getrandom::fill(&mut rnd);
            let tmp = run_dir.join(format!(".batch.{i}.{}.jsonl", hex::encode(rnd)));
            let mut user_tmp: Option<PathBuf> = None;
            let step = self
                .export_one(
                    batch,
                    &tmp,
                    &mut user_tmp,
                    &records_path,
                    target,
                    fmt,
                    i,
                    kept,
                    wrote_any,
                )
                .await;
            for t in std::iter::once(&tmp).chain(user_tmp.as_ref()) {
                let _ = std::fs::remove_file(t);
            }
            (kept, wrote_any) = step?;
        }
        if let Some(target) = target {
            let finish = || -> std::io::Result<()> {
                use std::io::Write as _;
                if fmt == "json" {
                    let mut out = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(target)?;
                    out.write_all(if wrote_any { b"\n]\n" } else { b"]\n" })?;
                    drop(out);
                    if !wrote_any {
                        std::fs::write(target, b"[]\n")?;
                    }
                }
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600))
            };
            if finish().is_err() {
                return None;
            }
        }
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&records_path, std::fs::Permissions::from_mode(0o600));
        }
        Some(kept)
    }

    /// One batch of `_export_batches`: convert to jsonl, append to records.jsonl, append to the user copy.
    #[allow(clippy::too_many_arguments)]
    async fn export_one(
        &self,
        batch: &Path,
        tmp: &Path,
        user_tmp: &mut Option<PathBuf>,
        records_path: &Path,
        target: Option<&Path>,
        fmt: &str,
        i: usize,
        kept: usize,
        wrote_any: bool,
    ) -> Option<(usize, bool)> {
        let b = batch.to_string_lossy().into_owned();
        let t = tmp.to_string_lossy().into_owned();
        let r = self
            .run(&[EXPORTER, &b, &t], EXPORT_TIMEOUT, 256_000, None)
            .await;
        if r.returncode != 0 {
            return None;
        }
        let kept = files::append_capped(tmp, records_path, kept).ok()?;
        let mut wrote_any = wrote_any;
        if let Some(target) = target {
            if fmt == "csv" {
                let run_dir = tmp.parent().unwrap_or(Path::new("/"));
                let mut rnd = [0u8; 6];
                let _ = getrandom::fill(&mut rnd);
                let ut = run_dir.join(format!(".batch.{i}.{}.csv", hex::encode(rnd)));
                *user_tmp = Some(ut.clone());
                let u = ut.to_string_lossy().into_owned();
                let rc = self
                    .run(&[EXPORTER, &b, &u], EXPORT_TIMEOUT, 256_000, None)
                    .await;
                if rc.returncode != 0 {
                    return None;
                }
                wrote_any = files::append_user_copy(&ut, target, fmt, i == 0, wrote_any).ok()?;
            } else {
                wrote_any = files::append_user_copy(tmp, target, fmt, i == 0, wrote_any).ok()?;
            }
        }
        Some((kept, wrote_any))
    }

    // ── report ──────────────────────────────────────────────────────────────────────────────

    fn report_text(&self, run_id: &str) -> (bool, String) {
        let run_dir = if run_id.is_empty() {
            let runs = self.list_runs(1);
            let Some(first) = runs.first() else {
                return (false, "no stored runs yet".to_owned());
            };
            self.runs_dir().join(pystr(first.data.get("run_id")))
        } else {
            if !RUN_ID_RE.is_match(run_id) {
                return (false, "unknown run id".to_owned());
            }
            self.runs_dir().join(run_id)
        };
        let Some(meta) = self.load_meta(&run_dir) else {
            return (false, "unknown run id".to_owned());
        };
        let mut lines = vec![Inner::event(&meta).summary, String::new()];
        let verdict = meta.get("verdict");
        if is_str(verdict, "running") {
            let ts = num(meta.get("ts"))
                .filter(|t| *t != 0.0)
                .unwrap_or_else(sp::now);
            let mins = ((sp::now() - ts) / 60.0).max(0.0);
            lines.push(format!(
                "Running for {} min. This report fills in when the run ends.",
                fixed(mins, 0, false)
            ));
            lines.push(String::new());
        }
        if is_str(verdict, "error") || is_str(verdict, "failed") {
            lines.push("Data Designer did not complete the run:".to_owned());
            lines.push(match meta.get("error") {
                None => String::new(),
                v => pystr(v),
            });
            lines.push(String::new());
        }
        if is_str(verdict, "interrupted") {
            lines.push("Interrupted: the console stopped or restarted while it ran.".to_owned());
            lines.push(String::new());
        }
        let mode = pystr(meta.get("mode"));
        if mode == "preview"
            && let Some(Value::String(pd)) = meta.get("preview_dir")
        {
            let browser = sp::pure_join(&run_dir, pd)
                .join("sample_records")
                .join("sample_records_browser.html");
            if browser.is_file() && !browser.is_symlink() {
                lines.push(format!(
                    "Per-record browser (HTML, open it locally): {}",
                    browser.display()
                ));
            }
        }
        if mode == "create"
            && let Some(root) = self.datasets_root()
        {
            let dest_rel = if truthy(meta.get("destination")) {
                pystr(meta.get("destination"))
            } else {
                String::new()
            };
            let dest = sp::pure_join(root, &dest_rel);
            if is_str(meta.get("format"), "parquet") {
                lines.push(format!(
                    "Dataset copy (parquet batches): {}",
                    dest.display()
                ));
            } else {
                let name = if truthy(meta.get("dataset_name")) {
                    pystr(meta.get("dataset_name"))
                } else {
                    "dataset".to_owned()
                };
                let file = format!("{name}.{}", pystr(meta.get("format")));
                lines.push(format!(
                    "Dataset copy: {}",
                    sp::pure_join(&dest, &file).display()
                ));
            }
        }
        let (rows, columns) = read_records(&run_dir.join("records.jsonl"));
        if !rows.is_empty() {
            let show: Vec<String> = if columns.is_empty() {
                vec!["?".to_owned()]
            } else {
                columns.into_iter().take(8).collect()
            };
            lines.push(String::new());
            lines.push("First records (at most 5, cells truncated at 120 chars):".to_owned());
            lines.push(String::new());
            lines.push(format!("| {} |", show.join(" | ")));
            lines.push(format!(
                "| {} |",
                show.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
            ));
            for row in rows.iter().take(MAX_REPORT_ROWS) {
                let cells: Vec<String> = show
                    .iter()
                    .map(|c| {
                        let s = match row.get(c) {
                            None => String::new(),
                            v => pystr(v),
                        };
                        let cell = char_prefix(&redact(&s), REPORT_CELL).replace('|', "\\|");
                        if cell.is_empty() {
                            " ".to_owned()
                        } else {
                            cell
                        }
                    })
                    .collect();
                lines.push(format!("| {} |", cells.join(" | ")));
            }
            lines.push(String::new());
            lines.push(format!(
                "records.jsonl: {} (capped at {MAX_PREVIEW_RECORDS} records)",
                run_dir.join("records.jsonl").display()
            ));
        }
        (true, lines.join("\n"))
    }
}

/// The preview's records.jsonl: the first MAX_PREVIEW_RECORDS lines of the converted file, byte for byte, 0600.
fn copy_capped(tmp: &Path, out: &Path) -> std::io::Result<usize> {
    use std::io::Write as _;
    let mut dst = std::fs::File::create(out)?;
    let mut count = 0usize;
    let mut err = None;
    files::for_each_line(tmp, |line| {
        if count >= MAX_PREVIEW_RECORDS {
            return false;
        }
        if let Err(e) = dst.write_all(line) {
            err = Some(e);
            return false;
        }
        if line.ends_with(b"\n") {
            count += 1;
        }
        true
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    drop(dst);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600))?;
    Ok(count)
}

/// `_dest_subdir_ok(sub)`.
#[must_use]
pub fn dest_subdir_ok(sub: &str) -> bool {
    if sub.is_empty() || sub.starts_with('/') || sub.contains('\\') || sub.chars().count() > 256 {
        return false;
    }
    let parts: Vec<&str> = sub.split('/').collect();
    parts.len() <= MAX_DEST_SEGMENTS
        && parts
            .iter()
            .all(|p| !matches!(*p, "" | "." | "..") && NAME_RE.is_match(p))
}

fn param(key: &str, label: &str, type_: ParamType, help: &str) -> ParamSpec {
    let mut p = ParamSpec::new(key, label, help, type_);
    p.requires_restart = false;
    p
}

/// `str(params.get(k) or "")`.
fn param_str(v: Option<&Value>) -> String {
    if truthy(v) { pystr(v) } else { String::new() }
}

/// `n` of `params.get(key, default)` when it is an int (not a bool) within [lo, hi].
fn int_param(
    params: &Map<String, Value>,
    key: &str,
    default: i64,
    lo: i64,
    hi: i64,
) -> Option<i64> {
    match params.get(key) {
        None => Some(default),
        Some(v) if sp::is_int(Some(v)) => v.as_i64().filter(|n| (lo..=hi).contains(n)),
        Some(_) => None,
    }
}

#[async_trait]
impl SystemAdapter for DataDesignerAdapter {
    fn id(&self) -> SystemId {
        SystemId::Datadesigner
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    #[allow(clippy::too_many_lines)]
    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let me = &self.inner;
        let ((launcher, version), (router_ok, router_detail)) =
            tokio::join!(me.launcher(), sp::router_up(&me.ctx));
        let scripts = me.scripts();
        let aliases = me.aliases()?;
        let last = me.list_runs(1);
        let litellm = me.litellm();
        let mut problems = Vec::new();
        let mut components = vec![
            Component {
                kind: "process".into(),
                name: launcher
                    .clone()
                    .unwrap_or_else(|| "data-designer-local".to_owned()),
                state: if launcher.is_some() {
                    "running"
                } else {
                    "absent"
                }
                .into(),
                image: None,
                detail: Some(match &launcher {
                    Some(l) => format!(
                        "{l} {} (OpenShell-contained: every run executes in a sandboxed runner; the router key lives in the OpenShell credential store — never in the console or the sandbox)",
                        version.clone().unwrap_or_default()
                    ),
                    None => "data-designer-local is not on PATH (installed by clients/data-designer/install.sh)".to_owned(),
                }),
            },
            Component {
                kind: "http".into(),
                name: format!("router {litellm}"),
                state: if router_ok { "running" } else { "stopped" }.into(),
                image: None,
                detail: Some(format!(
                    "{router_detail} on GET /health/liveliness (generation goes through it)"
                )),
            },
        ];
        if let Some((rid, script, started)) = me.live.info() {
            let mins = ((sp::now() - started) / 60.0).max(0.0);
            components.push(Component {
                kind: "process".into(),
                name: format!("create {script}"),
                state: "running".into(),
                image: None,
                detail: Some(format!(
                    "dataset build {rid}, running for {} min (one at a time; it shows in the runs list)",
                    fixed(mins, 0, false)
                )),
            });
        }
        if launcher.is_none() {
            problems.push(Problem::new(
                Severity::Error,
                "dd.not_installed",
                "Data Designer is not installed (or not on the console's PATH).",
                Some(me.install_fix()),
            ));
        }
        if !router_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "dd.router_down",
                format!(
                    "The router ({litellm}) is not answering; preview/create/check-models will fail."
                ),
                Some(format!(
                    "curl -s -o /dev/null -w '%{{http_code}}\\n' {litellm}/health/liveliness"
                )),
            ));
        }
        match me.scripts_root() {
            None => problems.push(Problem::new(
                Severity::Warn,
                "dd.no_scripts",
                "No scripts directory is configured for Data Designer.",
                Some("set NEMO_CONSOLE_DD_SCRIPTS_DIR and restart nemo-console".to_owned()),
            )),
            Some(root) if !root.is_dir() => problems.push(Problem::new(
                Severity::Warn,
                "dd.no_scripts",
                format!("The scripts directory {} does not exist.", root.display()),
                Some(format!(
                    "mkdir -p {} && add a config script (.py defining load_config_builder(), or .yaml/.yml/.json)",
                    root.display()
                )),
            )),
            Some(root) if scripts.is_empty() => problems.push(Problem::new(
                Severity::Warn,
                "dd.no_scripts",
                format!("No config scripts under {}.", root.display()),
                Some(
                    "add a config script: a .py defining load_config_builder(), or a .yaml/.yml/.json config"
                        .to_owned(),
                ),
            )),
            Some(_) => {}
        }
        if launcher.is_some()
            && let Some(k) = me.key_file()
            && !k.exists()
        {
            problems.push(Problem::new(
                Severity::Warn,
                "dd.no_key",
                format!(
                    "The wrapper's LITELLM_DD_KEY source file {} is absent; runs through the lab router will fail.",
                    k.display()
                ),
                Some(me.key_fix()),
            ));
        }
        if let Some(cfg) = me.config_dir()
            && !cfg.is_dir()
        {
            problems.push(Problem::new(
                Severity::Warn,
                "dd.no_config_dir",
                format!(
                    "{} does not exist yet: no model providers or aliases are configured.",
                    cfg.display()
                ),
                Some(
                    "run any data-designer-local command once (it creates the directory)"
                        .to_owned(),
                ),
            ));
        }
        let health = match (&launcher, router_ok) {
            (None, _) => Health::Down,
            (_, false) => Health::Degraded,
            _ => Health::Up,
        };
        let mut summary = format!("{} scripts · {} aliases", scripts.len(), aliases.len());
        if let Some(l) = last.first() {
            summary.push_str(&format!(" · last run: {}", l.summary));
        }
        Ok(SystemStatus {
            system: SystemId::Datadesigner,
            health,
            version,
            summary: char_prefix(&summary, 300).to_owned(),
            endpoints: vec![Endpoint {
                name: "router (generation)".into(),
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
        let scripts = me.scripts();
        let runs = me.list_runs(KEEP_RUNS);
        let pt = |k: &str, v: f64| {
            let mut p = MetricPoint::new(k, v);
            "1".clone_into(&mut p.unit);
            p
        };
        #[allow(clippy::cast_precision_loss)]
        let mut points = vec![
            pt("dd.scripts", scripts.len() as f64),
            pt("dd.runs", runs.len() as f64),
        ];
        if !runs.is_empty() {
            points.push(pt(
                "dd.live_running",
                if me.live.run_id().is_some() { 1.0 } else { 0.0 },
            ));
            if let Some(n) = runs.iter().find_map(|r| num(r.data.get("num_records"))) {
                points.push(pt("dd.last_records", n));
            }
            for verdict in VERDICTS {
                let n = runs
                    .iter()
                    .filter(|r| is_str(r.data.get("verdict"), verdict))
                    .count();
                #[allow(clippy::cast_precision_loss)]
                points.push(pt(
                    &format!("dd.runs_by_verdict{{verdict={verdict}}}"),
                    n as f64,
                ));
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Datadesigner,
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
        let root = self.inner.scripts_root();
        Ok(self
            .inner
            .scripts()
            .into_iter()
            .map(|name| ConfigFileSpec {
                id: name.clone(),
                label: name.clone(),
                description: "A Data Designer dataset config script. Validating or applying runs `data-designer-local validate`, which EXECUTES this script locally (it calls its load_config_builder()) with your privileges.".into(),
                display_path: root.map_or_else(|| name.clone(), |r| r.join(&name).display().to_string()),
                format: if name.ends_with(".yaml") || name.ends_with(".yml") {
                    "yaml"
                } else {
                    "text"
                }
                .into(),
                editable: true,
                why_readonly: None,
                validators: vec!["data-designer-local validate".into()],
                authoritative: true,
                apply_effect: "takes effect on the next preview/create run; nothing to restart".into(),
            })
            .collect())
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        self.inner.script_path(file_id)
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
            .find(|s| s.id == file_id)
            .ok_or_else(|| {
                AdapterError::failed("RuntimeError", "coroutine raised StopIteration")
            })?;
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
        let name = "data-designer-local validate";
        let result =
            |ok: bool, diagnostics: Vec<Diagnostic>, note: Option<String>| ValidationResult {
                ok,
                validator: name.into(),
                authoritative: true,
                diagnostics,
                note,
            };
        let err = |m: &str| Diagnostic {
            severity: Severity::Error,
            message: m.to_owned(),
            line: None,
        };
        let (launcher, _) = me.launcher().await;
        let Some(launcher) = launcher else {
            return Ok(result(
                false,
                vec![err("data-designer-local is not installed.")],
                None,
            ));
        };
        let tmp_dir = me.ctx.state_dir.join("datadesigner").join("tmp");
        sp::mkdir_private(&tmp_dir).map_err(|e| sp::io_adapter_error(&e, Some(&tmp_dir)))?;
        let id = Path::new(file_id);
        let stem = id
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let suffix = id
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        let mut rnd = [0u8; 6];
        let _ = getrandom::fill(&mut rnd);
        let tmp = tmp_dir.join(format!("{stem}.{}{suffix}", hex::encode(rnd)));
        let r = match write_private(&tmp, content) {
            Ok(()) => {
                let t = tmp.to_string_lossy().into_owned();
                let r = me
                    .run(&[&launcher, "validate", &t], RUN_TIMEOUT, 256_000, None)
                    .await;
                let _ = std::fs::remove_file(&tmp);
                r
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(sp::io_adapter_error(&e, Some(&tmp)));
            }
        };
        if r.timed_out {
            return Ok(result(false, vec![err("validation timed out")], None));
        }
        if r.returncode == 0 {
            return Ok(result(
                true,
                vec![],
                Some("Validation ran `data-designer-local validate`, which EXECUTES this config script locally (it calls its load_config_builder()).".to_owned()),
            ));
        }
        let tail = cli_tail(&r);
        let skip = tail.len().saturating_sub(10);
        Ok(result(
            false,
            tail.iter().skip(skip).map(|m| err(m)).collect(),
            None,
        ))
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        self.config_path(file_id)?;
        let mut r = ApplyResult::new(
            true,
            "nothing to restart: scripts are read by the next preview/create run",
        );
        r.post_check = PostCheck::Healthy;
        Ok(r)
    }

    #[allow(clippy::too_many_lines)]
    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let names = self.inner.scripts();
        let script_param = || {
            let mut p = param(
                "script",
                "Config script",
                ParamType::Enum,
                "A config script in the scripts dir. preview/create/validate EXECUTE it locally (it calls its load_config_builder()).",
            );
            p.choices = (!names.is_empty()).then(|| names.clone());
            p.default = names
                .first()
                .map_or(Value::Null, |n| Value::from(n.as_str()));
            p
        };
        let mut n_prev = param(
            "num_records",
            "Records",
            ParamType::Int,
            "Sample records to generate (1–100).",
        );
        n_prev.default = Value::from(5);
        n_prev.min = Some(1.0);
        n_prev.max = Some(100.0);
        let mut preview = ActionSpec::new(
            "preview",
            "Preview records",
            "Runs `data-designer-local preview <script> -n N --save-results`: N sample records through the configured models (spends router tokens), keeping report.html, the per-record HTML and records.jsonl in the console's state dir (newest 100 runs). disruptive: it EXECUTES the config script locally and spends router tokens.",
            Danger::Disruptive,
        );
        preview.params = vec![script_param(), n_prev];
        let mut n_create = param(
            "num_records",
            "Records",
            ParamType::Int,
            "Dataset size (1–100000).",
        );
        n_create.flag = Some("--num-records".into());
        n_create.default = Value::from(100);
        n_create.min = Some(1.0);
        n_create.max = Some(100_000.0);
        let mut dsname = param(
            "dataset_name",
            "Dataset name",
            ParamType::String,
            "Identifier for the dataset (letters, digits and . _ -). Default: dataset_<script stem>.",
        );
        dsname.flag = Some("--dataset-name".into());
        let dest = param(
            "destination_subdir",
            "Destination subdirectory",
            ParamType::String,
            "Relative path under the datasets root (no .., no leading /). Default: the dataset name.",
        );
        let mut fmt = param(
            "format",
            "Copy format",
            ParamType::Enum,
            "Format of the copy written under the datasets root; the run dir always keeps the parquet batches and records.jsonl (first 2000 records).",
        );
        fmt.choices = Some(FORMATS.iter().map(|s| (*s).to_owned()).collect());
        fmt.default = Value::from("parquet");
        let mut create = ActionSpec::new(
            "create",
            "Create a dataset",
            "Runs `data-designer-local create` in the background (ONE at a time): builds the full dataset through the configured models (spends router tokens, takes minutes), writes the parquet batches into the run dir and a converted copy (your chosen format) under the datasets root. The run shows in the runs list; 'Show a stored run' reports it.",
            Danger::Disruptive,
        );
        create.params = vec![script_param(), n_create, dsname, dest, fmt];
        let mut report = ActionSpec::new(
            "report",
            "Show a stored run",
            "The report of a stored run (newest first): verdict, record count, destination and a small table of the first records.",
            Danger::Safe,
        );
        report.params = vec![param(
            "run_id",
            "Run",
            ParamType::String,
            "A run id from the runs list; empty = the newest run.",
        )];
        let mut check = ActionSpec::new(
            "check_models",
            "Check models",
            "`data-designer-local check-models <script>`: a tiny generation against every model the config references, plus a connectivity probe to each referenced MCP tool. disruptive: it EXECUTES the config script locally and makes real model calls.",
            Danger::Disruptive,
        );
        check.params = vec![script_param()];
        let show = ActionSpec::new(
            "show_config",
            "Show provider config",
            "Read-only dump of the tool's own provider and alias tables (~/.data-designer/model_providers.yaml + model_configs.yaml); key values are never shown.",
            Danger::Safe,
        );
        Ok(vec![preview, create, report, check, show])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let done = Done::start();
        let me = &self.inner;
        if !["preview", "create", "report", "check_models", "show_config"].contains(&action_id) {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        if action_id == "create" {
            return self.start_create(params, done).await;
        }
        if action_id == "report" {
            let (ok, text) = me.report_text(&param_str(params.get("run_id")));
            return Ok(done.call(ok, &text, vec![]));
        }
        let (launcher, _) = me.launcher().await;
        let Some(launcher) = launcher else {
            return Ok(done.call(
                false,
                "Data Designer is not installed.",
                vec![me.not_installed_problem()],
            ));
        };
        if action_id == "show_config" {
            return Ok(done.call(true, &me.config_dump()?.join("\n"), vec![]));
        }
        let script = param_str(params.get("script"));
        let Ok(path) = me.script_path(&script) else {
            return Ok(done.call(
                false,
                &format!("unknown script {}", str_repr(&script)),
                vec![],
            ));
        };
        if action_id == "check_models" {
            let p = path.to_string_lossy().into_owned();
            let r = me
                .run(&[&launcher, "check-models", &p], RUN_TIMEOUT, 256_000, None)
                .await;
            let mut out = r.stdout.clone();
            if !r.stderr.is_empty() {
                out.push('\n');
                out.push_str(&r.stderr);
            }
            return Ok(done.call(r.returncode == 0, py_strip(&out), vec![]));
        }
        let Some(n) = int_param(params, "num_records", 5, 1, 100) else {
            return Ok(done.call(
                false,
                "num_records must be an integer from 1 to 100",
                vec![],
            ));
        };
        me.preview(&launcher, &script, &path, n, done).await
    }

    async fn records_page(
        &self,
        run_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value, AdapterError> {
        let me = &self.inner;
        if !RUN_ID_RE.is_match(run_id) {
            return Err(AdapterError::UnknownId(run_id.to_owned()));
        }
        let run_dir = me.runs_dir().join(run_id);
        let meta = me
            .load_meta(&run_dir)
            .ok_or_else(|| AdapterError::UnknownId(run_id.to_owned()))?;
        let (rows, columns) = read_records(&run_dir.join("records.jsonl"));
        let start = usize::try_from(offset.max(0)).unwrap_or(usize::MAX);
        let len = usize::try_from(limit.max(0)).unwrap_or(0);
        let mut out_rows = Vec::new();
        let mut truncated = false;
        for row in rows.iter().skip(start).take(len) {
            let mut capped = Map::new();
            for (k, v) in row {
                capped.insert(k.clone(), cap_value(v));
            }
            if capped
                .values()
                .any(|v| matches!(v, Value::String(s) if s.chars().count() >= CELL_CAP))
            {
                truncated = true;
            }
            out_rows.push(Value::Object(capped));
        }
        let total = match num(meta.get("records")) {
            Some(t) => int_value(t)?,
            None => Value::from(rows.len()),
        };
        let mut out = Map::new();
        out.insert("run_id".into(), Value::from(run_id));
        out.insert("total".into(), total);
        out.insert(
            "columns".into(),
            Value::Array(columns.into_iter().map(Value::String).collect()),
        );
        out.insert("rows".into(), Value::Array(out_rows));
        out.insert("truncated".into(), Value::Bool(truncated));
        Ok(Value::Object(out))
    }

    async fn run_files(&self, run_id: &str) -> Result<Value, AdapterError> {
        let me = &self.inner;
        if !RUN_ID_RE.is_match(run_id) {
            return Err(AdapterError::UnknownId(run_id.to_owned()));
        }
        let run_dir = me.runs_dir().join(run_id);
        if me.load_meta(&run_dir).is_none() {
            return Err(AdapterError::UnknownId(run_id.to_owned()));
        }
        let files: Vec<Value> = files::walk_files(&run_dir)
            .into_iter()
            .map(|(path, bytes)| {
                let mut m = Map::new();
                m.insert("path".into(), Value::from(path));
                m.insert("bytes".into(), Value::from(bytes));
                Value::Object(m)
            })
            .collect();
        let mut out = Map::new();
        out.insert("run_id".into(), Value::from(run_id));
        out.insert("files".into(), Value::Array(files));
        Ok(Value::Object(out))
    }
}

impl DataDesignerAdapter {
    fn busy_result(&self, done: Done) -> Option<ActionResult> {
        let busy = self.inner.live.run_id()?;
        Some(done.call(
            false,
            &format!(
                "A dataset build is already running ({busy}); one runs at a time. Wait for it to finish: it shows in the runs list."
            ),
            vec![Problem::new(
                Severity::Warn,
                "dd.create.busy",
                format!("dataset build {busy} is still running"),
                Some(format!("'Show a stored run' with run id {busy} shows its progress")),
            )],
        ))
    }

    #[allow(clippy::too_many_lines)]
    async fn start_create(
        &self,
        params: &Map<String, Value>,
        done: Done,
    ) -> Result<ActionResult, AdapterError> {
        let me = &self.inner;
        if let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        let refuse = |msg: &str| Ok(done.call(false, msg, vec![]));
        let script = param_str(params.get("script"));
        let Ok(path) = me.script_path(&script) else {
            return refuse(&format!("unknown script {}", str_repr(&script)));
        };
        let Some(n) = int_param(params, "num_records", 100, 1, 100_000) else {
            return refuse("num_records must be an integer from 1 to 100000");
        };
        let mut dataset_name = param_str(params.get("dataset_name"));
        if dataset_name.is_empty() {
            let stem = Path::new(&script)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            dataset_name = format!("dataset_{stem}");
        }
        if !NAME_RE.is_match(&dataset_name) {
            return refuse("dataset_name must use letters, digits and . _ - only");
        }
        let mut dest_sub = param_str(params.get("destination_subdir"));
        if dest_sub.is_empty() {
            dest_sub.clone_from(&dataset_name);
        }
        if !dest_subdir_ok(&dest_sub) {
            return refuse(
                "destination_subdir must be a relative path under the datasets root (segments of letters, digits and . _ -; no .., no leading /)",
            );
        }
        let fmt = match params.get("format") {
            v if truthy(v) => v.cloned().unwrap_or(Value::Null),
            _ => Value::from("parquet"),
        };
        let fmt = match &fmt {
            Value::String(f) if FORMATS.contains(&f.as_str()) => f.clone(),
            _ => return refuse(&format!("format must be one of {}", FORMATS.join(", "))),
        };
        let Some(root) = me.datasets_root() else {
            return Ok(done.call(
                false,
                "no datasets root is configured",
                vec![Problem::new(
                    Severity::Error,
                    "dd.no_datasets_root",
                    "NEMO_CONSOLE_DD_DATASETS_ROOT is not set.",
                    Some("set NEMO_CONSOLE_DD_DATASETS_ROOT and restart nemo-console".to_owned()),
                )],
            ));
        };
        let root_r = sp::realpath(root);
        let dest = sp::realpath(&root.join(&dest_sub));
        if dest == root_r || !dest.starts_with(&root_r) {
            return refuse("destination_subdir resolves outside the datasets root");
        }
        if dest.exists() && !dest.is_dir() {
            return refuse(&format!(
                "destination {} exists and is not a directory",
                dest.display()
            ));
        }
        let target = (fmt != "parquet").then(|| dest.join(format!("{dataset_name}.{fmt}")));
        let (launcher, _) = me.launcher().await;
        let Some(launcher) = launcher else {
            return Ok(done.call(
                false,
                "Data Designer is not installed.",
                vec![me.not_installed_problem()],
            ));
        };
        if let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        let runs = me.runs_dir();
        let (run_id, run_dir) =
            sp::new_run_dir(&runs, &script).map_err(|e| sp::io_adapter_error(&e, Some(&runs)))?;
        let argv: Vec<String> = vec![
            launcher.clone(),
            "create".into(),
            path.to_string_lossy().into_owned(),
            "--num-records".into(),
            n.to_string(),
            "--dataset-name".into(),
            dataset_name.clone(),
            "--artifact-path".into(),
            run_dir.to_string_lossy().into_owned(),
            "--no-tui".into(),
        ];
        let started = sp::now();
        let mut meta = Map::new();
        meta.insert("run_id".into(), Value::from(run_id.as_str()));
        meta.insert("script".into(), Value::from(script.as_str()));
        meta.insert("mode".into(), Value::from("create"));
        meta.insert("ts".into(), Value::from(started));
        meta.insert("verdict".into(), Value::from("running"));
        meta.insert("num_records_requested".into(), Value::from(n));
        meta.insert("dataset_name".into(), Value::from(dataset_name.as_str()));
        meta.insert("destination".into(), Value::from(dest_sub.as_str()));
        meta.insert("format".into(), Value::from(fmt.as_str()));
        meta.insert("launcher".into(), Value::from(launcher.as_str()));
        sp::write_meta(&run_dir, &meta, META_REDACT)
            .map_err(|e| sp::io_adapter_error(&e, Some(&run_dir.join(".run.json.tmp"))))?;
        let task_inner = Arc::clone(me);
        let task_dest = dest.clone();
        let started_ok = me.live.start(&run_id, &script, started, async move {
            create_task(task_inner, run_dir, argv, meta, task_dest, target).await;
        });
        if !started_ok && let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        Ok(done.call(
            true,
            &format!(
                "Started dataset build {run_id} ({script}; {n} records, dataset {dataset_name}, copy {fmt} → {}). It runs in the background: watch the runs list, then 'Show a stored run' with this run id.",
                dest.display()
            ),
            vec![],
        ))
    }
}

/// `_create_task`: run `create`, then record the outcome. Dropped before the CLI returned = interrupted by console
/// stop; dropped while finishing writes nothing (Python's `finishing` flag).
async fn create_task(
    me: Arc<Inner>,
    run_dir: PathBuf,
    argv: Vec<String>,
    meta: Map<String, Value>,
    dest: PathBuf,
    target: Option<PathBuf>,
) {
    let t0 = Instant::now();
    let shared = Arc::new(Mutex::new(meta));
    let mut guard = InterruptGuard {
        run_dir: run_dir.clone(),
        meta: Arc::clone(&shared),
        t0,
        armed: true,
        redact_keys: META_REDACT,
    };
    let r = sp::run_cmd(
        &me.ctx,
        &argv,
        CREATE_TIMEOUT,
        CREATE_MAX_BYTES,
        Some(&run_dir),
    )
    .await;
    guard.armed = false;
    let mut meta = lock(&shared).clone();
    let ms = sp::ms_since(t0);
    if let Err(e) = me
        .finish_create(&run_dir, &mut meta, &r, &dest, target.as_deref(), ms)
        .await
    {
        let (class, text) = sp::os_error(&e, Some(&run_dir.join(".run.json.tmp")));
        meta.insert("verdict".into(), Value::from("error"));
        meta.insert("error".into(), Value::from(sp::exc_text(class, &text)));
        let _ = sp::write_meta(&run_dir, &meta, META_REDACT);
    }
}

/// Build the adapter (Python: registry `build_adapter`; Data Designer has no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(DataDesignerAdapter::new(ctx.clone())))
}
