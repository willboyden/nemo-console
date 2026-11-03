//! NeMo Gym adapter: NVIDIA NeMo Gym (a reinforcement-learning environment and rollout framework, run per
//! command by the lab wrapper, not a standing service) as a managed system.
//!
//! Facts this adapter is built on (the lab wrapper, `clients/nemo-gym/`):
//!   * `gym-local` is the lab wrapper (NeMo Gym 0.7.0 pinned to a scanned commit, image `lab/nemo-gym:0.7.0`).
//!     It has no `--version`. `gym-local status` is read-only (`docker compose ps`, a network inspect, an image
//!     inspect and whether the key file exists, never its value) and exits 0 whenever Docker answers.
//!   * The client runs on ONE internal network, `ai-lab-nemo-gym`, whose only way out is a router broker that
//!     holds the wrapper's own router key. `gym-local verify` checks that network is internal against the real
//!     daemon (exit 0 PASS, 1 FAIL). The broker starts on demand; "stopped" is its normal resting state.
//!   * `gym-local smoke --model ALIAS --limit N` is ONE real Gym run in the FOREGROUND: it starts the broker,
//!     the Gym servers and Ray inside the client container, rolls out N tasks of the `mcqa` environment through
//!     the router (spending router tokens on a shared engine) and prints numbers only:
//!     `rollouts: N   with a reward: M   rewards: [..]   mean: X`, an `aggregate: {..}` line and a final
//!     `RESULT: PASS smoke` or `RESULT: FAIL ...`.
//!     The rollouts themselves (prompts and completions) stay in the wrapper's Docker volume; the console keeps
//!     only those numbers.
//!   * So the console runs a smoke in the BACKGROUND itself (one at a time, like an AIPerf load run): the action
//!     returns at once, a task runs the wrapper and writes the outcome into the console's own run.json. A run a
//!     restart left at "running" reads as "interrupted"; the console never guesses a number.
//!
//! The run.json the console writes and reads (its own file, under its state dir):
//!   { "run_id": str, "model": str, "limit": int, "ts": float, "verdict":
//!     "running"|"succeeded"|"failed"|"error"|"interrupted", "duration_ms": int,
//!     "metrics": { "rollouts": int, "rewarded": int, "mean_reward": float, "pass_at_1": float },
//!     "rewards": [float, ..], "error": str }
//! A number the wrapper did not print is simply absent.

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
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger, Health,
    MetricPoint, MetricsSnapshot, ParamSpec, ParamType, Problem, Severity, SystemEvent, SystemId,
    SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::redact;
use crate::services::pyfmt::str_repr;
use crate::services::{plumbing as wsb, pystr};

pub const LABEL: &str = "NeMo Gym";
pub const LAUNCHER: &str = "gym-local";
/// The NeMo Gym release the lab wrapper pins (and the tag of its image).
pub const PINNED_VERSION: &str = "0.7.0";
pub const IMAGE: &str = "lab/nemo-gym:0.7.0";
pub const NETWORK: &str = "ai-lab-nemo-gym";
pub const BROKER: &str = "nemo-gym-router-broker";
/// `gym-local status` runs a few Docker commands; the overview polls often, so it is cached this long.
pub const STATUS_TTL: f64 = 15.0;
/// The runner caps `gym-local smoke` at this (services/runner.rs `GYM_SMOKE`).
pub const SMOKE_TIMEOUT: f64 = 3600.0;
pub const SMOKE_MAX_BYTES: usize = 256_000;
pub const KEEP_RUNS: usize = 50;
pub const EVENT_KIND: &str = "gym.smoke";
pub const VERDICTS: [&str; 5] = ["running", "succeeded", "failed", "error", "interrupted"];
pub const DEFAULT_MODEL: &str = "glm-5.3-flash-fp4-local";
/// The console's own cap on a smoke run's size (the wrapper accepts up to 50).
pub const MAX_LIMIT: i64 = 20;
pub const MAX_RUN_BYTES: u64 = 64 * 1024;
pub const MAX_REWARDS: usize = 50;
pub const LAB_WRAPPER: &str = "clients/nemo-gym/gym-local";

static RUN_ID_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A\d{8}T\d{6}-\d{3}-smoke\z").ok());
static MODEL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9._/-]{1,128}\z").ok());
static ROLLOUTS_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"rollouts:\s*(\d+)\s+with a reward:\s*(\d+)\s+rewards:\s*\[([^\]]*)\]\s+mean:\s*([0-9.eE+-]+|None)",
    )
    .ok()
});
static PASS1_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"pass@1[^'":]*['"]?\s*:\s*([0-9]+(?:\.[0-9]+)?)"#).ok());
static INTERNAL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"Internal=([A-Za-z]+)").ok());

fn is(re: &LazyLock<Option<Regex>>, s: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(s))
}

// ── what `gym-local status` says ──────────────────────────────────────────────────────────────────
/// The network's state, as `gym-local status` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Net {
    /// It exists and is internal: the containment holds.
    Internal,
    /// Not created yet: the first `gym-local up` creates it (from the compose file, internal).
    Missing,
    /// It exists but is NOT internal (the reported `Internal=` value): the client would have a route out.
    Open(String),
    /// The output did not say.
    Unknown,
}

/// The facts `gym-local status` prints, parsed. Every field is what the wrapper said, never a guess.
#[derive(Clone, Debug)]
pub struct GymState {
    /// The broker's compose status text ("Up 3 minutes (healthy)", "Exited (0) ..."), None when it was never
    /// created.
    pub broker: Option<String>,
    pub network: Net,
    pub image_built: Option<bool>,
    pub key_set: Option<bool>,
}

/// Parse `gym-local status` (stdout and stderr together).
#[must_use]
pub fn parse_status(text: &str) -> GymState {
    let mut st = GymState {
        broker: None,
        network: Net::Unknown,
        image_built: None,
        key_set: None,
    };
    for raw in pystr::splitlines(text) {
        let line = pystr::strip(raw);
        if let Some(rest) = line.strip_prefix(BROKER) {
            let s = pystr::strip(rest);
            if !s.is_empty() {
                st.broker = Some(pystr::head(s, 80).to_owned());
            }
        } else if line.starts_with("RESULT: PASS") && line.contains(NETWORK) {
            st.network = Net::Internal;
        } else if line.starts_with("RESULT: FAIL") && line.contains(NETWORK) {
            let v = INTERNAL_RE
                .as_ref()
                .and_then(|r| r.captures(line))
                .and_then(|c| c.get(1))
                .map_or("unknown", |m| m.as_str());
            st.network = if v == "missing" {
                Net::Missing
            } else {
                Net::Open(v.to_owned())
            };
        } else if let Some(rest) = line.strip_prefix("image:") {
            st.image_built = Some(!rest.contains("not built"));
        } else if let Some(rest) = line.strip_prefix("key:") {
            st.key_set = Some(!pystr::strip(rest).starts_with("missing"));
        }
    }
    st
}

fn broker_state(b: Option<&String>) -> &'static str {
    match b {
        Some(s) if s.starts_with("Up") => "running",
        Some(_) => "stopped",
        None => "absent",
    }
}

// ── what `gym-local smoke` prints ─────────────────────────────────────────────────────────────────
/// The numbers of one smoke run, from the wrapper's output. Missing numbers are simply absent.
#[must_use]
pub fn parse_smoke(stdout: &str) -> (Map<String, Value>, Vec<f64>) {
    let mut metrics = Map::new();
    let mut rewards = Vec::new();
    if let Some(c) = ROLLOUTS_RE.as_ref().and_then(|r| r.captures(stdout)) {
        if let Some(n) = c.get(1).and_then(|m| m.as_str().parse::<i64>().ok()) {
            metrics.insert("rollouts".into(), json!(n));
        }
        if let Some(n) = c.get(2).and_then(|m| m.as_str().parse::<i64>().ok()) {
            metrics.insert("rewarded".into(), json!(n));
        }
        if let Some(list) = c.get(3) {
            rewards = list
                .as_str()
                .split(',')
                .filter_map(|x| pystr::strip(x).parse::<f64>().ok())
                .filter(|x| x.is_finite())
                .take(MAX_REWARDS)
                .collect();
        }
        if let Some(v) = c
            .get(4)
            .and_then(|m| m.as_str().parse::<f64>().ok())
            .filter(|v| v.is_finite())
        {
            metrics.insert("mean_reward".into(), json!(v));
        }
    }
    let agg = pystr::splitlines(stdout)
        .into_iter()
        .find(|l| pystr::strip(l).starts_with("aggregate:"));
    if let Some(line) = agg
        && let Some(v) = PASS1_RE
            .as_ref()
            .and_then(|r| r.captures(line))
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    {
        // the aggregate may say 0.8 or 80.0; kept as a fraction either way
        metrics.insert(
            "pass_at_1".into(),
            json!(if v > 1.0 { v / 100.0 } else { v }),
        );
    }
    (metrics, rewards)
}

/// The last `RESULT:` line of the wrapper's output, if any.
fn result_line(text: &str) -> Option<String> {
    pystr::splitlines(text)
        .into_iter()
        .rev()
        .map(|l| pystr::strip(l).to_owned())
        .find(|l| l.starts_with("RESULT:"))
}

/// The exact gym-local argv for a smoke run. Nothing secret is, or can be, in this list.
#[must_use]
pub fn render_argv(model: &str, limit: i64) -> Vec<String> {
    wsb::argv(&[
        LAUNCHER,
        "smoke",
        "--model",
        model,
        "--limit",
        &limit.to_string(),
    ])
}

fn get_or(meta: &Map<String, Value>, k: &str, default: &str) -> String {
    meta.get(k)
        .map_or_else(|| default.to_owned(), wsb::json_str)
}

/// One line about a run, for the summary and the event stream.
#[must_use]
pub fn headline(meta: &Map<String, Value>) -> String {
    let mut parts = vec![get_or(meta, "model", "?"), get_or(meta, "verdict", "?")];
    let m = meta.get("metrics").and_then(Value::as_object);
    if let Some(n) = m.and_then(|m| wsb::json_num(m.get("rollouts"))) {
        parts.push(format!("{n:.0} rollouts"));
    }
    if let Some(v) = m.and_then(|m| wsb::json_num(m.get("mean_reward"))) {
        parts.push(format!("mean reward {v:.2}"));
    }
    if let Some(v) = m.and_then(|m| wsb::json_num(m.get("pass_at_1"))) {
        parts.push(format!("pass@1 {:.0}%", v * 100.0));
    }
    if let Some(c) = wsb::json_num(meta.get("duration_ms")) {
        parts.push(format!("{:.1} min", c / 60_000.0));
    }
    pystr::head(&parts.join(" · "), 300).to_owned()
}

/// The report of one run: what it was, how it ended and the numbers.
#[must_use]
pub fn report_text(meta: Option<&Map<String, Value>>, run_id: &str) -> (bool, String) {
    let Some(meta) = meta else {
        return (false, "unknown run id".to_owned());
    };
    let mut lines = vec![
        format!(
            "Run {} · {}",
            get_or(meta, "run_id", run_id),
            headline(meta)
        ),
        String::new(),
    ];
    match get_or(meta, "verdict", "?").as_str() {
        "running" => lines.push("Running. This report fills in when the run ends.".to_owned()),
        "failed" | "error" => {
            lines.push("The run did not finish:".to_owned());
            lines.push(get_or(meta, "error", ""));
        }
        "interrupted" => lines.push(format!(
            "Interrupted by {} before it finished; no numbers were recorded.",
            get_or(meta, "interrupted_by", "a console restart")
        )),
        _ => {}
    }
    lines.push(String::new());
    lines.push(format!(
        "mcqa x simple_agent through the router: model {}, {} task(s).",
        get_or(meta, "model", "?"),
        get_or(meta, "limit", "?")
    ));
    if let Some(r) = meta
        .get("rewards")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
    {
        let vals: Vec<String> = r
            .iter()
            .filter_map(|v| wsb::json_num(Some(v)))
            .map(|v| format!("{v}"))
            .collect();
        lines.push(format!("Rewards: [{}]", vals.join(", ")));
    }
    lines.push(
        "The rollouts themselves (prompts and completions) stay in the wrapper's Docker volume; the console keeps only these numbers."
            .to_owned(),
    );
    (true, lines.join("\n"))
}

// ── adapter state ─────────────────────────────────────────────────────────────────────────────────
/// (checked at, ran, the parsed status, the raw text's first line for a failure)
type StatusCache = (Instant, bool, GymState, String);

struct LiveRun {
    run_id: String,
    done: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl LiveRun {
    fn running(&self) -> bool {
        !self.done.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Marks the live run done when the task ends, however it ends.
struct DoneFlag(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DoneFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct Inner {
    ctx: AdapterContext,
    status_cache: Mutex<Option<StatusCache>>,
    status_ttl: Mutex<f64>,
    follow_interval: Mutex<f64>,
    live: Mutex<Option<LiveRun>>,
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

/// `.run.json.tmp` + 0600 + rename (the plumbing helper); errors are redacted even if a caller forgot.
fn write_meta(run_dir: &Path, meta: &Map<String, Value>) -> std::io::Result<()> {
    let mut out = meta.clone();
    if let Some(Value::String(e)) = out.get("error") {
        let red = redact(e);
        out.insert("error".into(), Value::String(red));
    }
    wsb::write_private(
        &run_dir.join("run.json"),
        &wsb::json_indent1(&Value::Object(out)),
    )
}

/// The NeMo Gym adapter. Cheap to clone handles share one state.
pub struct GymAdapter {
    inner: Arc<Inner>,
}

impl Inner {
    fn dir(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("gym_dir")
    }

    fn runs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("gym").join("runs")
    }

    /// The wrapper's path in the lab repo, for the fix text (never executed by path: the runner only runs bare
    /// allowlisted names found on PATH).
    fn lab_wrapper(&self) -> Option<PathBuf> {
        let cands = [
            self.dir().map(|b| b.join(LAUNCHER)),
            self.ctx.paths.get("repo").map(|r| r.join(LAB_WRAPPER)),
        ];
        cands.into_iter().flatten().find(|p| p.exists())
    }

    fn install_fix(&self) -> String {
        match self.lab_wrapper() {
            Some(lab) => format!(
                "put the lab wrapper on the console's PATH: ln -s {} ~/.local/bin/{LAUNCHER} (the wrapper must resolve its own symlink to find its compose file)",
                lab.display()
            ),
            None => format!(
                "install clients/nemo-gym from the lab repository (pinned NeMo Gym {PINNED_VERSION}) and link its {LAUNCHER} onto PATH"
            ),
        }
    }

    fn wrapper_cmd(&self, sub: &str) -> String {
        match self.lab_wrapper() {
            Some(lab) => format!("{} {sub}", lab.display()),
            None => format!("{LAUNCHER} {sub}"),
        }
    }

    async fn run(&self, argv: &[String], opts: RunOpts) -> CommandResult {
        wsb::run(&self.ctx, argv, opts).await
    }

    /// `gym-local status`, TTL-cached: (the wrapper ran, the parsed facts, the first error line).
    async fn probe(&self) -> (bool, GymState, String) {
        let ttl = *lock(&self.status_ttl);
        if let Some((ts, ran, st, err)) = lock(&self.status_cache).clone()
            && ts.elapsed() < Duration::from_secs_f64(ttl)
        {
            return (ran, st, err);
        }
        let r = self
            .run(
                &wsb::argv(&[LAUNCHER, "status"]),
                wsb::opts(30.0, 64 * 1024),
            )
            .await;
        let text = format!("{}\n{}", r.stdout, r.stderr);
        let ran = r.returncode == 0 && !pystr::strip(&r.stdout).is_empty();
        let st = parse_status(&text);
        let err = pystr::head(pystr::strip(&redact(&r.stderr)), 200).to_owned();
        *lock(&self.status_cache) = Some((Instant::now(), ran, st.clone(), err.clone()));
        (ran, st, err)
    }

    fn live_run_id(&self) -> Option<String> {
        lock(&self.live)
            .as_ref()
            .filter(|l| l.running())
            .map(|l| l.run_id.clone())
    }

    // ── stored runs ─────────────────────────────────────────────────────────────────────────────
    fn run_dirs(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.runs_dir()) else {
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

    /// run.json; a run left at `running` by a task that no longer runs it reads as interrupted.
    fn load_meta(&self, run_dir: &Path) -> Option<Map<String, Value>> {
        let p = run_dir.join("run.json");
        if p.is_symlink() || std::fs::metadata(&p).ok()?.len() > MAX_RUN_BYTES {
            return None;
        }
        let mut meta = wsb::load_json_dict(&p)?;
        if meta.get("verdict").and_then(Value::as_str) == Some("running") {
            let same = match (meta.get("run_id"), self.live_run_id()) {
                (Some(Value::String(r)), Some(l)) => *r == l,
                _ => false,
            };
            if !same {
                meta.insert("verdict".into(), json!("interrupted"));
                meta.insert("interrupted_by".into(), json!("a console restart"));
            }
        }
        Some(meta)
    }

    fn metas(&self, limit: usize) -> Vec<Map<String, Value>> {
        self.run_dirs()
            .iter()
            .take(limit)
            .filter_map(|d| self.load_meta(d))
            .collect()
    }

    fn event(meta: &Map<String, Value>) -> SystemEvent {
        let mut data = Map::new();
        for k in ["run_id", "model", "limit", "verdict", "duration_ms"] {
            data.insert(k.to_owned(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        if let Some(m) = meta.get("metrics").and_then(Value::as_object) {
            for k in ["rollouts", "rewarded", "mean_reward", "pass_at_1"] {
                if let Some(v) = wsb::json_num(m.get(k)) {
                    data.insert(k.to_owned(), json!(v));
                }
            }
        }
        SystemEvent {
            ts: wsb::json_num(meta.get("ts")).unwrap_or(0.0),
            system: SystemId::Gym,
            kind: EVENT_KIND.to_owned(),
            summary: headline(meta),
            data,
        }
    }

    fn list_runs(&self, limit: usize) -> Vec<SystemEvent> {
        self.metas(limit).iter().map(Self::event).collect()
    }

    /// The newest run that finished well, with its numbers.
    fn finished(&self) -> Option<Map<String, Value>> {
        self.metas(KEEP_RUNS)
            .into_iter()
            .find(|m| get_or(m, "verdict", "") == "succeeded")
    }

    fn prune(&self) {
        let live = self.live_run_id();
        for d in self.run_dirs().iter().skip(KEEP_RUNS) {
            if d.file_name().and_then(|n| n.to_str()) != live.as_deref() {
                wsb::rmtree(d);
            }
        }
    }

    fn new_run_dir(&self) -> std::io::Result<(String, PathBuf)> {
        let run_id = format!("{}-smoke", wsb::run_stamp());
        let dir = self.runs_dir().join(&run_id);
        wsb::mkdir_leaf(&dir, 0o700)?;
        Ok((run_id, dir))
    }
}

/// Writes the "interrupted" outcome if the run task is dropped (console stop) before it finished.
struct InterruptGuard {
    run_dir: PathBuf,
    meta: Map<String, Value>,
    started: Instant,
    finishing: bool,
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if self.finishing {
            return;
        }
        self.meta.insert("verdict".into(), json!("interrupted"));
        self.meta
            .insert("interrupted_by".into(), json!("console stop"));
        self.meta.insert(
            "duration_ms".into(),
            json!(i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)),
        );
        let _ = write_meta(&self.run_dir, &self.meta);
    }
}

/// The outcome of a finished wrapper call, folded into the run's meta.
fn finish(meta: &mut Map<String, Value>, r: &CommandResult, ms: i64) {
    meta.insert("duration_ms".into(), json!(ms));
    let (metrics, rewards) = parse_smoke(&r.stdout);
    if !metrics.is_empty() {
        meta.insert("metrics".into(), Value::Object(metrics));
    }
    if !rewards.is_empty() {
        meta.insert("rewards".into(), json!(rewards));
    }
    let result = result_line(&format!("{}\n{}", r.stdout, r.stderr));
    if r.timed_out {
        meta.insert("verdict".into(), json!("failed"));
        meta.insert(
            "error".into(),
            json!(format!("timed out after {SMOKE_TIMEOUT:.0} s")),
        );
    } else if r.returncode == 0
        && result
            .as_deref()
            .is_some_and(|l| l.starts_with("RESULT: PASS"))
    {
        meta.insert("verdict".into(), json!("succeeded"));
    } else {
        meta.insert("verdict".into(), json!("failed"));
        let why = result.unwrap_or_else(|| {
            let tail = pystr::strip(&r.stderr);
            if tail.is_empty() {
                format!("exit {}", r.returncode)
            } else {
                tail.to_owned()
            }
        });
        meta.insert("error".into(), json!(pystr::head(&redact(&why), 300)));
    }
}

async fn smoke_task(
    inner: Arc<Inner>,
    run_dir: PathBuf,
    argv: Vec<String>,
    meta: Map<String, Value>,
    flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let _done = DoneFlag(flag);
    let mut guard = InterruptGuard {
        run_dir: run_dir.clone(),
        meta,
        started: Instant::now(),
        finishing: false,
    };
    let r = inner
        .run(
            &argv,
            RunOpts::timeout(SMOKE_TIMEOUT)
                .max_bytes(SMOKE_MAX_BYTES)
                .cwd(run_dir.clone()),
        )
        .await;
    guard.finishing = true; // from here this task writes the outcome
    let ms = i64::try_from(guard.started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let mut meta = std::mem::take(&mut guard.meta);
    finish(&mut meta, &r, ms);
    if let Err(e) = write_meta(&run_dir, &meta) {
        meta.insert("verdict".into(), json!("error"));
        meta.insert(
            "error".into(),
            json!(pystr::head(&format!("OSError: {e}"), 300)),
        );
        let _ = write_meta(&run_dir, &meta);
    }
    *lock(&inner.status_cache) = None; // the broker state changed: re-probe on the next status
    inner.prune();
}

#[async_trait]
impl SystemAdapter for GymAdapter {
    fn id(&self) -> SystemId {
        SystemId::Gym
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let inner = &self.inner;
        let (ran, st, err) = inner.probe().await;
        let last = inner.metas(1);
        let mut problems: Vec<Problem> = Vec::new();
        // Component kinds and states are the domain contract's (container|systemd|process|http and
        // running|stopped|failed|absent|unknown): the network and the image are facts about the wrapper's client,
        // so they live in its detail, the summary and the problems, not as components of their own.
        let net_text = match &st.network {
            Net::Internal => "network internal".to_owned(),
            Net::Missing => "network not created yet".to_owned(),
            Net::Open(v) => format!("network NOT internal (Internal={v})"),
            Net::Unknown => "network ?".to_owned(),
        };
        let mut components = vec![Component {
            kind: "process".into(),
            name: LAUNCHER.into(),
            state: if ran { "running" } else { "absent" }.into(),
            image: (ran && st.image_built == Some(true)).then(|| IMAGE.to_owned()),
            detail: Some(if ran {
                format!(
                    "the lab wrapper (NeMo Gym {PINNED_VERSION}, pinned commit); runs each Gym command in a client container ({IMAGE} {}) on the internal network {NETWORK} ({net_text})",
                    match st.image_built {
                        Some(true) => "built",
                        Some(false) => "not built",
                        None => "?",
                    }
                )
            } else if err.is_empty() {
                format!(
                    "{LAUNCHER} is not on the console's PATH (clients/nemo-gym/ in the lab repository)"
                )
            } else {
                format!("{LAUNCHER} did not answer: {err}")
            }),
        }];
        if ran {
            components.push(Component {
                kind: "container".into(),
                name: BROKER.into(),
                state: broker_state(st.broker.as_ref()).into(),
                image: None,
                detail: Some(format!(
                    "{}: the client's only way out (holds the wrapper's router key; the client never does). Started on demand, so 'stopped' is normal.",
                    st.broker.clone().unwrap_or_else(|| "not created yet".into())
                )),
            });
        }
        if let Some(id) = inner.live_run_id() {
            components.push(Component {
                kind: "process".into(),
                name: format!("smoke run {id}"),
                state: "running".into(),
                image: None,
                detail: Some("a background smoke run (one at a time)".into()),
            });
        }
        if !ran {
            problems.push(Problem::new(
                Severity::Error,
                "gym.not_installed",
                "NeMo Gym's lab wrapper (gym-local) is not on the console's PATH.",
                Some(inner.install_fix()),
            ));
        } else {
            if let Net::Open(v) = &st.network {
                problems.push(Problem::new(
                    Severity::Error,
                    "gym.not_contained",
                    format!("The client network {NETWORK} exists but is not internal (Internal={v}): a Gym container would have a route out."),
                    Some(format!(
                        "{} (refuses to start while the network is open); recreate the network from the wrapper's compose file, then {}",
                        inner.wrapper_cmd("verify"),
                        inner.wrapper_cmd("verify")
                    )),
                ));
            }
            if st.image_built == Some(false) {
                problems.push(Problem::new(
                    Severity::Warn,
                    "gym.no_image",
                    format!("The client image {IMAGE} is not built: there is nothing to run."),
                    Some(inner.wrapper_cmd("build")),
                ));
            }
            if st.key_set == Some(false) {
                problems.push(Problem::new(
                    Severity::Warn,
                    "gym.no_key",
                    "The wrapper's router key file is missing: the broker cannot start.",
                    Some(inner.wrapper_cmd("key")),
                ));
            }
        }
        if let Some(m) = last.first()
            && get_or(m, "verdict", "") == "failed"
        {
            problems.push(Problem::new(
                Severity::Warn,
                "gym.last_smoke_failed",
                format!(
                    "The last smoke run failed: {}",
                    get_or(m, "error", "no reason recorded")
                ),
                Some("'Show a run' has the wrapper's RESULT line".to_owned()),
            ));
        }
        let open = matches!(st.network, Net::Open(_));
        let health = if !ran || open {
            Health::Down
        } else if st.image_built == Some(false) || st.key_set == Some(false) {
            Health::Degraded
        } else {
            Health::Up
        };
        let mut summary = if ran {
            format!(
                "image {} · key {} · broker {} · {net_text}",
                match st.image_built {
                    Some(true) => "built",
                    Some(false) => "missing",
                    None => "?",
                },
                match st.key_set {
                    Some(true) => "[set]",
                    Some(false) => "[missing]",
                    None => "?",
                },
                broker_state(st.broker.as_ref())
            )
        } else {
            "gym-local is not on PATH".to_owned()
        };
        if let Some(m) = last.first() {
            summary.push_str(&format!(" · last smoke: {}", headline(m)));
        }
        Ok(SystemStatus {
            system: SystemId::Gym,
            health,
            version: if ran && st.image_built == Some(true) {
                Some(PINNED_VERSION.to_owned())
            } else {
                None
            },
            summary: pystr::head(&summary, 300).to_owned(),
            endpoints: vec![],
            components,
            problems,
            checked_at: now_ts(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let inner = &self.inner;
        let metas = inner.metas(KEEP_RUNS);
        let point = |key: &str, value: f64, unit: &str| MetricPoint {
            key: key.into(),
            value,
            unit: unit.into(),
            labels: IndexMap::new(),
        };
        #[allow(clippy::cast_precision_loss)]
        let mut points = vec![point("gym.runs", metas.len() as f64, "1")];
        if !metas.is_empty() {
            let running = f64::from(u8::from(inner.live_run_id().is_some()));
            points.push(point("gym.live_running", running, "1"));
            for verdict in VERDICTS {
                let n = metas
                    .iter()
                    .filter(|m| get_or(m, "verdict", "") == verdict)
                    .count();
                let mut labels = IndexMap::new();
                labels.insert("verdict".to_owned(), verdict.to_owned());
                #[allow(clippy::cast_precision_loss)]
                points.push(MetricPoint {
                    key: "gym.runs_by_verdict".into(),
                    value: n as f64,
                    unit: "1".into(),
                    labels,
                });
            }
        }
        if let Some(m) = inner.finished() {
            if let Some(metrics) = m.get("metrics").and_then(Value::as_object) {
                for (key, name, unit) in [
                    ("mean_reward", "gym.mean_reward", "1"),
                    ("rollouts", "gym.rollouts", "1"),
                    ("pass_at_1", "gym.pass_at_1", "ratio"),
                ] {
                    if let Some(v) = wsb::json_num(metrics.get(key)) {
                        points.push(point(name, v, unit));
                    }
                }
            }
            if let Some(v) = wsb::json_num(m.get("duration_ms")) {
                points.push(point("gym.duration_s", v / 1000.0, "s"));
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Gym,
            ts: now_ts(),
            available: true,
            points,
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(self.inner.list_runs(limit.min(KEEP_RUNS)))
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        let inner = Arc::clone(&self.inner);
        Box::pin(async_stream::stream! {
            // keyed by (run, verdict): a run is announced when it starts AND when it ends
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            let mut first = true;
            loop {
                let mut events = inner.list_runs(50);
                events.sort_by(|a, b| a.ts.total_cmp(&b.ts));
                for ev in events {
                    let key = (
                        wsb::json_get_str(ev.data.get("run_id")),
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

    /// No config file is edited here: the Gym environment config belongs to the lab wrapper.
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

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let mut model = ParamSpec::new(
            "model",
            "Model alias",
            "A LOCAL router alias the wrapper's key may use (the router refuses others).",
            ParamType::String,
        );
        model.default = json!(DEFAULT_MODEL);
        model.requires_restart = false;
        let mut limit = ParamSpec::new(
            "limit",
            "Tasks",
            "How many mcqa tasks to roll out (1-20).",
            ParamType::Int,
        );
        limit.default = json!(5);
        limit.min = Some(1.0);
        #[allow(clippy::cast_precision_loss)]
        {
            limit.max = Some(MAX_LIMIT as f64);
        }
        limit.requires_restart = false;
        let mut run_id = ParamSpec::new(
            "run_id",
            "Run",
            "A run id from the runs list; empty = the newest run.",
            ParamType::String,
        );
        run_id.requires_restart = false;
        let mut smoke = ActionSpec::new(
            "smoke",
            "Run a smoke rollout",
            "Starts `gym-local smoke`: the mcqa environment x simple_agent rolls out N tasks through the router and scores each one. disruptive: it spends router tokens on a shared engine and starts the broker, the Gym servers and Ray in the client container (a few minutes). One run at a time, in the background; watch the runs list, then 'Show a run'.",
            Danger::Disruptive,
        );
        smoke.params = vec![model, limit];
        let verify = ActionSpec::new(
            "verify",
            "Check the containment",
            "`gym-local verify`: the client network ai-lab-nemo-gym must be internal (checked against the Docker daemon). Read-only.",
            Danger::Safe,
        );
        let mut report = ActionSpec::new(
            "report",
            "Show a run",
            "The report of a stored smoke run (newest first): verdict, model, rollouts and rewards.",
            Danger::Safe,
        );
        report.params = vec![run_id];
        Ok(vec![smoke, verify, report])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        match action_id {
            "report" => {
                let run_id = match params.get("run_id") {
                    Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
                    _ => String::new(),
                };
                let meta = self.run_meta(&run_id);
                let (ok, text) = report_text(meta.as_ref(), &run_id);
                Ok(done(t0, ok, &text, Vec::new()))
            }
            "verify" => {
                let r = self
                    .inner
                    .run(&wsb::argv(&[LAUNCHER, "verify"]), wsb::opts(30.0, 16_384))
                    .await;
                *lock(&self.inner.status_cache) = None;
                let out = format!("{}\n{}", r.stdout, r.stderr);
                Ok(done(t0, r.returncode == 0, pystr::strip(&out), Vec::new()))
            }
            "smoke" => self.start_smoke(params, t0).await,
            _ => Err(AdapterError::UnknownId(action_id.to_owned())),
        }
    }
}

impl GymAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        GymAdapter {
            inner: Arc::new(Inner {
                ctx,
                status_cache: Mutex::new(None),
                status_ttl: Mutex::new(STATUS_TTL),
                follow_interval: Mutex::new(2.0),
                live: Mutex::new(None),
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

    /// Tests: 0 makes every status call probe again.
    pub fn set_status_ttl(&self, s: f64) {
        *lock(&self.inner.status_ttl) = s;
    }

    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.inner.runs_dir()
    }

    /// The run the background task is writing, while it runs.
    #[must_use]
    pub fn live_run_id(&self) -> Option<String> {
        self.inner.live_run_id()
    }

    /// Wait for the background run to end; false if it was cancelled.
    pub async fn join_live(&self) -> bool {
        let handle = lock(&self.inner.live)
            .as_mut()
            .and_then(|l| l.handle.take());
        match handle {
            Some(h) => h.await.is_ok(),
            None => true,
        }
    }

    /// The meta of one run (by id, or the newest one when the id is empty).
    pub fn run_meta(&self, run_id: &str) -> Option<Map<String, Value>> {
        let inner = &self.inner;
        if run_id.is_empty() {
            return inner.metas(1).into_iter().next();
        }
        if !is(&RUN_ID_RE, run_id) {
            return None;
        }
        inner.load_meta(&inner.runs_dir().join(run_id))
    }

    async fn start_smoke(
        &self,
        params: &Map<String, Value>,
        t0: Instant,
    ) -> Result<ActionResult, AdapterError> {
        let inner = &self.inner;
        let model = match params.get("model") {
            Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
            _ => DEFAULT_MODEL.to_owned(),
        };
        if !is(&MODEL_RE, &model) {
            return Ok(done(
                t0,
                false,
                &format!("bad model alias {}", str_repr(&model)),
                Vec::new(),
            ));
        }
        let limit = match params.get("limit") {
            None | Some(Value::Null) => 5,
            Some(v) => match wsb::json_num(Some(v)) {
                #[allow(clippy::cast_possible_truncation)]
                Some(n) if n.fract() == 0.0 && (1.0..=20.0).contains(&n) => n as i64,
                _ => {
                    return Ok(done(
                        t0,
                        false,
                        &format!("tasks must be a whole number from 1 to {MAX_LIMIT}"),
                        Vec::new(),
                    ));
                }
            },
        };
        let (ran, st, _) = inner.probe().await;
        if !ran {
            return Ok(done(
                t0,
                false,
                "NeMo Gym's wrapper is not on the console's PATH.",
                problem(
                    "gym.not_installed",
                    "gym-local is not on PATH.",
                    &inner.install_fix(),
                ),
            ));
        }
        if let Net::Open(v) = &st.network {
            return Ok(done(
                t0,
                false,
                &format!(
                    "The client network {NETWORK} is not internal (Internal={v}); no run starts until it is."
                ),
                problem(
                    "gym.not_contained",
                    "the containment check fails",
                    &inner.wrapper_cmd("verify"),
                ),
            ));
        }
        if st.image_built == Some(false) {
            return Ok(done(
                t0,
                false,
                &format!("The client image {IMAGE} is not built."),
                problem("gym.no_image", "no image", &inner.wrapper_cmd("build")),
            ));
        }
        // From here to the spawn nothing awaits, and the live slot is held: no second request slips in between.
        let mut live = lock(&inner.live);
        if let Some(busy) = live
            .as_ref()
            .filter(|l| l.running())
            .map(|l| l.run_id.clone())
        {
            return Ok(done(
                t0,
                false,
                &format!("A smoke run is already running ({busy}); one runs at a time."),
                Vec::new(),
            ));
        }
        let (run_id, run_dir) = inner
            .new_run_dir()
            .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let argv = render_argv(&model, limit);
        let meta = json!({
            "run_id": run_id, "model": model, "limit": limit, "ts": now_ts(), "verdict": "running",
            "environment": "mcqa", "agent": "simple_agent", "launcher": LAUNCHER,
        });
        let meta = meta.as_object().cloned().unwrap_or_default();
        write_meta(&run_dir, &meta).map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = smoke_task(Arc::clone(inner), run_dir, argv, meta, Arc::clone(&flag));
        let handle = tokio::spawn(async move {
            crate::services::common::contained("gym smoke run", task).await;
        });
        *live = Some(LiveRun {
            run_id: run_id.clone(),
            done: flag,
            handle: Some(handle),
        });
        drop(live);
        Ok(done(
            t0,
            true,
            &format!(
                "Started smoke run {run_id} (mcqa x simple_agent, model {model}, {limit} task(s)). It runs in the background: watch the runs list, then 'Show a run' with this run id."
            ),
            Vec::new(),
        ))
    }
}

/// Build the adapter (NeMo Gym has no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(GymAdapter::new(ctx.clone())))
}
