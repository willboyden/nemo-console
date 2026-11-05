//! NVIDIA Flare: NVIDIA's research-prototype Rust inference server (github.com/NVIDIA/spark-local-inference), on sm120
//! GPUs (RTX PRO 6000, RTX 5090) and sm121 GPUs (DGX Spark; one, or several as one engine).
//!
//! Everything goes through the lab launcher `flare-local` (inference/flare/flare.sh, linked onto PATH): it defines the
//! targets (names, hosts, chip, tensor-parallel sizes), holds the checks that make starting an engine safe (the GPU must
//! be free, nothing is stopped for you, the lane lock), reaches remote hosts over ssh, and `flare-local status-json`
//! reports every target in one JSON array. The console learns the targets from that report and passes only names it
//! listed; it never talks to a remote engine itself: its own HTTP stays on loopback (the playground uses the local
//! target only).
//!
//! A Flare package is prepared per GPU family x tensor-parallel size ON that family (Ignition), so each target lists the
//! packages of its family.
//!
//! Starting an engine (`up`) can take many minutes (package load, for a multi-host target a peer transfer over the
//! fabric), so it runs in the background, one at a time; the status shows it and "Show the last start" prints its output.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

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
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType,
    PlaygroundSpec, Problem, Severity, SystemEvent, SystemId, SystemStatus, ValidationResult,
    now_ts,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::pytoml as tomllib;
use crate::services::pyyaml;
use crate::services::{plumbing as wsb, pystr};

pub const LABEL: &str = "NVIDIA Flare";
pub const LAUNCHER: &str = "flare-local";
/// The chip families Flare packages exist for: sm120 (gb202: RTX PRO 6000, RTX 5090) and sm121 (gb10: DGX Spark).
/// The targets themselves (names, hosts, modes) come from the launcher at run time, never from this file.
pub const CHIPS: [&str; 2] = ["sm120", "sm121"];
pub const MODES: [&str; 4] = ["auto", "tp1", "tp2", "tp4"];
/// One `status-json` (one ssh per host) per minute at most, whatever polls; pages get the last report at once.
pub const STATUS_TTL: f64 = 60.0;
/// `flare-local up` waits up to 30 minutes for /health (remote) or 15 (local); a little more for its preflight.
pub const UP_TIMEOUT: f64 = 1900.0;
pub const DEFAULT_URL: &str = "http://127.0.0.1:8035";
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;
/// The lane's files under inference/flare, all read-only here: (id, label, file name, format, description).
pub const FILES: [(&str, &str, &str, &str, &str); 4] = [
    (
        "compose",
        "docker-compose.yml",
        "docker-compose.yml",
        "yaml",
        "The local serving container: image, ports, mounts, the `flare run` command line.",
    ),
    (
        "compose-tp1",
        "docker-compose.tp1.yml",
        "docker-compose.tp1.yml",
        "yaml",
        "The local tp1 override: one GPU.",
    ),
    (
        "compose-tp2",
        "docker-compose.tp2.yml",
        "docker-compose.tp2.yml",
        "yaml",
        "The local tp2 override: both GPUs.",
    ),
    (
        "ignition",
        "ignition-config.toml",
        "ignition-config.toml",
        "toml",
        "The seed config of Ignition, the package preparation tool (cache and toolkit paths).",
    ),
];
pub const WHY_READONLY: &str = "Flare is started and stopped with flare.sh (this page's actions run it as flare-local), which checks that the GPUs are free first; a bare compose start skips those checks. These files change in git, not from the console.";

#[allow(clippy::expect_used)]
static PKG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._-]{0,200}\z").expect("static regex"));
/// A target or host name as the launcher prints it (it becomes an argv element; never a path or an option).
#[allow(clippy::expect_used)]
static TARGET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9-]{0,31}\z").expect("static regex"));

/// One target as `flare-local status-json` reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Target {
    pub target: String,
    pub chip: String,
    pub family: String,
    pub modes: String,
    pub hosts: Vec<String>,
    pub url: String,
    pub build: String,
    pub container: String,
    pub image: Option<bool>,
    pub serving: bool,
    pub busy: String,
    pub packages: Vec<String>,
    pub model: Option<String>,
    pub max_model_len: Option<f64>,
    pub metrics: Option<Map<String, Value>>,
    pub error: Option<String>,
}

fn s_of(o: &Map<String, Value>, k: &str) -> String {
    o.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// Parse `flare-local status-json`. Unknown or malformed entries are skipped; names are checked again here.
#[must_use]
pub fn parse_status(text: &str) -> Option<Vec<Target>> {
    let v: Value = serde_json::from_str(pystr::strip(text)).ok()?;
    let arr = v.as_array()?;
    let mut out = Vec::new();
    for e in arr {
        let Some(o) = e.as_object() else { continue };
        let target = s_of(o, "target");
        if !TARGET_RE.is_match(&target) {
            continue;
        }
        let list = |k: &str| -> Vec<String> {
            o.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| {
                            if k == "hosts" {
                                TARGET_RE.is_match(s)
                            } else {
                                PKG_RE.is_match(s)
                            }
                        })
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let models = o.get("models").and_then(Value::as_object);
        let first = models
            .and_then(|m| m.get("data"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_object);
        out.push(Target {
            chip: s_of(o, "chip"),
            family: s_of(o, "family"),
            modes: s_of(o, "modes"),
            hosts: list("hosts"),
            url: s_of(o, "url"),
            build: s_of(o, "build"),
            container: {
                let c = s_of(o, "container");
                if c.is_empty() { "absent".to_owned() } else { c }
            },
            image: o.get("image").and_then(Value::as_bool),
            serving: s_of(o, "health") == "up",
            busy: pystr::head(&s_of(o, "busy"), 400).to_owned(),
            packages: list("packages"),
            model: first
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                .map(|s| pystr::head(s, 200).to_owned()),
            max_model_len: first.and_then(|m| wsb::json_num(m.get("max_model_len"))),
            metrics: o.get("metrics").and_then(Value::as_object).cloned(),
            error: o.get("error").and_then(Value::as_str).map(str::to_owned),
            target,
        });
    }
    Some(out)
}

/// The launcher's state: did it run, and what it reported.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub ran: bool,
    pub err: String,
    pub targets: Vec<Target>,
    /// No report yet: the first check is still running.
    pub checking: bool,
}

/// A background `flare-local up`.
#[derive(Debug, Clone)]
pub struct StartRun {
    pub target: String,
    pub package: String,
    pub started: f64,
    pub finished: Option<f64>,
    pub ok: Option<bool>,
    pub output: String,
}

struct Inner {
    ctx: AdapterContext,
    cache: Mutex<Option<(Instant, Probe)>>,
    ttl: Mutex<f64>,
    start: Mutex<Option<StartRun>>,
    /// Single flight: at most one `status-json` runs, in the background, whatever polls (pages, the metrics loop).
    refreshing: std::sync::atomic::AtomicBool,
    /// After an action the report is stale but kept: polls still get it at once while a refresh runs.
    expired: std::sync::atomic::AtomicBool,
    /// Bumped by every finished refresh (an action that needs a fresh report waits for the next one).
    generation: std::sync::atomic::AtomicU64,
    /// Bumped by every forget(): a refresh that started before it does not count as fresh.
    forgets: std::sync::atomic::AtomicU64,
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

impl Inner {
    fn dir(&self) -> Option<&PathBuf> {
        self.ctx.paths.get("flare_dir")
    }

    fn install_fix(&self) -> String {
        match self.dir() {
            Some(d) => format!(
                "ln -s {} ~/.local/bin/{LAUNCHER}",
                d.join("flare.sh").display()
            ),
            None => format!("put inference/flare/flare.sh on PATH as {LAUNCHER}"),
        }
    }

    async fn run(&self, argv: &[String], opts: RunOpts) -> CommandResult {
        wsb::run(&self.ctx, argv, opts).await
    }

    /// The launcher's last report. A stale one is returned at once while a refresh runs in the background (one at a
    /// time); only when there is no report at all does this wait, up to `wait` seconds. The launcher talks to every
    /// remote host over ssh, so it must never run inside the console's per-call deadline (a cancelled run would be
    /// killed before it caches anything, and the next poll would start it again).
    async fn probe(self: &Arc<Self>, wait: f64) -> Probe {
        let ttl = *lock(&self.ttl);
        let expired = self.expired.load(std::sync::atomic::Ordering::SeqCst);
        let stale = match lock(&self.cache).as_ref() {
            Some((at, p)) if !expired && at.elapsed().as_secs_f64() < ttl => return p.clone(),
            Some((_, p)) => Some(p.clone()),
            None => None,
        };
        if !self
            .refreshing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let me = Arc::clone(self);
            tokio::spawn(async move {
                let task = async {
                    // a run that an action overtook (forget() during it) may predate that action: keep its
                    // report for polls, but it neither clears `expired` nor counts as fresh
                    let f0 = me.forgets.load(std::sync::atomic::Ordering::SeqCst);
                    let p = me.run_status().await;
                    *lock(&me.cache) = Some((Instant::now(), p));
                    if me.forgets.load(std::sync::atomic::Ordering::SeqCst) == f0 {
                        me.expired.store(false, std::sync::atomic::Ordering::SeqCst);
                        me.generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                };
                crate::services::common::contained("flare status", task).await;
                me.refreshing
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        if let Some(p) = stale {
            return p;
        }
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f64() < wait {
            if let Some((_, p)) = lock(&self.cache).as_ref() {
                return p.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Probe {
            ran: true,
            checking: true,
            ..Probe::default()
        }
    }

    async fn run_status(&self) -> Probe {
        let r = self
            .run(
                &wsb::argv(&[LAUNCHER, "status-json"]),
                wsb::opts(60.0, 256_000),
            )
            .await;
        if r.returncode == 127 && r.stdout.is_empty() {
            return Probe {
                ran: false,
                err: pystr::head(pystr::strip(&r.stderr), 300).to_owned(),
                ..Probe::default()
            };
        }
        match parse_status(&r.stdout) {
            Some(targets) => Probe {
                ran: true,
                targets,
                ..Probe::default()
            },
            None => Probe {
                ran: true,
                err: format!(
                    "{LAUNCHER} status-json gave no JSON (exit {}): {}",
                    r.returncode,
                    pystr::head(pystr::strip(&r.stderr), 200)
                ),
                ..Probe::default()
            },
        }
    }

    /// Mark the report stale (after an action changed something): the next poll starts a refresh.
    fn forget(&self) {
        self.forgets
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.expired
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// A report collected after this call (up to `wait` seconds), else the last one.
    async fn probe_fresh(self: &Arc<Self>, wait: f64) -> Probe {
        let g0 = self.generation.load(std::sync::atomic::Ordering::SeqCst);
        self.forget();
        let t0 = Instant::now();
        let mut p = self.probe(wait).await;
        while self.generation.load(std::sync::atomic::Ordering::SeqCst) == g0
            && t0.elapsed().as_secs_f64() < wait
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            p = self.probe(0.0).await;
        }
        if let Some((_, fresh)) = lock(&self.cache).as_ref()
            && self.generation.load(std::sync::atomic::Ordering::SeqCst) != g0
        {
            p = fresh.clone();
        }
        p
    }

    fn base_url(&self) -> String {
        self.ctx
            .urls
            .get("flare")
            .cloned()
            .unwrap_or_else(|| DEFAULT_URL.to_owned())
            .trim_end_matches('/')
            .to_owned()
    }

    fn starting(&self) -> Option<StartRun> {
        lock(&self.start).clone().filter(|s| s.finished.is_none())
    }
}

pub struct FlareAdapter {
    inner: Arc<Inner>,
}

impl FlareAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        FlareAdapter {
            inner: Arc::new(Inner {
                ctx,
                cache: Mutex::new(None),
                ttl: Mutex::new(STATUS_TTL),
                start: Mutex::new(None),
                refreshing: std::sync::atomic::AtomicBool::new(false),
                expired: std::sync::atomic::AtomicBool::new(false),
                generation: std::sync::atomic::AtomicU64::new(0),
                forgets: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    /// Tests: 0 makes every status call run the launcher again.
    pub fn set_status_ttl(&self, s: f64) {
        *lock(&self.inner.ttl) = s;
    }

    /// The last background start (running or finished).
    #[must_use]
    pub fn last_start(&self) -> Option<StartRun> {
        lock(&self.inner.start).clone()
    }

    fn file(&self, file_id: &str) -> Result<(PathBuf, ConfigFileSpec), AdapterError> {
        let Some(&(id, label, name, fmt, desc)) = FILES.iter().find(|f| f.0 == file_id) else {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        };
        let dir = self
            .inner
            .dir()
            .ok_or_else(|| AdapterError::UnknownId(file_id.to_owned()))?;
        let spec = ConfigFileSpec {
            id: id.to_owned(),
            label: label.to_owned(),
            description: desc.to_owned(),
            display_path: format!("inference/flare/{name}"),
            format: fmt.to_owned(),
            editable: false,
            why_readonly: Some(WHY_READONLY.to_owned()),
            validators: vec![format!("{} parse", fmt.to_uppercase())],
            authoritative: false,
            apply_effect: "read-only: nothing to apply".to_owned(),
        };
        Ok((dir.join(name), spec))
    }

    async fn start_up(
        &self,
        target: &str,
        package: &str,
        mode: &str,
        t0: Instant,
    ) -> Result<ActionResult, AdapterError> {
        let inner = Arc::clone(&self.inner);
        let mut slot = lock(&inner.start);
        if let Some(s) = slot.as_ref().filter(|s| s.finished.is_none()) {
            return Ok(done(
                t0,
                false,
                &format!(
                    "A start is already running (package {} on {}); one at a time.",
                    s.package, s.target
                ),
                Vec::new(),
            ));
        }
        let mut argv = wsb::argv(&[LAUNCHER, "up", target, package]);
        if mode != "auto" {
            argv.push(mode.to_owned());
        }
        *slot = Some(StartRun {
            target: target.to_owned(),
            package: package.to_owned(),
            started: now_ts(),
            finished: None,
            ok: None,
            output: String::new(),
        });
        drop(slot);
        inner.forget();
        let task_inner = Arc::clone(&inner);
        let task = async move {
            let r = task_inner.run(&argv, wsb::opts(UP_TIMEOUT, 64_000)).await;
            let mut out = format!("{}\n{}", r.stdout, r.stderr);
            if r.timed_out {
                out.push_str(
                    "\n(stopped waiting: the container may still be loading; check the status)",
                );
            }
            if let Some(s) = lock(&task_inner.start).as_mut() {
                s.finished = Some(now_ts());
                s.ok = Some(r.returncode == 0);
                s.output = pystr::head(&redact(pystr::strip(&out)), 20_000).to_owned();
            }
            task_inner.forget();
        };
        tokio::spawn(async move {
            crate::services::common::contained("flare start", task).await;
        });
        Ok(done(
            t0,
            true,
            &format!(
                "Starting {package} on {target} in the background. {LAUNCHER} refuses unless the GPU(s) are free and nothing is stopped for you; loading can take many minutes. Watch the status, then 'Show the last start' and 'Probe'."
            ),
            Vec::new(),
        ))
    }
}

fn target_param(known: &[Target]) -> ParamSpec {
    let mut p = ParamSpec::new(
        "target",
        "Target",
        "Where Flare runs, as the launcher defines it: each target has a chip (sm120 takes gb202 packages, sm121 gb10) and the tensor-parallel sizes it can run. Show every target lists them.",
        if known.is_empty() {
            ParamType::String
        } else {
            ParamType::Enum
        },
    );
    if let Some(first) = known.first() {
        p.choices = Some(known.iter().map(|t| t.target.clone()).collect());
        p.default = json!(first.target);
    }
    p.requires_restart = false;
    p
}

fn mode_param() -> ParamSpec {
    let mut p = ParamSpec::new(
        "mode",
        "Tensor-parallel size",
        "Must match how the package was exported, and the target must offer it. auto = the target's only mode; a target with several (e.g. tp1 and tp2) needs one named.",
        ParamType::Enum,
    );
    p.choices = Some(MODES.iter().map(|m| (*m).to_owned()).collect());
    p.default = json!("auto");
    p.requires_restart = false;
    p
}

fn get_str(params: &Map<String, Value>, k: &str, default: &str) -> String {
    match params.get(k) {
        Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
        _ => default.to_owned(),
    }
}

/// A one-line description of a target for components, summaries and the status action.
#[must_use]
pub fn describe(t: &Target) -> String {
    let mut parts = vec![format!(
        "{} {} ({}), modes {}",
        t.chip,
        t.family,
        t.hosts.join(" "),
        t.modes
    )];
    if let Some(e) = &t.error {
        parts.push(format!("check failed: {e}"));
        return parts.join(" · ");
    }
    if t.serving {
        let mut s = format!("serving {}", t.model.as_deref().unwrap_or("?"));
        if let Some(n) = t.max_model_len {
            s.push_str(&format!(", context {n:.0}"));
        }
        parts.push(s);
    } else {
        parts.push(format!("container {}", t.container));
    }
    parts.push(match t.image {
        Some(true) => format!("image lab/flare:{} loaded", t.build),
        Some(false) => "image not loaded".to_owned(),
        None => "image ?".to_owned(),
    });
    parts.push(format!("{} package(s)", t.packages.len()));
    parts.push(if t.busy.is_empty() {
        "GPU free".to_owned()
    } else {
        format!("GPU busy: {}", t.busy)
    });
    parts.join(" · ")
}

#[async_trait]
impl SystemAdapter for FlareAdapter {
    fn id(&self) -> SystemId {
        SystemId::Flare
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let inner = &self.inner;
        let p = inner.probe(5.0).await;
        let starting = inner.starting();
        let mut problems: Vec<Problem> = Vec::new();
        let mut components: Vec<Component> = vec![Component {
            kind: "process".into(),
            name: LAUNCHER.into(),
            state: if p.ran { "running" } else { "absent" }.into(),
            image: None,
            detail: Some(if p.ran {
                "the lab launcher (inference/flare/flare.sh): checks the GPUs, starts and stops Flare, reaches the AITop100 and the Sparks over ssh".to_owned()
            } else if p.err.is_empty() {
                format!("{LAUNCHER} is not on the console's PATH")
            } else {
                format!("{LAUNCHER} did not answer: {}", p.err)
            }),
        }];
        for t in &p.targets {
            let state = if t.error.is_some() || t.container == "unknown" {
                "unknown"
            } else if t.serving || t.container == "running" {
                "running"
            } else if t.container == "absent" {
                "absent"
            } else if t.container == "exited" || t.container == "dead" {
                "failed"
            } else {
                "stopped"
            };
            components.push(Component {
                kind: "container".into(),
                name: format!("flare @ {}", t.target),
                state: state.into(),
                image: (t.image == Some(true)).then(|| format!("lab/flare:{}", t.build)),
                detail: Some(describe(t)),
            });
            if let Some(e) = &t.error {
                problems.push(Problem::new(
                    Severity::Warn,
                    &format!("flare.{}.unreachable", t.target),
                    format!(
                        "Target {} could not be checked: {}.",
                        t.target,
                        pystr::head(e, 200)
                    ),
                    Some(format!(
                        "ssh {} true",
                        t.hosts.first().map_or("<host>", String::as_str)
                    )),
                ));
            }
            if t.container == "running" && !t.serving {
                problems.push(Problem::new(
                    Severity::Warn,
                    &format!("flare.{}.loading", t.target),
                    format!(
                        "Flare's container on {} runs but /health does not answer yet (loading, or failing).",
                        t.target
                    ),
                    Some(format!("{LAUNCHER} log {}", t.target)),
                ));
            }
            if matches!(t.container.as_str(), "exited" | "dead") {
                problems.push(Problem::new(
                    Severity::Warn,
                    &format!("flare.{}.exited", t.target),
                    format!(
                        "Flare's container on {} has stopped ({}).",
                        t.target, t.container
                    ),
                    Some(format!("{LAUNCHER} log {}", t.target)),
                ));
            }
        }
        if let Some(s) = &starting {
            components.push(Component {
                kind: "process".into(),
                name: format!("start on {}", s.target),
                state: "running".into(),
                image: None,
                detail: Some(format!(
                    "{LAUNCHER} up {} {} (in the background)",
                    s.target, s.package
                )),
            });
        }
        if let Some(s) = lock(&inner.start).as_ref()
            && s.ok == Some(false)
        {
            problems.push(Problem::new(
                Severity::Warn,
                "flare.last_start_failed",
                format!("The last start ({} on {}) failed.", s.package, s.target),
                Some("'Show the last start' has the launcher's output".to_owned()),
            ));
        }
        let serving: Vec<&Target> = p.targets.iter().filter(|t| t.serving).collect();
        if !p.ran {
            problems.push(Problem::new(
                Severity::Error,
                "flare.not_installed",
                "Flare's lab launcher (flare-local) is not on the console's PATH.",
                Some(inner.install_fix()),
            ));
        } else if p.checking {
            // the first report is still being collected (ssh to every host): nothing to say yet
        } else if !p.err.is_empty() {
            problems.push(Problem::new(
                Severity::Error,
                "flare.status_failed",
                pystr::head(&p.err, 300).to_owned(),
                Some(format!("{LAUNCHER} status-json")),
            ));
        } else if serving.is_empty() {
            let gb202 = p
                .targets
                .iter()
                .find(|t| t.family == "gb202")
                .map_or(0, |t| t.packages.len());
            let gb10 = p
                .targets
                .iter()
                .find(|t| t.family == "gb10")
                .map_or(0, |t| t.packages.len());
            problems.push(Problem::new(
                Severity::Info,
                "flare.stopped",
                if gb202 + gb10 == 0 {
                    "Flare is not serving anywhere, and no package is prepared yet (a package is prepared once per GPU family and TP size, on that family).".to_owned()
                } else {
                    format!("Flare is not serving anywhere (an experiment lane, off by default). Packages: {gb202} for sm120 (gb202), {gb10} for sm121 (gb10).")
                },
                Some(if gb202 + gb10 == 0 {
                    format!("{LAUNCHER} prepare-help")
                } else {
                    format!("{LAUNCHER} up <target> <package> [tp1|tp2|tp4]")
                }),
            ));
        }
        let health = if p.checking {
            Health::Unknown
        } else if !p.ran || !p.err.is_empty() {
            Health::Down
        } else if !serving.is_empty() {
            if problems.iter().any(|x| x.severity == Severity::Warn) {
                Health::Degraded
            } else {
                Health::Up
            }
        } else if starting.is_some() || p.targets.iter().any(|t| t.container == "running") {
            Health::Degraded
        } else {
            Health::Down
        };
        let summary = if p.checking {
            "checking every target (the first report is still running)".to_owned()
        } else if !p.ran {
            format!("{LAUNCHER} is not on PATH")
        } else if !serving.is_empty() {
            serving
                .iter()
                .map(|t| {
                    format!(
                        "serving on {} ({}): {}",
                        t.target,
                        t.chip,
                        t.model.as_deref().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ")
        } else if let Some(s) = &starting {
            format!("starting {} on {}", s.package, s.target)
        } else {
            let sm120 = p.targets.iter().filter(|t| t.chip == "sm120").count();
            let sm121 = p.targets.iter().filter(|t| t.chip == "sm121").count();
            format!("not serving · {sm120} sm120 target(s), {sm121} sm121 target(s)")
        };
        let endpoints = serving
            .iter()
            .map(|t| Endpoint {
                name: format!("{} API", t.target),
                url: format!("{}/v1", t.url),
                role: "api".into(),
                auth: "none".into(),
            })
            .collect();
        Ok(SystemStatus {
            system: SystemId::Flare,
            health,
            version: p
                .targets
                .iter()
                .find(|t| t.image == Some(true) && !t.build.is_empty())
                .map(|t| t.build.clone()),
            summary: pystr::head(&summary, 300).to_owned(),
            endpoints,
            components,
            problems,
            checked_at: now_ts(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let p = self.inner.probe(5.0).await;
        let mut points: Vec<MetricPoint> = Vec::new();
        let with_target = |key: &str, value: f64, unit: &str, target: &str| {
            let mut labels = IndexMap::new();
            labels.insert("target".to_owned(), target.to_owned());
            MetricPoint {
                key: key.into(),
                value,
                unit: unit.into(),
                labels,
            }
        };
        #[allow(clippy::cast_precision_loss)]
        points.push(MetricPoint::new(
            "flare.targets_serving",
            p.targets.iter().filter(|t| t.serving).count() as f64,
        ));
        for t in &p.targets {
            #[allow(clippy::cast_precision_loss)]
            points.push(with_target(
                "flare.packages",
                t.packages.len() as f64,
                "1",
                &t.target,
            ));
            let Some(m) = &t.metrics else { continue };
            for (field, key, unit) in [
                ("generated_tokens", "flare.generated_total", "tokens"),
                ("prefill_tokens", "flare.prefill_total", "tokens"),
                ("active", "flare.active_requests", "1"),
                ("pending", "flare.pending_requests", "1"),
                ("completed", "flare.requests_completed_total", "1"),
                ("failed", "flare.requests_failed_total", "1"),
                ("reused_tokens", "flare.cache_reused_total", "tokens"),
            ] {
                if let Some(v) = wsb::json_num(m.get(field)) {
                    points.push(with_target(key, v, unit, &t.target));
                }
            }
            if let (Some(acc), Some(drafts)) = (
                wsb::json_num(m.get("accepted_drafts")),
                wsb::json_num(m.get("verification_drafts")),
            ) && drafts > 0.0
            {
                points.push(with_target(
                    "flare.draft_acceptance",
                    acc / drafts,
                    "ratio",
                    &t.target,
                ));
            }
            if let Some(n) = t.max_model_len {
                points.push(with_target("flare.max_model_len", n, "tokens", &t.target));
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Flare,
            ts: now_ts(),
            available: p.ran && !p.checking,
            points,
            error: if !p.ran {
                Some(format!("{LAUNCHER} is not on PATH"))
            } else if p.checking {
                Some("the first report is still running".to_owned())
            } else {
                None
            },
        })
    }

    async fn recent_events(&self, _limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(Vec::new())
    }

    fn follow_events(self: Arc<Self>, _since_ts: Option<f64>) -> EventStream {
        Box::pin(futures::stream::empty())
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        if self.inner.dir().is_none() {
            return Ok(Vec::new());
        }
        FILES
            .iter()
            .map(|f| self.file(f.0).map(|(_, s)| s))
            .collect()
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        let (p, _) = self.file(file_id)?;
        // containment: the real file stays inside the lane directory (no symlink out of it)
        if let Some(d) = self.inner.dir() {
            let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            let root = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
            if !real.starts_with(&root) {
                return Err(AdapterError::UnknownId(file_id.to_owned()));
            }
        }
        Ok(p)
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.config_path(file_id)?;
        let (_, spec) = self.file(file_id)?;
        let nf = || AdapterError::FileNotFound(file_id.to_owned());
        let md = std::fs::symlink_metadata(&p).map_err(|_| nf())?;
        if !md.is_file() || md.len() > MAX_CONFIG_BYTES {
            return Err(nf());
        }
        let raw = std::fs::read(&p).map_err(|_| nf())?;
        Ok(ConfigFile {
            spec,
            content: String::from_utf8_lossy(&raw).into_owned(),
            sha256: sha256_hex(&raw),
            mtime: crate::services::config_store::mtime_f64(&md).unwrap_or(0.0),
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        let (_, spec) = self.file(file_id)?;
        let upper = spec.format.to_uppercase();
        let err_text = if spec.format == "toml" {
            match tomllib::loads(content) {
                Ok(_) => None,
                Err(e) if e.is_decode() => Some(e.to_string()),
                Err(e) => return Err(AdapterError::failed(e.class(), e.to_string())),
            }
        } else {
            match pyyaml::safe_load(content) {
                Ok(_) => None,
                Err(e) => match wsb::yaml_escaped(&e) {
                    Some(err) => return Err(err),
                    None => Some(e.text()),
                },
            }
        };
        let diagnostics = err_text
            .map(|t| Diagnostic {
                severity: Severity::Error,
                message: format!("{upper} syntax error: {}", pystr::head(&t, 300)),
                line: None,
            })
            .into_iter()
            .collect::<Vec<_>>();
        Ok(ValidationResult {
            ok: diagnostics.is_empty(),
            validator: format!("{upper} parse"),
            authoritative: false,
            diagnostics,
            note: Some(WHY_READONLY.to_owned()),
        })
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        self.file(file_id)?;
        Ok(ApplyResult::new(
            false,
            "read-only: the console never writes Flare's lane files",
        ))
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let targets: Vec<Target> = lock(&self.inner.cache)
            .as_ref()
            .map(|(_, p)| p.targets.clone())
            .unwrap_or_default();
        let known: Vec<String> = lock(&self.inner.cache)
            .as_ref()
            .map(|(_, p)| {
                let mut v: Vec<String> = p
                    .targets
                    .iter()
                    .flat_map(|t| t.packages.iter().cloned())
                    .collect();
                v.sort();
                v.dedup();
                v
            })
            .unwrap_or_default();
        let package = || {
            let mut p = ParamSpec::new(
                "package",
                "Package",
                "A prepared package file (*.safetensors) of the target's GPU family (gb202 for an sm120 target, gb10 for sm121). Its TP size must match the mode.",
                if known.is_empty() {
                    ParamType::String
                } else {
                    ParamType::Enum
                },
            );
            if !known.is_empty() {
                p.choices = Some(known.clone());
                p.default = json!(known[0]);
            }
            p.requires_restart = false;
            p
        };
        let mut lines = ParamSpec::new(
            "lines",
            "Lines",
            "Newest log lines per host (1-500).",
            ParamType::Int,
        );
        lines.default = json!(80);
        lines.min = Some(1.0);
        lines.max = Some(500.0);
        lines.requires_restart = false;
        let mut status = ActionSpec::new(
            "targets",
            "Show every target",
            "Every target with its chip (sm120 / sm121), package family, hosts, container, image, packages and whether its GPU is free. Read-only (the launcher looks over ssh).",
            Danger::Safe,
        );
        status.params = Vec::new();
        let mut preflight = ActionSpec::new(
            "preflight",
            "Check a start",
            "`flare-local preflight <target> <package> [mode]`: the package exists for that family, the image is loaded on every host, the GPU(s) are free and no Flare runs there. Changes nothing.",
            Danger::Safe,
        );
        preflight.params = vec![target_param(&targets), package(), mode_param()];
        let mut up = ActionSpec::new(
            "up",
            "Start Flare",
            "`flare-local up <target> <package> [mode]` in the background: refuses unless every GPU of the target is free (nothing is stopped for you), then starts the engine (for a multi-host target one rank per host over RoCE) and waits for /health. disruptive: it takes the GPU(s) for as long as it runs.",
            Danger::Disruptive,
        );
        up.params = vec![target_param(&targets), package(), mode_param()];
        let mut down = ActionSpec::new(
            "down",
            "Stop Flare",
            "`flare-local down <target>`: stops and removes Flare's container(s) on that target (every rank of a multi-host target) and frees the GPU(s). Requests in flight fail.",
            Danger::Disruptive,
        );
        down.params = vec![target_param(&targets)];
        let mut probe = ActionSpec::new(
            "probe",
            "Probe",
            "`flare-local probe <target>`: one real completion ('Reply with exactly: FLARE-OK'); /health alone is not proof. Spends a little GPU time.",
            Danger::Safe,
        );
        probe.params = vec![target_param(&targets)];
        let mut log = ActionSpec::new(
            "log",
            "Show the log",
            "The newest lines of Flare's container log on each host of the target.",
            Danger::Safe,
        );
        log.params = vec![target_param(&targets), lines];
        let mut stage = ActionSpec::new(
            "stage",
            "Stage a remote target",
            "`flare-local stage <target>` (remote targets): loads the serving image from the console host's share and seeds ~/flare there (trust dir, the io_uring seccomp profile, the PUBLIC publisher key, the serving key). No GPU; refuses if the image tarball is not on the share.",
            Danger::Disruptive,
        );
        stage.params = vec![target_param(&targets)];
        let last = ActionSpec::new(
            "last_start",
            "Show the last start",
            "The output of the last background start, or that it is still running.",
            Danger::Safe,
        );
        let help = ActionSpec::new(
            "prepare_help",
            "How to prepare a package",
            "`flare-local prepare-help`: the preparation sequence (Ignition, on the target's own GPU family) for the checkpoints on hand. Prints only.",
            Danger::Safe,
        );
        Ok(vec![
            status, preflight, up, down, probe, log, stage, last, help,
        ])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        let inner = &self.inner;
        let target = get_str(params, "target", "");
        // one start at a time: refuse a second before asking the launcher anything
        if action_id == "up"
            && let Some(s) = inner.starting()
        {
            return Ok(done(
                t0,
                false,
                &format!(
                    "A start is already running (package {} on {}); one at a time.",
                    s.package, s.target
                ),
                Vec::new(),
            ));
        }
        let needs_target = matches!(
            action_id,
            "preflight" | "up" | "down" | "probe" | "log" | "stage"
        );
        let mut chosen: Option<Target> = None;
        if needs_target {
            let p = inner.probe(30.0).await;
            if p.checking {
                return Ok(done(
                    t0,
                    false,
                    "Still checking the targets (the first report is running): try again in a few seconds.",
                    Vec::new(),
                ));
            }
            if !p.ran || !p.err.is_empty() {
                let why = if p.err.is_empty() {
                    format!("{LAUNCHER} is not on PATH")
                } else {
                    p.err
                };
                return Ok(done(
                    t0,
                    false,
                    &format!("{why}\nfix: {}", inner.install_fix()),
                    Vec::new(),
                ));
            }
            let Some(t) = p.targets.iter().find(|t| t.target == target).cloned() else {
                let names: Vec<&str> = p.targets.iter().map(|t| t.target.as_str()).collect();
                return Ok(done(
                    t0,
                    false,
                    &format!(
                        "unknown target {target:?}; the launcher knows: {}",
                        names.join(", ")
                    ),
                    Vec::new(),
                ));
            };
            chosen = Some(t);
        }
        let package = get_str(params, "package", "");
        let mode = get_str(params, "mode", "auto");
        if matches!(action_id, "preflight" | "up")
            && let Some(t) = &chosen
        {
            if !PKG_RE.is_match(&package) {
                return Ok(done(
                    t0,
                    false,
                    "name a package file (letters, digits, '.', '_' and '-'; no path)",
                    Vec::new(),
                ));
            }
            if !MODES.contains(&mode.as_str()) {
                return Ok(done(
                    t0,
                    false,
                    "mode must be auto, tp1, tp2 or tp4",
                    Vec::new(),
                ));
            }
            let offered: Vec<&str> = t.modes.split_whitespace().collect();
            if mode != "auto" && !offered.contains(&mode.as_str()) {
                return Ok(done(
                    t0,
                    false,
                    &format!(
                        "target {target} ({}) does not take {mode}: it takes {}",
                        t.chip,
                        offered.join(" ")
                    ),
                    Vec::new(),
                ));
            }
            if mode == "auto" && offered.len() > 1 {
                return Ok(done(
                    t0,
                    false,
                    &format!(
                        "target {target} takes {}: name the one the package was exported for",
                        offered.join(" or ")
                    ),
                    Vec::new(),
                ));
            }
        }
        let simple = |argv: Vec<String>, timeout: f64| async move {
            let r = inner.run(&argv, wsb::opts(timeout, 64_000)).await;
            inner.forget();
            let out = format!("{}\n{}", r.stdout, r.stderr);
            let mut text = pystr::strip(&out).to_owned();
            if r.returncode == 127 && r.stdout.is_empty() {
                text = format!("{text}\nfix: {}", inner.install_fix());
            }
            done(t0, r.returncode == 0, &text, Vec::new())
        };
        match action_id {
            "targets" => {
                let p = inner.probe_fresh(60.0).await;
                if p.checking {
                    return Ok(done(
                        t0,
                        false,
                        "Still checking the targets (the first report is running): try again in a few seconds.",
                        Vec::new(),
                    ));
                }
                if !p.ran || !p.err.is_empty() {
                    let why = if p.err.is_empty() {
                        format!("{LAUNCHER} is not on PATH")
                    } else {
                        p.err
                    };
                    return Ok(done(
                        t0,
                        false,
                        &format!("{why}\nfix: {}", inner.install_fix()),
                        Vec::new(),
                    ));
                }
                let mut lines: Vec<String> = Vec::new();
                for t in &p.targets {
                    lines.push(format!("{:<9} {}", t.target, describe(t)));
                    if !t.packages.is_empty() {
                        lines.push(format!("          packages: {}", t.packages.join(", ")));
                    }
                }
                Ok(done(t0, true, &lines.join("\n"), Vec::new()))
            }
            "preflight" => {
                let mut argv = wsb::argv(&[LAUNCHER, "preflight", &target, &package]);
                if mode != "auto" {
                    argv.push(mode.clone());
                }
                Ok(simple(argv, 120.0).await)
            }
            "up" => self.start_up(&target, &package, &mode, t0).await,
            "down" => Ok(simple(wsb::argv(&[LAUNCHER, "down", &target]), 300.0).await),
            "probe" => Ok(simple(wsb::argv(&[LAUNCHER, "probe", &target]), 150.0).await),
            "stage" => {
                if chosen
                    .as_ref()
                    .is_some_and(|t| t.hosts.iter().any(|h| h == "local"))
                {
                    return Ok(done(
                        t0,
                        false,
                        "a local target needs no staging: the image is built on this host (flare.sh build)",
                        Vec::new(),
                    ));
                }
                Ok(simple(wsb::argv(&[LAUNCHER, "stage", &target]), 600.0).await)
            }
            "log" => {
                let n = wsb::json_num(params.get("lines")).unwrap_or(80.0);
                if n.fract() != 0.0 || !(1.0..=500.0).contains(&n) {
                    return Ok(done(
                        t0,
                        false,
                        "lines must be a whole number from 1 to 500",
                        Vec::new(),
                    ));
                }
                #[allow(clippy::cast_possible_truncation)]
                let n = (n as i64).to_string();
                Ok(simple(wsb::argv(&[LAUNCHER, "log", &target, &n]), 60.0).await)
            }
            "last_start" => Ok(match self.last_start() {
                None => done(t0, true, "No start since the console started.", Vec::new()),
                Some(s) if s.finished.is_none() => done(
                    t0,
                    true,
                    &format!(
                        "Still starting {} on {} (started {:.0} s ago).",
                        s.package,
                        s.target,
                        now_ts() - s.started
                    ),
                    Vec::new(),
                ),
                Some(s) => done(
                    t0,
                    s.ok == Some(true),
                    &format!(
                        "{} {} on {}:\n{}",
                        if s.ok == Some(true) {
                            "Started"
                        } else {
                            "Failed to start"
                        },
                        s.package,
                        s.target,
                        s.output
                    ),
                    Vec::new(),
                ),
            }),
            "prepare_help" => Ok(simple(wsb::argv(&[LAUNCHER, "prepare-help"]), 30.0).await),
            _ => Err(AdapterError::UnknownId(action_id.to_owned())),
        }
    }

    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        Ok(Some(PlaygroundSpec {
            wire: "openai_chat".into(),
            models: vec!["flare".into()],
            default_model: "flare".into(),
            note: "Chats with Flare on the local target (127.0.0.1:8035 by default), the only one on this machine's loopback. Flare ignores the model name: the loaded package always answers. For remote targets use the Probe action, which runs through the launcher with their serving key. No key is sent from here.".into(),
        }))
    }

    fn playground_url(&self) -> Option<String> {
        Some(format!("{}/v1/chat/completions", self.inner.base_url()))
    }
}

/// Build the adapter (no configure hook).
pub fn build(ctx: &AdapterContext, _cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(FlareAdapter::new(ctx.clone())))
}
