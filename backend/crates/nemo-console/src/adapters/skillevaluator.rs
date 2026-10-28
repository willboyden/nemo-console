//! SkillEvaluator adapter: evaluate agent skills with NVIDIA SkillEvaluator and keep the reports. OWNER: workstream C.
//! Port of `backend/src/nemo_console/adapters/skillevaluator.py` (see its module docstring for the facts it is built
//! on): the `skillevaluator-local` / `skillevaluator` CLI (Tier 1/2 inside the request, Tier 3 as ONE background run
//! through the lab wrapper), reports kept under `<state>/skillevaluator/runs/<run_id>/`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, SystemAdapter,
};
use crate::config::Settings;
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger, Endpoint,
    Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, Problem, Severity, SystemEvent,
    SystemId, SystemStatus, ValidationResult,
};
use crate::services::common::redact;
use crate::services::pyfmt::{char_prefix, str_repr};
use crate::services::pyobj::py_strip;
use crate::services::support::{
    self as sp, Done, InterruptGuard, LiveSlot, dict, fixed, full, is_str, list, num, num_value,
    pystr, truthy,
};

/// `SkillEvaluatorAdapter.label` in Python.
pub const LABEL: &str = "SkillEvaluator";

pub const LAUNCHERS: [&str; 2] = ["skillevaluator-local", "skillevaluator"];
pub const RUN_TIMEOUT: f64 = 540.0;
pub const VERSION_TTL: f64 = 300.0;
pub const KEEP_RUNS: usize = 200;
pub const EVENT_KIND: &str = "skilleval.run";
pub const PINNED_SHA: &str = "7304d76cde371287b67ea99653409d014b6b9c85";
pub const LAB_INSTALLER: &str = "clients/skillevaluator/install.sh";
pub const LAB_OLLAMA: &str = "inference/ollama";
pub const LIVE_LAUNCHER: &str = "skillevaluator-local";
pub const LIVE_AGENT: &str = "opencode";
pub const LIVE_TIMEOUT: f64 = 4800.0;
pub const LIVE_MAX_BYTES: usize = 4_000_000;
pub const LIVE_MAX_ATTEMPTS: i64 = 5;
pub const LIVE_DATASETS: [&str; 3] = ["auto", "existing", "generate"];
pub const DEFAULT_AGENT_MODEL: &str = "nemotron-lightning";
pub const DEFAULT_JUDGE_MODEL: &str = "nemotron-lightning-judge";
pub const MAX_RESULT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_FEEDBACK: usize = 8;
pub const TIER3_STATUSES: [&str; 5] = [
    "succeeded",
    "failed",
    "unknown",
    "skipped",
    "not_applicable",
];
pub const VERDICTS: [&str; 6] = [
    "passed",
    "failed",
    "incomplete",
    "error",
    "succeeded",
    "interrupted",
];
pub const TIERS: [(&str, &str); 3] = [
    (
        "static",
        "Tier 1 static checks only (offline, no model calls)",
    ),
    (
        "tier1",
        "Tier 1 with the LLM judge (rubric + LLM security review)",
    ),
    (
        "tier1+2",
        "Tier 1 with the LLM judge, then Tier 2 deduplication (needs an embeddings model)",
    ),
];
const META_REDACT: &[&str] = &["error", "tier2_error"];

/// `PINNED_INSTALL`.
#[must_use]
pub fn pinned_install() -> String {
    format!(
        "uv tool install --python 3.13 \"skillevaluator[all] @ git+https://github.com/NVIDIA/SkillEvaluator.git@{PINNED_SHA}\"   # v0.5.0"
    )
}

static NAME_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}"));
static RUN_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"\d{8}T\d{6}-\d{3}-[A-Za-z0-9._-]{1,128}"));
static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._:/@\-]{0,127}"));
static WORD_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z][a-z0-9_]{0,31}"));
static RESULT_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}"));
/// `re.search(r"version\s+(\S+)", stdout)` with Python's Unicode `\s`.
#[allow(clippy::expect_used)]
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"version[\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}]+([^\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}]+)")
        .expect("static regex")
});

fn word(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if WORD_RE.is_match(s) => Some(s.clone()),
        _ => None,
    }
}

fn model_id(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if MODEL_RE.is_match(s) => Some(s.clone()),
        _ => None,
    }
}

fn opt_str(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::String)
}

/// `summarize_tier1(d)`: the few fields the UI shows; missing fields stay None (never invented).
#[must_use]
pub fn summarize_tier1(d: &Map<String, Value>) -> Map<String, Value> {
    let q = list(d.get("quality_summary"));
    let q0 = dict(q.first());
    let rub = dict(d.get("rubric_eval"));
    let sev = dict(d.get("severity_counts"));
    let inc = d.get("incomplete_scans");
    let mut out = Map::new();
    out.insert(
        "status".into(),
        match d.get("overall_status") {
            Some(Value::String(s)) => Value::String(s.clone()),
            _ => Value::Null,
        },
    );
    out.insert("quality_score".into(), num_value(q0.get("overall_score")));
    out.insert(
        "grade".into(),
        match q0.get("grade") {
            Some(Value::String(s)) => Value::String(s.clone()),
            _ => Value::Null,
        },
    );
    out.insert(
        "rubric_score".into(),
        if is_str(rub.get("execution_status"), "succeeded") {
            num_value(rub.get("overall_score"))
        } else {
            Value::Null
        },
    );
    out.insert("errors".into(), num_value(d.get("total_errors")));
    out.insert("warnings".into(), num_value(d.get("total_warnings")));
    out.insert("high".into(), num_value(sev.get("high")));
    out.insert("critical".into(), num_value(sev.get("critical")));
    out.insert(
        "incomplete".into(),
        match inc {
            Some(Value::Array(a)) => Value::Array(
                a.iter()
                    .take(10)
                    .map(|x| Value::String(pystr(Some(x))))
                    .collect(),
            ),
            _ => json!([]),
        },
    );
    out
}

/// `failed_checks(d, limit=12)`: `[SEVERITY] validator: message` for failed findings, most severe first.
#[must_use]
pub fn failed_checks(d: &Map<String, Value>, limit: usize) -> Vec<String> {
    let order = |s: &str| match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        "info" => 4,
        _ => 5,
    };
    let mut rows: Vec<(i32, String)> = Vec::new();
    for r in list(d.get("results")) {
        let Value::Object(r) = r else { continue };
        for f in list(r.get("findings")) {
            let Value::Object(f) = f else { continue };
            let sev = match f.get("severity") {
                None => "info".to_owned(),
                v => pystr(v).to_lowercase(),
            };
            if sev == "info" || sev == "low" {
                continue;
            }
            let pick = ["message", "title", "check"]
                .iter()
                .map(|k| f.get(*k))
                .find(|v| truthy(*v))
                .flatten();
            let msg_full = pick.map_or_else(String::new, |v| pystr(Some(v)));
            let msg = char_prefix(&msg_full, 200);
            let validator = match r.get("validator") {
                None => "?".to_owned(),
                v => pystr(v),
            };
            rows.push((
                order(&sev),
                format!("[{}] {validator}: {msg}", sev.to_uppercase()),
            ));
        }
    }
    rows.sort_by_key(|x| x.0);
    rows.into_iter().take(limit).map(|(_, l)| l).collect()
}

fn feedback(items: Option<&Value>) -> Value {
    let mut out = Vec::new();
    for it in list(items).into_iter().take(MAX_FEEDBACK) {
        let it = match it {
            Value::String(s) => json!({ "message": s }),
            other => other,
        };
        let Value::Object(it) = it else { continue };
        let pick = |k: &str| {
            let v = it.get(k);
            if truthy(v) { pystr(v) } else { String::new() }
        };
        let msg = char_prefix(&redact(&pick("message")), 400).to_owned();
        let title = char_prefix(&redact(&pick("title")), 160).to_owned();
        if !msg.is_empty() || !title.is_empty() {
            out.push(json!({
                "severity": word(it.get("severity")).unwrap_or_else(|| "info".to_owned()),
                "title": title,
                "message": msg,
            }));
        }
    }
    Value::Array(out)
}

fn redacted_errors(v: Option<&Value>) -> Value {
    Value::Array(
        list(v)
            .iter()
            .take(5)
            .map(|e| Value::String(char_prefix(&redact(&pystr(Some(e))), 300).to_owned()))
            .collect(),
    )
}

/// `summarize_tier3(d)`: the fields the console keeps from a Tier 3 result.json.
#[must_use]
pub fn summarize_tier3(d: &Map<String, Value>) -> Map<String, Value> {
    let mut agents = Map::new();
    for (name, raw) in dict(d.get("agents")).iter().take(8) {
        if !NAME_RE.is_match(name) {
            continue;
        }
        let a = dict(Some(raw));
        let lift = dict(a.get("lift"));
        let overall = dict(lift.get("overall"));
        let mut metrics = Map::new();
        for (key, v) in lift.iter().take(16) {
            if key == "overall" || !WORD_RE.is_match(key) {
                continue;
            }
            let m = dict(Some(v));
            metrics.insert(
                key.clone(),
                json!({
                    "with_skill": num_value(m.get("with_skill")),
                    "without_skill": num_value(m.get("without_skill")),
                    "delta": num_value(m.get("delta")),
                }),
            );
        }
        let mut ag = Map::new();
        ag.insert("model".into(), opt_str(model_id(a.get("model"))));
        ag.insert(
            "execution_status".into(),
            opt_str(word(a.get("execution_status"))),
        );
        ag.insert("skill_lift".into(), num_value(overall.get("delta")));
        ag.insert("with_skill".into(), num_value(overall.get("with_skill")));
        ag.insert(
            "without_skill".into(),
            num_value(overall.get("without_skill")),
        );
        ag.insert(
            "scored_attempts".into(),
            num_value(a.get("scored_attempts")),
        );
        ag.insert(
            "expected_attempts".into(),
            num_value(a.get("expected_attempts")),
        );
        ag.insert("metrics".into(), Value::Object(metrics));
        ag.insert("errors".into(), redacted_errors(a.get("execution_errors")));
        agents.insert(name.clone(), Value::Object(ag));
    }
    let fb = dict(d.get("tier3_feedback"));
    let mut out = Map::new();
    out.insert(
        "execution_status".into(),
        opt_str(word(d.get("execution_status"))),
    );
    out.insert(
        "report_status".into(),
        opt_str(word(d.get("report_status"))),
    );
    out.insert(
        "scored_attempts".into(),
        num_value(d.get("scored_attempts")),
    );
    out.insert(
        "expected_attempts".into(),
        num_value(d.get("expected_attempts")),
    );
    out.insert(
        "duration_seconds".into(),
        num_value(d.get("duration_seconds")),
    );
    out.insert(
        "run_id".into(),
        match d.get("run_id") {
            Some(Value::String(s)) if RESULT_NAME_RE.is_match(s) => Value::String(s.clone()),
            _ => Value::Null,
        },
    );
    out.insert("agents".into(), Value::Object(agents));
    out.insert("conclusions".into(), feedback(fb.get("conclusions")));
    out.insert(
        "recommendations".into(),
        feedback(fb.get("recommendations")),
    );
    out.insert("errors".into(), redacted_errors(d.get("execution_errors")));
    out
}

/// `tier3_verdict(t3)`.
#[must_use]
pub fn tier3_verdict(t3: &Map<String, Value>) -> String {
    match t3.get("execution_status") {
        Some(Value::String(s)) if TIER3_STATUSES.contains(&s.as_str()) => s.clone(),
        _ => "error".to_owned(),
    }
}

/// `last_json_object(text)`: the launcher's final JSON object (printed with indent=2, so it starts on the last line
/// that begins with `{`).
#[must_use]
pub fn last_json_object(text: &str) -> Option<Map<String, Value>> {
    let start = match text.rfind("\n{") {
        Some(i) => i + 1,
        None if text.starts_with('{') => 0,
        None => return None,
    };
    match sp::json_loads_str(&text[start..])? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// `_fmt(v, signed)`.
fn fmt3(v: Option<&Value>, signed: bool) -> String {
    num(v).map_or_else(|| "-".to_owned(), |n| fixed(n, 3, signed))
}

/// The SkillEvaluator adapter (Python `SkillEvaluatorAdapter`).
pub struct SkillEvaluatorAdapter {
    inner: Arc<Inner>,
}

type VersionCache = (Option<Instant>, Option<String>, Option<String>);

struct Inner {
    ctx: AdapterContext,
    version_cache: Mutex<VersionCache>,
    follow_interval_bits: AtomicU64,
    keep_runs: AtomicUsize,
    live: LiveSlot,
}

impl SkillEvaluatorAdapter {
    /// `SkillEvaluatorAdapter(ctx)`.
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        SkillEvaluatorAdapter {
            inner: Arc::new(Inner {
                ctx,
                version_cache: Mutex::new((None, None, None)),
                follow_interval_bits: AtomicU64::new(2.0f64.to_bits()),
                keep_runs: AtomicUsize::new(KEEP_RUNS),
                live: LiveSlot::default(),
            }),
        }
    }

    /// `a.follow_interval = s` (tests use 0.01).
    pub fn set_follow_interval(&self, s: f64) {
        self.inner
            .follow_interval_bits
            .store(s.to_bits(), Ordering::Relaxed);
    }

    /// `monkeypatch.setattr(se, "KEEP_RUNS", n)` (tests).
    pub fn set_keep_runs(&self, n: usize) {
        self.inner.keep_runs.store(n, Ordering::Relaxed);
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

    /// `a.skills()`.
    #[must_use]
    pub fn skills(&self) -> Vec<String> {
        self.inner.skills()
    }

    /// `a._skill_path(name)`.
    pub fn skill_path(&self, name: &str) -> Result<PathBuf, AdapterError> {
        self.inner.skill_path(name)
    }

    /// The live run slot (tests wait for / cancel the background task through it).
    #[must_use]
    pub fn live(&self) -> &LiveSlot {
        &self.inner.live
    }

    /// The adapter context.
    #[must_use]
    pub fn ctx(&self) -> &AdapterContext {
        &self.inner.ctx
    }
}

impl Inner {
    fn skills_root(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("skills_dir")
    }

    fn runs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("skillevaluator").join("runs")
    }

    fn litellm(&self) -> String {
        sp::litellm(&self.ctx)
    }

    fn install_fix(&self) -> String {
        match sp::lab_file(&self.ctx, "skillevaluator_dir", LAB_INSTALLER) {
            Some(lab) => format!(
                "{}   # pinned SkillEvaluator {} (v0.5.0) + the skillevaluator-local wrapper",
                lab.display(),
                &PINNED_SHA[..7]
            ),
            None => pinned_install(),
        }
    }

    fn wrapper_fix(&self) -> String {
        match sp::lab_file(&self.ctx, "skillevaluator_dir", LAB_INSTALLER) {
            Some(lab) => format!(
                "install the lab wrapper: {}   # links skillevaluator-local into ~/.local/bin",
                lab.display()
            ),
            None => "install the lab wrapper: clients/skillevaluator/install.sh in the lab repository (it links skillevaluator-local onto PATH and builds the runner)".to_owned(),
        }
    }

    fn embeddings_fix(&self) -> String {
        let repo = self.ctx.paths.get("repo").map(|r| r.join(LAB_OLLAMA));
        let ollama = [self.ctx.paths.get("ollama_dir").cloned(), repo]
            .into_iter()
            .flatten()
            .find(|p| p.exists());
        match ollama {
            Some(o) => format!("cd {} && docker compose up -d", o.display()),
            None => "start the embeddings model the launcher names in SKILL_EVAL_EMBEDDING_MODEL"
                .to_owned(),
        }
    }

    fn skills(&self) -> Vec<String> {
        let Some(root) = self.skills_root() else {
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
                NAME_RE.is_match(n) && !p.is_symlink() && p.is_dir() && p.join("SKILL.md").is_file()
            })
            .take(500)
            .collect()
    }

    fn skill_path(&self, name: &str) -> Result<PathBuf, AdapterError> {
        let unknown = || AdapterError::UnknownId(name.to_owned());
        let root = self.skills_root().ok_or_else(unknown)?;
        if !NAME_RE.is_match(name) {
            return Err(unknown());
        }
        let base = sp::realpath(root);
        let p = sp::realpath(&root.join(name));
        if p.parent() != Some(base.as_path()) || !p.join("SKILL.md").is_file() {
            return Err(unknown());
        }
        Ok(p)
    }

    async fn launcher(&self) -> (Option<String>, Option<String>) {
        {
            let c = match self.version_cache.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
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
                let v = match VERSION_RE.captures(&r.stdout).and_then(|c| c.get(1)) {
                    Some(m) => Some(m.as_str().to_owned()),
                    None => {
                        let t = char_prefix(py_strip(&r.stdout), 40).to_owned();
                        (!t.is_empty()).then_some(t)
                    }
                };
                launcher = Some(name.to_owned());
                version = v;
                break;
            }
        }
        let mut c = match self.version_cache.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *c = (Some(Instant::now()), launcher.clone(), version.clone());
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

    fn event(meta: &Map<String, Value>) -> Result<SystemEvent, AdapterError> {
        let get_or = |k: &str, d: &str| match meta.get(k) {
            None => d.to_owned(),
            v => pystr(v),
        };
        let mut parts = vec![get_or("skill", "?"), get_or("tiers", "?")];
        if is_str(meta.get("tiers"), "tier3") {
            let live = dict(meta.get("live"));
            let t3 = dict(meta.get("tier3"));
            for (name, a) in &dict(t3.get("agents")) {
                let a = dict(Some(a));
                if num(a.get("skill_lift")).is_some() {
                    parts.push(format!("lift {} ({name})", fmt3(a.get("skill_lift"), true)));
                    break;
                }
            }
            if is_str(meta.get("verdict"), "running") {
                let lg = |k: &str| match live.get(k) {
                    None => "?".to_owned(),
                    v => pystr(v),
                };
                parts.push(format!(
                    "{LIVE_AGENT}={}, {} attempt(s)",
                    lg("agent_model"),
                    lg("attempts")
                ));
            }
            parts.push(get_or("verdict", "?"));
            let (sa, ea) = (
                num(t3.get("scored_attempts")),
                num(t3.get("expected_attempts")),
            );
            if let (Some(sa), Some(ea), Some(last)) = (sa, ea, parts.last_mut()) {
                last.push_str(&format!(
                    " ({}/{} attempts scored)",
                    fixed(sa, 0, false),
                    fixed(ea, 0, false)
                ));
            }
        } else {
            let s = dict(meta.get("tier1"));
            if !matches!(s.get("quality_score"), None | Some(Value::Null)) {
                let q = sp::fixed_of(s.get("quality_score"), 1)?;
                let grade = if truthy(s.get("grade")) {
                    match s.get("grade") {
                        Some(Value::String(g)) => format!(" {g}"),
                        other => {
                            return Err(AdapterError::failed(
                                "TypeError",
                                format!(
                                    "can only concatenate str (not \"{}\") to str",
                                    sp::type_name(other)
                                ),
                            ));
                        }
                    }
                } else {
                    String::new()
                };
                parts.push(format!("quality {q}{grade}"));
            }
            if !matches!(s.get("rubric_score"), None | Some(Value::Null)) {
                parts.push(format!("judge {}", sp::fixed_of(s.get("rubric_score"), 1)?));
            }
            parts.push(get_or("verdict", "?"));
            if truthy(s.get("incomplete")) {
                let joined = join_strs(s.get("incomplete"))?;
                if let Some(last) = parts.last_mut() {
                    last.push_str(&format!(" ({joined} incomplete)"));
                }
            }
        }
        let ts = num(meta.get("ts")).unwrap_or(0.0);
        let mut data = Map::new();
        for k in [
            "run_id",
            "skill",
            "tiers",
            "verdict",
            "model_calls",
            "tier1",
            "tier2_status",
            "duration_ms",
        ] {
            data.insert(k.into(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        if is_str(meta.get("tiers"), "tier3") {
            let t3 = dict(meta.get("tier3"));
            let mut summary = Map::new();
            for k in [
                "execution_status",
                "scored_attempts",
                "expected_attempts",
                "duration_seconds",
            ] {
                summary.insert(k.into(), t3.get(k).cloned().unwrap_or(Value::Null));
            }
            let mut agents = Map::new();
            for (n, a) in &dict(t3.get("agents")) {
                let a = dict(Some(a));
                let mut o = Map::new();
                for k in [
                    "model",
                    "skill_lift",
                    "with_skill",
                    "without_skill",
                    "execution_status",
                ] {
                    o.insert(k.into(), a.get(k).cloned().unwrap_or(Value::Null));
                }
                agents.insert(n.clone(), Value::Object(o));
            }
            summary.insert("agents".into(), Value::Object(agents));
            data.insert("tier3".into(), Value::Object(summary));
            data.insert("live".into(), Value::Object(dict(meta.get("live"))));
        }
        Ok(SystemEvent {
            ts,
            system: SystemId::Skillevaluator,
            kind: EVENT_KIND.to_owned(),
            summary: char_prefix(&parts.join(" · "), 300).to_owned(),
            data,
        })
    }

    fn read_run(&self, run_dir: &Path) -> Result<Option<SystemEvent>, AdapterError> {
        match self.load_meta(run_dir) {
            None => Ok(None),
            Some(m) => Inner::event(&m).map(Some),
        }
    }

    fn run_dirs(&self) -> Vec<PathBuf> {
        sp::run_dirs(&self.runs_dir(), &RUN_ID_RE, false)
    }

    fn list_runs(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let mut out = Vec::new();
        for d in self.run_dirs().into_iter().take(limit) {
            if let Some(ev) = self.read_run(&d)? {
                out.push(ev);
            }
        }
        Ok(out)
    }

    fn prune(&self) {
        let live = self.live.run_id();
        sp::prune(
            &self.run_dirs(),
            self.keep_runs.load(Ordering::Relaxed),
            live.as_deref(),
        );
    }

    fn no_embeddings_problem(&self) -> Problem {
        Problem::new(
            Severity::Warn,
            "skillevaluator.tier2.no_embeddings",
            "Tier 2 could not get embeddings: the launcher's embeddings model (nomic-embed through the router, served by Ollama in the lab) did not answer, so deduplication did not run.",
            Some(self.embeddings_fix()),
        )
    }

    fn not_installed_problem(&self) -> Problem {
        Problem::new(
            Severity::Error,
            "skillevaluator.not_installed",
            "SkillEvaluator is not on PATH.",
            Some(self.install_fix()),
        )
    }

    fn load(p: &Path) -> Option<Map<String, Value>> {
        sp::load_json_dict(p, Some(8 * 1024 * 1024))
    }

    async fn evaluate(
        &self,
        launcher: &str,
        skill: &str,
        path: &Path,
        tiers: &str,
    ) -> Result<Map<String, Value>, AdapterError> {
        let runs = self.runs_dir();
        let (run_id, run_dir) =
            sp::new_run_dir(&runs, skill).map_err(|e| sp::io_adapter_error(&e, Some(&runs)))?;
        let t0 = Instant::now();
        let path_s = path.to_string_lossy().into_owned();
        let run_dir_s = run_dir.to_string_lossy().into_owned();
        let mut argv = vec![launcher, "tier1", path_s.as_str()];
        if tiers == "static" {
            argv.push("--no-llm");
        }
        argv.extend(["-r", "json,markdown", "-o", run_dir_s.as_str()]);
        let r1 = self.run(&argv, RUN_TIMEOUT, 256_000, Some(&run_dir)).await;
        let mut meta = Map::new();
        meta.insert("run_id".into(), Value::from(run_id.as_str()));
        meta.insert("skill".into(), Value::from(skill));
        meta.insert("tiers".into(), Value::from(tiers));
        meta.insert("ts".into(), Value::from(sp::now()));
        meta.insert("model_calls".into(), Value::from(tiers != "static"));
        meta.insert("launcher".into(), Value::from(launcher));
        match Inner::load(&run_dir.join("skillevaluator-tier1.json")) {
            None => {
                meta.insert("verdict".into(), Value::from("error"));
                meta.insert("error".into(), Value::from(failure_text(&r1, 1500)));
            }
            Some(t1) => {
                let s = summarize_tier1(&t1);
                let status = s.get("status").cloned().unwrap_or(Value::Null);
                meta.insert("tier1".into(), Value::Object(s));
                meta.insert("failed_checks".into(), Value::from(failed_checks(&t1, 12)));
                meta.insert(
                    "verdict".into(),
                    if truthy(Some(&status)) {
                        status
                    } else {
                        Value::from("error")
                    },
                );
            }
        }
        if tiers == "tier1+2" {
            let r2 = self
                .run(
                    &[launcher, "tier2", &path_s, "-r", "json", "-o", &run_dir_s],
                    RUN_TIMEOUT,
                    256_000,
                    Some(&run_dir),
                )
                .await;
            let t2 = Inner::load(&run_dir.join("skillevaluator-tier2.json"));
            let st = t2
                .as_ref()
                .and_then(|t| t.get("overall_status"))
                .cloned()
                .unwrap_or(Value::Null);
            let st = if truthy(Some(&st)) {
                st
            } else {
                Value::from("error")
            };
            meta.insert("tier2_status".into(), st.clone());
            if is_str(Some(&st), "error") {
                meta.insert("tier2_error".into(), Value::from(failure_text(&r2, 800)));
            }
            let inc: Vec<String> = list(t2.as_ref().and_then(|t| t.get("incomplete_scans")))
                .iter()
                .map(|x| pystr(Some(x)))
                .collect();
            let t2err = match meta.get("tier2_error") {
                None => String::new(),
                v => pystr(v),
            };
            if inc.iter().any(|x| x == "embedding-provider")
                || t2err.to_lowercase().contains("embedding")
            {
                meta.insert("tier2_no_embeddings".into(), Value::Bool(true));
            }
            if !is_str(Some(&st), "passed") && is_str(meta.get("verdict"), "passed") {
                let v = if is_str(Some(&st), "failed") {
                    Value::from("failed")
                } else {
                    st
                };
                meta.insert("verdict".into(), v);
            }
        }
        meta.insert("duration_ms".into(), Value::from(sp::ms_since(t0)));
        sp::write_meta(&run_dir, &meta, META_REDACT)
            .map_err(|e| sp::io_adapter_error(&e, Some(&run_dir.join(".run.json.tmp"))))?;
        self.prune();
        Ok(meta)
    }

    fn find_result(run_dir: &Path) -> Option<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        let sorted = |d: &Path| -> Option<Vec<PathBuf>> {
            let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(d)
                .ok()?
                .flatten()
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            Some(v.into_iter().map(|(_, p)| p).take(50).collect())
        };
        let name_of = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        'outer: {
            let Some(skill_dirs) = sorted(run_dir) else {
                break 'outer;
            };
            for sd in skill_dirs {
                if sd.is_symlink() || !sd.is_dir() || !RESULT_NAME_RE.is_match(&name_of(&sd)) {
                    continue;
                }
                let Some(rds) = sorted(&sd) else {
                    break 'outer;
                };
                for rd in rds {
                    let rn = name_of(&rd);
                    let p = rd.join("result.json");
                    if rn != "latest"
                        && RESULT_NAME_RE.is_match(&rn)
                        && !rd.is_symlink()
                        && rd.is_dir()
                        && !p.is_symlink()
                        && p.is_file()
                    {
                        found.push(p);
                    }
                }
            }
        }
        let mut best: Option<(String, PathBuf)> = None;
        for p in found {
            let key = p.parent().map(name_of).unwrap_or_default();
            if best.as_ref().is_none_or(|(b, _)| key > *b) {
                best = Some((key, p));
            }
        }
        best.map(|(_, p)| p)
    }

    /// `_load_result(p)`: no-follow open, regular file, size-capped, a JSON object or nothing.
    fn load_result(p: &Path) -> Option<Map<String, Value>> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
            .open(p)
            .ok()?;
        let md = f.metadata().ok()?;
        if !md.file_type().is_file() || md.len() > MAX_RESULT_BYTES {
            return None;
        }
        let mut buf = Vec::new();
        f.take(MAX_RESULT_BYTES + 1).read_to_end(&mut buf).ok()?;
        match sp::json_loads_bytes(&buf)? {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    fn finish_live(
        &self,
        run_dir: &Path,
        meta: &mut Map<String, Value>,
        r: &CommandResult,
        ms: i64,
    ) -> std::io::Result<()> {
        meta.insert("duration_ms".into(), Value::from(ms));
        let out = last_json_object(py_strip(&r.stdout)).unwrap_or_default();
        if sp::is_int(out.get("container_exit"))
            && let Some(v) = out.get("container_exit")
        {
            meta.insert("container_exit".into(), v.clone());
        }
        if let Some(Value::String(j)) = out.get("job")
            && RESULT_NAME_RE.is_match(j)
        {
            meta.insert("job".into(), Value::from(j.as_str()));
        }
        let result_path = Inner::find_result(run_dir);
        let result = result_path.as_deref().and_then(Inner::load_result);
        match (result, result_path) {
            (Some(result), Some(rp)) => {
                let mut t3 = summarize_tier3(&result);
                let conclusions = t3.shift_remove("conclusions").unwrap_or(Value::Null);
                let recommendations = t3.shift_remove("recommendations").unwrap_or(Value::Null);
                meta.insert("conclusions".into(), conclusions);
                meta.insert("recommendations".into(), recommendations);
                let verdict = tier3_verdict(&t3);
                meta.insert("tier3".into(), Value::Object(t3));
                meta.insert("verdict".into(), Value::from(verdict));
                let rel = rp
                    .strip_prefix(run_dir)
                    .map(|r| r.to_string_lossy().into_owned())
                    .unwrap_or_default();
                meta.insert("result".into(), Value::from(rel));
            }
            _ => {
                let t3_summary = dict(dict(dict(out.get("summary")).get("tiers")).get("3"));
                let why = match t3_summary.get("error") {
                    Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                    _ => None,
                };
                meta.insert("verdict".into(), Value::from("error"));
                let err = why.unwrap_or_else(|| failure_text(r, 1500));
                meta.insert("error".into(), Value::from(err));
            }
        }
        sp::write_meta(run_dir, meta, META_REDACT)?;
        self.prune();
        Ok(())
    }

    // ── reports ─────────────────────────────────────────────────────────────────────────────

    fn report_text(&self, run_id: &str) -> Result<(bool, String), AdapterError> {
        let run_dir = if run_id.is_empty() {
            let runs = self.list_runs(1)?;
            let Some(first) = runs.first() else {
                return Ok((false, "no stored runs yet".to_owned()));
            };
            self.runs_dir().join(pystr(first.data.get("run_id")))
        } else {
            if !RUN_ID_RE.is_match(run_id) {
                return Ok((false, "unknown run id".to_owned()));
            }
            self.runs_dir().join(run_id)
        };
        let Some(meta) = self.load_meta(&run_dir) else {
            return Ok((false, "unknown run id".to_owned()));
        };
        if is_str(meta.get("tiers"), "tier3") {
            return Ok((true, self.tier3_report(&run_dir, meta)?.join("\n")));
        }
        let mut head = vec![Inner::event(&meta)?.summary, String::new()];
        if is_str(meta.get("verdict"), "error") {
            head.push("SkillEvaluator did not produce a report:".to_owned());
            head.push(match meta.get("error") {
                None => String::new(),
                v => pystr(v),
            });
            head.push(String::new());
        }
        let fc = list(meta.get("failed_checks"));
        if !fc.is_empty() {
            head.push("Top findings (medium and above):".to_owned());
            head.extend(fc.iter().map(|x| format!("  {}", pystr(Some(x)))));
            head.push(String::new());
        }
        if truthy(meta.get("tier2_status")) {
            head.push(format!(
                "Tier 2 (deduplication): {}",
                pystr(meta.get("tier2_status"))
            ));
            head.push(String::new());
        }
        if truthy(meta.get("tier2_no_embeddings")) {
            let p = self.no_embeddings_problem();
            head.push(p.message.clone());
            head.push(format!("Fix: {}", p.fix.unwrap_or_default()));
            head.push(String::new());
        }
        let md = run_dir.join("skillevaluator-tier1.md");
        let body = match std::fs::metadata(&md) {
            Err(_) => String::new(),
            Ok(m) if m.len() > 4 * 1024 * 1024 => "(report too large to show)".to_owned(),
            Ok(_) => match sp::read_text(&md) {
                Ok(t) => char_prefix(&t, 16_000).to_owned(),
                Err(sp::ReadErr::Os(_)) => String::new(),
                Err(e) => return Err(e.to_adapter(&md)),
            },
        };
        let mut text = head.join("\n");
        if !body.is_empty() {
            text.push('\n');
            text.push_str(&body);
        }
        Ok((true, text))
    }

    #[allow(clippy::too_many_lines)]
    fn tier3_report(
        &self,
        run_dir: &Path,
        mut meta: Map<String, Value>,
    ) -> Result<Vec<String>, AdapterError> {
        let live = dict(meta.get("live"));
        let verdict = match meta.get("verdict") {
            None => "?".to_owned(),
            v => pystr(v),
        };
        let lg = |k: &str, d: &str| match live.get(k) {
            None => d.to_owned(),
            v => pystr(v),
        };
        let mut lines = vec![
            Inner::event(&meta)?.summary,
            String::new(),
            format!("Status: {verdict}"),
            format!(
                "Agent: {} on {}; judge {}; {} attempt(s) per case; dataset {}; baseline {}",
                lg("agent", LIVE_AGENT),
                lg("agent_model", "?"),
                lg("judge_model", "?"),
                lg("attempts", "?"),
                lg("dataset", "?"),
                if truthy(live.get("skip_baseline")) {
                    "skipped"
                } else {
                    "on"
                }
            ),
        ];
        let mut t3 = dict(meta.get("tier3"));
        if verdict == "running" {
            let ts = num(meta.get("ts"))
                .filter(|t| *t != 0.0)
                .unwrap_or_else(sp::now);
            let mins = ((sp::now() - ts) / 60.0).max(0.0);
            lines.push(format!(
                "Running for {} min. This report fills in when the run ends (a 2-case skill takes about 30 s; dataset generation and several attempts take much longer).",
                fixed(mins, 0, false)
            ));
            return Ok(lines);
        }
        if verdict == "interrupted" {
            let why = if is_str(meta.get("interrupted_by"), "console stop") {
                "the console stopped while it ran (the launcher was interrupted and stops its job container)"
            } else {
                "the console restarted while it ran, so nothing recorded its end"
            };
            lines.push(String::new());
            lines.push(format!("Interrupted: {why}."));
            let late = Inner::find_result(run_dir);
            let Some(res) = late.as_deref().and_then(Inner::load_result) else {
                return Ok(lines);
            };
            t3 = summarize_tier3(&res);
            let c = t3.shift_remove("conclusions").unwrap_or(Value::Null);
            let r = t3.shift_remove("recommendations").unwrap_or(Value::Null);
            meta.insert("conclusions".into(), c);
            meta.insert("recommendations".into(), r);
            let es = t3.get("execution_status");
            lines.push(format!(
                "A result.json landed after that (the run finished on its own): execution_status {}.",
                if truthy(es) { pystr(es) } else { "?".to_owned() }
            ));
        }
        if verdict == "error" {
            lines.push(String::new());
            lines.push("No Tier 3 result was written:".to_owned());
            let e = match meta.get("error") {
                None => String::new(),
                v => pystr(v),
            };
            lines.push(py_strip(&e).to_owned());
            if !matches!(meta.get("container_exit"), None | Some(Value::Null)) {
                lines.push(format!(
                    "(runner container exit code {})",
                    pystr(meta.get("container_exit"))
                ));
            }
            return Ok(lines);
        }
        let (sa, ea, dur) = (
            num(t3.get("scored_attempts")),
            num(t3.get("expected_attempts")),
            num(t3.get("duration_seconds")),
        );
        if let (Some(sa), Some(ea)) = (sa, ea) {
            let mut l = format!(
                "Attempts scored: {} of {}",
                fixed(sa, 0, false),
                fixed(ea, 0, false)
            );
            if let Some(d) = dur.filter(|d| *d != 0.0) {
                l.push_str(&format!("; evaluation took {} s", fixed(d, 1, false)));
            }
            lines.push(l);
        }
        let agents = dict(t3.get("agents"));
        if !agents.is_empty() {
            lines.push(String::new());
            lines.push(
                "Skill Lift = overall score with the skill minus without it (per agent):"
                    .to_owned(),
            );
            lines.push(format!(
                "  {:<12} {:<34} {:>7} {:>7} {:>8}  status",
                "agent", "model", "lift", "with", "without"
            ));
            for (name, a) in &agents {
                let a = dict(Some(a));
                let model = if truthy(a.get("model")) {
                    pystr(a.get("model"))
                } else {
                    "-".to_owned()
                };
                let status = if truthy(a.get("execution_status")) {
                    pystr(a.get("execution_status"))
                } else {
                    "-".to_owned()
                };
                lines.push(format!(
                    "  {:<12} {:<34} {:>7} {:>7} {:>8}  {status}",
                    char_prefix(name, 12),
                    char_prefix(&model, 34),
                    fmt3(a.get("skill_lift"), true),
                    fmt3(a.get("with_skill"), false),
                    fmt3(a.get("without_skill"), false),
                ));
                let metrics = dict(a.get("metrics"));
                if !metrics.is_empty() {
                    lines.push(format!(
                        "    {:<20} {:>7} {:>8} {:>7}",
                        "metric", "with", "without", "delta"
                    ));
                    for (mk, mv) in &metrics {
                        let mv = dict(Some(mv));
                        lines.push(format!(
                            "    {:<20} {:>7} {:>8} {:>7}",
                            char_prefix(mk, 20),
                            fmt3(mv.get("with_skill"), false),
                            fmt3(mv.get("without_skill"), false),
                            fmt3(mv.get("delta"), true),
                        ));
                    }
                }
                for err in list(a.get("errors")) {
                    lines.push(format!("    error: {}", pystr(Some(&err))));
                }
            }
        }
        for (title, key) in [
            ("Conclusions", "conclusions"),
            ("Recommendations", "recommendations"),
        ] {
            let items = list(meta.get(key));
            if !items.is_empty() {
                lines.push(String::new());
                lines.push(format!("{title}:"));
                for it in items {
                    let it = dict(Some(&it));
                    let head = if truthy(it.get("title")) && it.get("title") != it.get("message") {
                        format!("{}: ", pystr(it.get("title")))
                    } else {
                        String::new()
                    };
                    let sev = match it.get("severity") {
                        None => "info".to_owned(),
                        v => pystr(v),
                    };
                    let msg = match it.get("message") {
                        None => String::new(),
                        v => pystr(v),
                    };
                    lines.push(format!("  [{sev}] {head}{msg}"));
                }
            }
        }
        if let Some(Value::String(rel)) = meta.get("result")
            && !rel.starts_with('/')
            && !rel.contains("..")
        {
            let html = sp::pure_join(run_dir, &sp::pure_parent(rel)).join("report.html");
            if html.is_file() && !html.is_symlink() {
                lines.push(String::new());
                lines.push(format!(
                    "Full report (HTML, open it locally): {}",
                    html.display()
                ));
            }
        }
        Ok(lines)
    }
}

/// `", ".join(xs)` of a JSON list (TypeError text for a non-str item, like Python).
fn join_strs(v: Option<&Value>) -> Result<String, AdapterError> {
    let mut out = Vec::new();
    match v {
        Some(Value::Array(items)) => {
            for (i, x) in items.iter().enumerate() {
                match x {
                    Value::String(s) => out.push(s.clone()),
                    other => {
                        return Err(AdapterError::failed(
                            "TypeError",
                            format!(
                                "sequence item {i}: expected str instance, {} found",
                                sp::type_name(Some(other))
                            ),
                        ));
                    }
                }
            }
        }
        Some(Value::String(s)) => out.extend(s.chars().map(String::from)),
        Some(Value::Object(m)) => out.extend(m.keys().cloned()),
        _ => {
            return Err(AdapterError::failed(
                "TypeError",
                "can only join an iterable",
            ));
        }
    }
    Ok(out.join(", "))
}

/// `("timed out" if r.timed_out else (r.stderr or r.stdout).strip()[-n:]) or f"exit {rc}"`.
fn failure_text(r: &CommandResult, n: usize) -> String {
    let t = if r.timed_out {
        "timed out".to_owned()
    } else {
        let src = if r.stderr.is_empty() {
            &r.stdout
        } else {
            &r.stderr
        };
        sp::strip_tail(src, n)
    };
    if t.is_empty() {
        format!("exit {}", r.returncode)
    } else {
        t
    }
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

#[async_trait]
impl SystemAdapter for SkillEvaluatorAdapter {
    fn id(&self) -> SystemId {
        SystemId::Skillevaluator
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let me = &self.inner;
        let ((launcher, version), (router_ok, router_detail)) =
            tokio::join!(me.launcher(), sp::router_up(&me.ctx));
        let skills = me.skills();
        let last = me.list_runs(1)?;
        let litellm = me.litellm();
        let mut problems = Vec::new();
        let detail = match &launcher {
            Some(l) => {
                let base = format!("{l} {}", version.clone().unwrap_or_default());
                Some(format!(
                    "{}{}",
                    py_strip(&base),
                    if l == "skillevaluator-local" {
                        " (wrapper: provider settings and its own key file)"
                    } else {
                        " (provider from SKILL_EVAL_* in the console's environment)"
                    }
                ))
            }
            None => Some("neither skillevaluator-local nor skillevaluator is on PATH".to_owned()),
        };
        let mut components = vec![
            Component {
                kind: "process".into(),
                name: launcher
                    .clone()
                    .unwrap_or_else(|| "skillevaluator".to_owned()),
                state: if launcher.is_some() {
                    "running"
                } else {
                    "absent"
                }
                .into(),
                image: None,
                detail,
            },
            Component {
                kind: "http".into(),
                name: format!("router {litellm}"),
                state: if router_ok { "running" } else { "stopped" }.into(),
                image: None,
                detail: Some(format!(
                    "{router_detail} on GET /health/liveliness (LLM judge and embeddings go through it)"
                )),
            },
        ];
        let live = me.live.info();
        if let Some((rid, skill, started)) = &live {
            let mins = ((sp::now() - started) / 60.0).max(0.0);
            components.push(Component {
                kind: "process".into(),
                name: format!("tier3 {skill}"),
                state: "running".into(),
                image: None,
                detail: Some(format!(
                    "live evaluation {rid}, running for {} min (one at a time; it shows in the runs list)",
                    fixed(mins, 0, false)
                )),
            });
        }
        match launcher.as_deref() {
            None => problems.push(Problem::new(
                Severity::Error,
                "skillevaluator.not_installed",
                "SkillEvaluator is not installed (or not on the console's PATH).",
                Some(me.install_fix()),
            )),
            Some("skillevaluator") => problems.push(Problem::new(
                Severity::Info,
                "skillevaluator.no_wrapper",
                "Running plain `skillevaluator`: LLM checks need SKILL_EVAL_* (provider, base URL, key) in the console's environment; without them Tier 1 runs static checks only and Tier 2 fails. Tier 3 (live agent runs) is refused: it needs the skillevaluator-local wrapper's sandboxed runner.",
                Some("Put a `skillevaluator-local` wrapper on PATH, or set SKILL_EVAL_* in the console's EnvironmentFile".to_owned()),
            )),
            Some(_) => {}
        }
        if !router_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "skillevaluator.router_down",
                format!(
                    "The router ({litellm}) is not answering; only the 'static' tier will work."
                ),
                Some(format!(
                    "curl -s -o /dev/null -w '%{{http_code}}\\n' {litellm}/health/liveliness"
                )),
            ));
        }
        if me.skills_root().is_none() || skills.is_empty() {
            let root = me
                .skills_root()
                .map_or_else(|| "None".to_owned(), |p| p.display().to_string());
            problems.push(Problem::new(
                Severity::Warn,
                "skillevaluator.no_skills",
                format!(
                    "No skills found under {root} (one directory per skill, each with a SKILL.md)."
                ),
                Some("export NEMO_CONSOLE_REPO_ROOT=/path/to/repo-with-.claude/skills and restart nemo-console".to_owned()),
            ));
        }
        let health = match (&launcher, router_ok) {
            (None, _) => Health::Down,
            (_, false) => Health::Degraded,
            _ => Health::Up,
        };
        let mut summary = format!("{} skills", skills.len());
        if let Some((_, skill, _)) = &live {
            summary.push_str(&format!("; live: {skill} running"));
        }
        if let Some(l) = last.first() {
            summary.push_str(&format!("; last run: {}", l.summary));
        }
        Ok(SystemStatus {
            system: SystemId::Skillevaluator,
            health,
            version,
            summary: char_prefix(&summary, 300).to_owned(),
            endpoints: vec![Endpoint {
                name: "router (judge + embeddings)".into(),
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
        let skills = me.skills();
        let runs = me.list_runs(me.keep_runs.load(Ordering::Relaxed))?;
        let pt = |k: &str, v: f64| {
            let mut p = MetricPoint::new(k, v);
            "1".clone_into(&mut p.unit);
            p
        };
        #[allow(clippy::cast_precision_loss)]
        let mut points = vec![
            pt("se.skills", skills.len() as f64),
            pt("se.runs", runs.len() as f64),
        ];
        if !runs.is_empty() {
            points.push(pt(
                "se.live_running",
                if me.live.run_id().is_some() { 1.0 } else { 0.0 },
            ));
            let static_run = runs.iter().find(|r| !is_str(r.data.get("tiers"), "tier3"));
            let t1 = static_run
                .and_then(|r| r.data.get("tier1"))
                .filter(|v| truthy(Some(v)))
                .cloned();
            if let Some(Value::Object(t1)) = t1 {
                for (key, field) in [
                    ("se.last_quality_score", "quality_score"),
                    ("se.last_judge_score", "rubric_score"),
                ] {
                    if let Some(v) = num(t1.get(field)) {
                        points.push(pt(key, v));
                    }
                }
            }
            for r in &runs {
                let lift = dict(dict(r.data.get("tier3")).get("agents"))
                    .values()
                    .find_map(|a| num(dict(Some(a)).get("skill_lift")));
                if let Some(l) = lift {
                    points.push(pt("se.last_skill_lift", l));
                    break;
                }
            }
            for verdict in VERDICTS {
                let n = runs
                    .iter()
                    .filter(|r| is_str(r.data.get("verdict"), verdict))
                    .count();
                #[allow(clippy::cast_precision_loss)]
                points.push(pt(
                    &format!("se.runs_by_verdict{{verdict={verdict}}}"),
                    n as f64,
                ));
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Skillevaluator,
            ts: now,
            available: true,
            points,
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let me = &self.inner;
        me.list_runs(limit.min(me.keep_runs.load(Ordering::Relaxed)))
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        let me = Arc::clone(&self.inner);
        Box::pin(async_stream::stream! {
            let mut seen = std::collections::HashSet::new();
            let mut first = true;
            loop {
                let mut events = match me.list_runs(50) {
                    Ok(e) => e,
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                };
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
        Ok(Vec::new())
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    async fn validate_config(
        &self,
        file_id: &str,
        _content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    #[allow(clippy::too_many_lines)]
    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let names = self.inner.skills();
        let skill_param = || {
            let mut p = param(
                "skill",
                "Skill",
                ParamType::Enum,
                "A directory under the skills dir that contains SKILL.md.",
            );
            p.choices = (!names.is_empty()).then(|| names.clone());
            p.default = names
                .first()
                .map_or(Value::Null, |n| Value::from(n.as_str()));
            p
        };
        let mut tiers = param(
            "tiers",
            "Tiers",
            ParamType::Enum,
            &TIERS
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join("; "),
        );
        tiers.choices = Some(TIERS.iter().map(|(k, _)| (*k).to_owned()).collect());
        tiers.default = Value::from("tier1");
        let mut evaluate = ActionSpec::new(
            "evaluate",
            "Evaluate a skill",
            "Runs SkillEvaluator on one skill and keeps the JSON + Markdown reports in the console's state dir (newest 200 runs). 'static' makes no model calls; 'tier1' adds the LLM judge through the router; 'tier1+2' also runs embedding deduplication. Tier 3 (live agent runs) is the separate 'Tier 3: live agent evaluation' action.",
            Danger::Safe,
        );
        evaluate.params = vec![skill_param(), tiers];
        let mut attempts = param(
            "attempts",
            "Attempts per case",
            ParamType::Int,
            "Runs per eval case and arm. The lift varies between runs; use 3 or more before trusting it.",
        );
        attempts.flag = Some("--n-attempts".into());
        attempts.default = Value::from(1);
        attempts.min = Some(1.0);
        #[allow(clippy::cast_precision_loss)]
        {
            attempts.max = Some(LIVE_MAX_ATTEMPTS as f64);
        }
        let mut agent_model = param(
            "agent_model",
            "Agent model (router alias)",
            ParamType::String,
            &format!(
                "The model opencode runs on (passed as {LIVE_AGENT}=ALIAS). It must handle tool results on the Responses API."
            ),
        );
        agent_model.flag = Some("--agent-model".into());
        agent_model.default = Value::from(DEFAULT_AGENT_MODEL);
        let mut judge_model = param(
            "judge_model",
            "Judge model (router alias)",
            ParamType::String,
            "Scores both arms; a model with thinking off so its JSON parses.",
        );
        judge_model.flag = Some("--judge-model".into());
        judge_model.default = Value::from(DEFAULT_JUDGE_MODEL);
        let mut dataset = param(
            "dataset",
            "Eval dataset",
            ParamType::Enum,
            "auto: the skill's evals/evals.json, else one starter case; existing: fail without evals.json; generate: write up to four cases with the chat model first (slower).",
        );
        dataset.flag = Some("--dataset".into());
        dataset.choices = Some(LIVE_DATASETS.iter().map(|s| (*s).to_owned()).collect());
        dataset.default = Value::from("auto");
        let mut skip = param(
            "skip_baseline",
            "Skip the without-skill arm",
            ParamType::Bool,
            "Half the agent runs, but no Skill Lift (nothing to compare against).",
        );
        skip.flag = Some("--skip-baseline".into());
        skip.default = Value::Bool(false);
        let mut live = ActionSpec::new(
            "evaluate_live",
            "Tier 3: live agent evaluation",
            "Runs an agent (opencode) on the skill's eval cases with and without the skill, in a sandboxed runner container started by the skillevaluator-local wrapper (it reaches the router and the public internet only); a judge model scores both arms and the difference is the Skill Lift. Spends router tokens and takes minutes (up to about an hour with dataset generation and several attempts). It starts in the background: the run shows as running in the runs list and fills in when it ends; open it with 'Show a stored report'. One live run at a time.",
            Danger::Disruptive,
        );
        live.params = vec![
            skill_param(),
            attempts,
            agent_model,
            judge_model,
            dataset,
            skip,
        ];
        let all = ActionSpec::new(
            "evaluate_all_static",
            "Static check of every skill",
            "Tier 1 static checks (no model calls) on every skill, one run each; prints a score table. Takes about 2 s per skill.",
            Danger::Safe,
        );
        let mut report = ActionSpec::new(
            "report",
            "Show a stored report",
            "The report of a stored run (newest first): Tier 1/2 findings, or a Tier 3 run's status, Skill Lift per agent and its conclusions.",
            Danger::Safe,
        );
        report.params = vec![param(
            "run_id",
            "Run",
            ParamType::String,
            "A run id from the runs list; empty = the newest run.",
        )];
        let doctor = ActionSpec::new(
            "doctor",
            "Doctor",
            "skillevaluator doctor: which provider and Harbor agents the launcher resolves.",
            Danger::Safe,
        );
        Ok(vec![evaluate, live, all, report, doctor])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let done = Done::start();
        let me = &self.inner;
        if ![
            "evaluate",
            "evaluate_live",
            "evaluate_all_static",
            "report",
            "doctor",
        ]
        .contains(&action_id)
        {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        if action_id == "report" {
            let rid = param_str(params.get("run_id"));
            let (ok, text) = me.report_text(&rid)?;
            return Ok(done.call(ok, &text, vec![]));
        }
        if action_id == "evaluate_live" {
            return self.start_live(params, done).await;
        }
        let (launcher, _) = me.launcher().await;
        let Some(launcher) = launcher else {
            return Ok(done.call(
                false,
                "SkillEvaluator is not installed.",
                vec![me.not_installed_problem()],
            ));
        };
        if action_id == "doctor" {
            let r = me.run(&[&launcher, "doctor"], 60.0, 256_000, None).await;
            let mut out = r.stdout.clone();
            if !r.stderr.is_empty() {
                out.push('\n');
                out.push_str(&r.stderr);
            }
            return Ok(done.call(r.returncode == 0, py_strip(&out), vec![]));
        }
        if action_id == "evaluate" {
            let skill = param_str(params.get("skill"));
            let tiers = match params.get("tiers") {
                v if truthy(v) => pystr(v),
                _ => "tier1".to_owned(),
            };
            if !TIERS.iter().any(|(k, _)| *k == tiers) {
                return Ok(done.call(
                    false,
                    &format!("unknown tiers {}", str_repr(&tiers)),
                    vec![],
                ));
            }
            let Ok(path) = me.skill_path(&skill) else {
                return Ok(done.call(
                    false,
                    &format!("unknown skill {}", str_repr(&skill)),
                    vec![],
                ));
            };
            let meta = me.evaluate(&launcher, &skill, &path, &tiers).await?;
            let (_, text) = me.report_text(&pystr(meta.get("run_id")))?;
            let problems = if truthy(meta.get("tier2_no_embeddings")) {
                vec![me.no_embeddings_problem()]
            } else {
                vec![]
            };
            let ok = ["passed", "incomplete", "failed"]
                .iter()
                .any(|v| is_str(meta.get("verdict"), v));
            return Ok(done.call(ok, &text, problems));
        }
        // evaluate_all_static
        let mut rows = vec![
            "skill                                     verdict     quality  grade  errors  high"
                .to_owned(),
        ];
        let mut ok = true;
        for name in me.skills() {
            let Ok(path) = me.skill_path(&name) else {
                continue;
            };
            let meta = me.evaluate(&launcher, &name, &path, "static").await?;
            let s = dict(meta.get("tier1"));
            ok = ok && !is_str(meta.get("verdict"), "error");
            let dash = |v: Option<&Value>| {
                v.filter(|x| !x.is_null())
                    .map_or_else(|| "-".to_owned(), |x| pystr(Some(x)))
            };
            let grade = if truthy(s.get("grade")) {
                pystr(s.get("grade"))
            } else {
                "-".to_owned()
            };
            rows.push(format!(
                "{:<41} {:<11} {:>7}  {:<5}  {:>6}  {:>4}",
                char_prefix(&name, 41),
                pystr(meta.get("verdict")),
                dash(s.get("quality_score")),
                grade,
                dash(s.get("errors")),
                dash(s.get("high")),
            ));
        }
        Ok(done.call(ok, &rows.join("\n"), vec![]))
    }
}

impl SkillEvaluatorAdapter {
    fn busy_result(&self, done: Done) -> Option<ActionResult> {
        let busy = self.inner.live.run_id()?;
        Some(done.call(
            false,
            &format!(
                "A live evaluation is already running ({busy}); one runs at a time. Wait for it to finish: it shows in the runs list."
            ),
            vec![Problem::new(
                Severity::Warn,
                "skillevaluator.live.busy",
                format!("live evaluation {busy} is still running"),
                Some(format!(
                    "'Show a stored report' with run id {busy} shows its progress"
                )),
            )],
        ))
    }

    #[allow(clippy::too_many_lines)]
    async fn start_live(
        &self,
        params: &Map<String, Value>,
        done: Done,
    ) -> Result<ActionResult, AdapterError> {
        let me = &self.inner;
        if let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        let skill = param_str(params.get("skill"));
        let Ok(path) = me.skill_path(&skill) else {
            return Ok(done.call(
                false,
                &format!("unknown skill {}", str_repr(&skill)),
                vec![],
            ));
        };
        let attempts = match params.get("attempts") {
            None => Some(1),
            Some(v) if sp::is_int(Some(v)) => {
                v.as_i64().filter(|n| (1..=LIVE_MAX_ATTEMPTS).contains(n))
            }
            Some(_) => None,
        };
        let Some(attempts) = attempts else {
            return Ok(done.call(
                false,
                &format!("attempts must be an integer from 1 to {LIVE_MAX_ATTEMPTS}"),
                vec![],
            ));
        };
        let mut models: Vec<String> = Vec::new();
        for (key, default) in [
            ("agent_model", DEFAULT_AGENT_MODEL),
            ("judge_model", DEFAULT_JUDGE_MODEL),
        ] {
            let value = match params.get(key) {
                None | Some(Value::Null) => Value::from(default),
                Some(Value::String(s)) if s.is_empty() => Value::from(default),
                Some(v) => v.clone(),
            };
            match value {
                Value::String(s) if MODEL_RE.is_match(&s) => models.push(s),
                _ => {
                    return Ok(done.call(
                        false,
                        &format!("{key} must be a router model alias (letters, digits and ._:/@-)"),
                        vec![],
                    ));
                }
            }
        }
        let dataset = match params.get("dataset") {
            v if truthy(v) => v.cloned().unwrap_or(Value::Null),
            _ => Value::from("auto"),
        };
        if !LIVE_DATASETS.iter().any(|d| is_str(Some(&dataset), d)) {
            return Ok(done.call(
                false,
                &format!("dataset must be one of {}", LIVE_DATASETS.join(", ")),
                vec![],
            ));
        }
        let skip_baseline = match params.get("skip_baseline") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                return Ok(done.call(false, "skip_baseline must be true or false", vec![]));
            }
        };
        let (launcher, _) = me.launcher().await;
        let Some(launcher) = launcher else {
            return Ok(done.call(
                false,
                "SkillEvaluator is not installed.",
                vec![me.not_installed_problem()],
            ));
        };
        if launcher != LIVE_LAUNCHER {
            return Ok(done.call(
                false,
                "Refused: Tier 3 runs an agent that executes the skill's own scripts, so it only runs through the skillevaluator-local wrapper, which contains each job in a sandboxed runner container. Plain `skillevaluator` would run it with SkillEvaluator's stock sandboxes (open network).",
                vec![Problem::new(
                    Severity::Error,
                    "skillevaluator.live.needs_wrapper",
                    "Tier 3 needs the skillevaluator-local wrapper on the console's PATH.",
                    Some(me.wrapper_fix()),
                )],
            ));
        }
        if let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        let runs = me.runs_dir();
        let (run_id, run_dir) =
            sp::new_run_dir(&runs, &skill).map_err(|e| sp::io_adapter_error(&e, Some(&runs)))?;
        let dataset_s = pystr(Some(&dataset));
        let mut argv: Vec<String> = vec![
            launcher.clone(),
            "tier3".into(),
            path.to_string_lossy().into_owned(),
            "--n-attempts".into(),
            attempts.to_string(),
            "--agent-model".into(),
            format!("{LIVE_AGENT}={}", models[0]),
            "--judge-model".into(),
            models[1].clone(),
            "--dataset".into(),
            dataset_s.clone(),
        ];
        if skip_baseline {
            argv.push("--skip-baseline".into());
        }
        argv.extend([
            "--results-dir".into(),
            run_dir.to_string_lossy().into_owned(),
            "--quiet".into(),
        ]);
        let started = sp::now();
        let mut meta = Map::new();
        meta.insert("run_id".into(), Value::from(run_id.as_str()));
        meta.insert("skill".into(), Value::from(skill.as_str()));
        meta.insert("tiers".into(), Value::from("tier3"));
        meta.insert("ts".into(), Value::from(started));
        meta.insert("verdict".into(), Value::from("running"));
        meta.insert("model_calls".into(), Value::Bool(true));
        meta.insert("launcher".into(), Value::from(launcher.as_str()));
        meta.insert(
            "live".into(),
            json!({
                "agent": LIVE_AGENT,
                "agent_model": models[0],
                "judge_model": models[1],
                "attempts": attempts,
                "dataset": dataset,
                "skip_baseline": skip_baseline,
            }),
        );
        sp::write_meta(&run_dir, &meta, META_REDACT)
            .map_err(|e| sp::io_adapter_error(&e, Some(&run_dir.join(".run.json.tmp"))))?;
        let task_inner = Arc::clone(me);
        let started_ok = me.live.start(&run_id, &skill, started, async move {
            live_task(task_inner, run_dir, argv, meta).await;
        });
        if !started_ok && let Some(b) = self.busy_result(done) {
            return Ok(b);
        }
        Ok(done.call(
            true,
            &format!(
                "Started live evaluation {run_id} ({skill}; {LIVE_AGENT}={}, judge {}, {attempts} attempt(s), dataset {dataset_s}{}). It runs in the background: watch the runs list, then 'Show a stored report' with this run id.",
                models[0],
                models[1],
                if skip_baseline { ", no baseline" } else { "" }
            ),
            vec![],
        ))
    }
}

/// `_live_task`: run the launcher, then record the outcome; dropped early = interrupted by console stop.
async fn live_task(me: Arc<Inner>, run_dir: PathBuf, argv: Vec<String>, meta: Map<String, Value>) {
    let t0 = Instant::now();
    let shared = Arc::new(Mutex::new(meta));
    let mut guard = InterruptGuard {
        run_dir: run_dir.clone(),
        meta: Arc::clone(&shared),
        t0,
        armed: true,
        redact_keys: META_REDACT,
    };
    let r = sp::run_cmd(&me.ctx, &argv, LIVE_TIMEOUT, LIVE_MAX_BYTES, Some(&run_dir)).await;
    guard.armed = false;
    let mut meta = match shared.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    };
    if let Err(e) = me.finish_live(&run_dir, &mut meta, &r, sp::ms_since(t0)) {
        let (class, text) = sp::os_error(&e, Some(&run_dir.join(".run.json.tmp")));
        meta.insert("verdict".into(), Value::from("error"));
        meta.insert("error".into(), Value::from(sp::exc_text(class, &text)));
        let _ = sp::write_meta(&run_dir, &meta, META_REDACT);
    }
}

/// Build the adapter (Python: registry `build_adapter`; SkillEvaluator has no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(SkillEvaluatorAdapter::new(ctx.clone())))
}
