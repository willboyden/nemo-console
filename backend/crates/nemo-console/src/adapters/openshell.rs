//! OpenShell adapter: the gateway that contains Data Designer runs (clients/data-designer/runner). OWNER: workstream B.
//! Port of `backend/src/nemo_console/adapters/openshell.py`.
//!
//! Facts this adapter is built on (CLI 0.0.71 + gateway 0.0.77, captured live 2026-10-24; tests/fixtures/openshell):
//!   * The CLI is `openshell`; everything else talks to the gateway over mTLS. Its human output carries ANSI escapes:
//!     every text is stripped before it is parsed or shown.
//!   * `openshell status` exits 0 even when no gateway is registered: ONLY the `Status:` line decides "connected".
//!   * `openshell sandbox list -o json` prints a JSON array of {id, name, phase, created_at (UTC), ...}.
//!   * `openshell policy prove` needs `--policy FILE` and `--credentials FILE`; the descriptor is derived from the
//!     lab's provider profile (no key is read). Exit 0 = no findings, 1 = findings (or an "Error:" line).
//!   * A stopped gateway is health "off" (data-designer-local starts it on demand). `gateway_up` mirrors
//!     ensure_gateway(); `gateway_stop` is `compose stop` (never `down -v`).
//!   * Trust files are READ-ONLY (ADR 0007): the console only shows and parses them.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
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
use crate::config::{OPENSHELL_CONTAINERS, Settings};
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, Problem,
    Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::pyfmt::str_repr;
use crate::services::pytoml as tomllib;
use crate::services::pyyaml::{self, DumpOpts, PyValue as Yv};
use crate::services::{plumbing as wsb, pystr};

/// `OpenShellAdapter.label` in Python.
pub const LABEL: &str = "OpenShell";
pub const CLI: &str = "openshell";
pub const DEFAULT_ENDPOINT: &str = "https://127.0.0.1:17670";
/// must be --internal (data-designer-local checks it too)
pub const SANDBOX_NET: &str = "openshell-docker";
pub const SANDBOX_SUBNET: &str = "172.16.0.0/24";
/// re-pinned on SANDBOX_NET by ensure_gateway()
pub const LITELLM_CONTAINER: &str = "ai-lab-litellm-litellm-1";
pub const LITELLM_IP: &str = "172.16.0.5";
pub const PROFILE_NAME: &str = "profile-lab-router.yaml";
pub const EVENT_KIND: &str = "openshell.sandbox";
pub const VERSION_TTL: f64 = 300.0;
pub const SNAPSHOT_TTL: f64 = 2.0;
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;
/// `sandbox list --limit`, and the bound on tracked sandbox state
pub const MAX_SANDBOXES: usize = 200;
/// events kept in memory
pub const RING: usize = 200;
/// bounded label set for os.sandboxes_by_phase
pub const PHASES: [&str; 5] = ["Provisioning", "Ready", "Error", "Deleting", "Other"];
pub const LOOPBACK: [&str; 3] = ["127.0.0.1", "::1", "localhost"];
/// = data-designer-local GATEWAY_TIMEOUT
pub const GATEWAY_READY_TIMEOUT: f64 = 90.0;

static NAME_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z").ok());
static SCOPE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[a-z][a-z0-9_.:-]{0,31}\z").ok());
static ANSI_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").ok());
static CTRL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").ok());
static VERSION_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\d+\.\d+\.\d+[0-9A-Za-z.+-]*").ok());
static KV_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(Gateway|Server|Status|Version):\s*(.*?)\s*$").ok());
static ERROR_LINE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*Error:").ok());
static FINDING_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?m)^\s*•").ok());
static LOOPBACK_URL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\Ahttps?://(127\.0\.0\.1|localhost|\[::1\]):\d{1,5}/?\z").ok());
/// `strptime("%Y-%m-%d %H:%M:%S")` / `"%Y-%m-%dT%H:%M:%S"` as CPython's _strptime compiles them (1-digit fields,
/// ` 1`-style days, any whitespace for the space, case-insensitive `T`).
static UTC_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?i)\A([0-9]{4})-(1[0-2]|0[1-9]|[1-9])-(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])\s+(2[0-3]|[0-1][0-9]|[0-9]):([0-5][0-9]|[0-9]):(6[0-1]|[0-5][0-9]|[0-9])\z").ok()
});
static ISO_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?i)\A([0-9]{4})-(1[0-2]|0[1-9]|[1-9])-(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])T(2[0-3]|[0-1][0-9]|[0-9]):([0-5][0-9]|[0-9]):(6[0-1]|[0-5][0-9]|[0-9])\z").ok()
});

fn is(re: &LazyLock<Option<Regex>>, s: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(s))
}

fn sub(re: &LazyLock<Option<Regex>>, s: &str, rep: &str) -> String {
    match re.as_ref() {
        Some(r) => r.replace_all(s, regex::NoExpand(rep)).into_owned(),
        None => s.to_owned(),
    }
}

/// ANSI escapes and control characters (except \t \n \r) removed.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    sub(&CTRL_RE, &sub(&ANSI_RE, text, ""), "")
}

fn epoch(caps: &regex::Captures<'_>) -> Option<f64> {
    let n = |i: usize| {
        caps.get(i)
            .map(|m| pystr::strip(m.as_str()))
            .and_then(|s| s.parse::<i64>().ok())
    };
    let (y, mo, d, h, mi, s) = (n(1)?, n(2)?, n(3)?, n(4)?, n(5)?, n(6)?);
    let month = time::Month::try_from(u8::try_from(mo).ok()?).ok()?;
    let date = time::Date::from_calendar_date(i32::try_from(y).ok()?, month, u8::try_from(d).ok()?)
        .ok()?;
    let t = time::Time::from_hms(
        u8::try_from(h).ok()?,
        u8::try_from(mi).ok()?,
        u8::try_from(s).ok()?,
    )
    .ok()?;
    #[allow(clippy::cast_precision_loss)]
    let ts = time::PrimitiveDateTime::new(date, t)
        .assume_utc()
        .unix_timestamp() as f64;
    Some(ts)
}

/// created_at: "YYYY-MM-DD HH:MM:SS" in UTC -> epoch seconds.
fn utc(stamp: Option<&Value>) -> Option<f64> {
    let Some(Value::String(s)) = stamp else {
        return None;
    };
    let head = pystr::head(pystr::strip(s), 19);
    UTC_RE.as_ref()?.captures(head).as_ref().and_then(epoch)
}

/// Docker StartedAt ("2026-10-24T07:47:51.919656462Z") -> epoch seconds; the zero time -> None.
fn iso(stamp: &str) -> Option<f64> {
    let t = ISO_RE
        .as_ref()?
        .captures(pystr::head(stamp, 19))
        .as_ref()
        .and_then(epoch)?;
    (t > 0.0).then_some(t)
}

/// The phase as one of the bounded label values.
#[must_use]
pub fn phase_bucket(phase: &str) -> String {
    let p = pystr::strip(phase).to_lowercase();
    PHASES[..4]
        .iter()
        .find(|k| k.to_lowercase() == p)
        .map_or("Other", |k| k)
        .to_owned()
}

/// repo@sha256:<64 hex> -> repo@sha256:<12 hex> (display).
fn short_image(image: &str) -> String {
    match image.split_once("@sha256:") {
        Some((repo, digest)) => format!("{repo}@sha256:{}", pystr::head(digest, 12)),
        None => pystr::head(image, 200).to_owned(),
    }
}

/// The gateway URL as shown: the CLI's own Server line when it is a loopback https URL, else the default.
fn loopback(server: &str) -> String {
    let s = pystr::strip(server);
    if is(&LOOPBACK_URL_RE, s) {
        s.trim_end_matches('/').to_owned()
    } else {
        DEFAULT_ENDPOINT.to_owned()
    }
}

/// Python `int(s)` for the docker fields (whitespace, sign, single underscores).
fn py_int(s: &str) -> Option<i64> {
    let t = pystr::strip(s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty()
        || body.starts_with('_')
        || body.ends_with('_')
        || body.contains("__")
        || !body.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let v: i64 = body.replace('_', "").parse().ok()?;
    Some(if neg { -v } else { v })
}

/// `_int_param`: an int (not a bool) within [lo, hi].
fn int_param(v: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().filter(|i| lo <= *i && *i <= hi),
        _ => None,
    }
}

/// `path.resolve()` (strict=False): symlinks resolved as far as the path exists.
fn resolve(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => resolve(parent).join(name),
        _ => p.to_path_buf(),
    }
}

#[derive(Debug, Clone)]
struct Ctr {
    name: String,
    /// docker State.Status
    state: String,
    exit_code: i64,
    oom: bool,
    started: String,
    restarts: i64,
    image: String,
    host_ips: Vec<String>,
}

impl Ctr {
    fn running(&self) -> bool {
        self.state == "running"
    }

    /// Crashed, not stopped: `docker stop` exits 0, 137 (SIGKILL after the grace) or 143 (SIGTERM).
    fn failed(&self) -> bool {
        self.state == "restarting"
            || self.state == "dead"
            || self.oom
            || (self.state == "exited" && ![0, 137, 143].contains(&self.exit_code))
    }
}

#[derive(Debug, Clone)]
struct Conn {
    /// the `openshell status` command itself ran
    ran: bool,
    connected: bool,
    /// the Status: line (or the reason it is missing)
    text: String,
    server: String,
    version: Option<String>,
}

impl Conn {
    fn none(text: &str) -> Conn {
        Conn {
            ran: false,
            connected: false,
            text: text.to_owned(),
            server: String::new(),
            version: None,
        }
    }
}

#[derive(Debug, Clone)]
struct Snap {
    ts: Option<Instant>,
    ok: bool,
    rows: Vec<Map<String, Value>>,
    error: String,
}

impl Snap {
    fn empty(error: &str) -> Snap {
        Snap {
            ts: None,
            ok: false,
            rows: Vec::new(),
            error: error.to_owned(),
        }
    }
}

/// Intervals the tests shorten.
#[derive(Debug, Clone, Copy)]
pub struct OpenShellTiming {
    pub follow_interval: f64,
    /// gateway_up readiness poll
    pub poll_interval: f64,
    pub ready_timeout: f64,
}

impl Default for OpenShellTiming {
    fn default() -> Self {
        OpenShellTiming {
            follow_interval: 3.0,
            poll_interval: 3.0,
            ready_timeout: GATEWAY_READY_TIMEOUT,
        }
    }
}

struct St {
    version_cache: Option<(Instant, Option<String>)>,
    /// name -> (id, phase); bounded by MAX_SANDBOXES
    known: IndexMap<String, (String, String)>,
    baselined: bool,
    gw_cache: Option<(Instant, bool)>,
    ring: VecDeque<(u64, SystemEvent)>,
    seq: u64,
}

pub struct OpenShellAdapter {
    ctx: AdapterContext,
    /// The gateway is the first name containing "gateway" (else the first); the others are CLI forwarders.
    containers: Mutex<Vec<String>>,
    /// the gateway registration the CLI is pinned to (`-g`); never the "active" one
    gateway_name: String,
    timing: Mutex<OpenShellTiming>,
    st: Mutex<St>,
    snap: tokio::sync::Mutex<Snap>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn done(t0: Instant, ok: bool, out: &str, problems: Vec<Problem>) -> ActionResult {
    ActionResult {
        ok,
        output: pystr::head(&redact(&strip_ansi(out)), 20_000).to_owned(),
        duration_ms: i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX),
        problems,
    }
}

/// `(r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip()`
fn combined(r: &CommandResult) -> String {
    let mut s = r.stdout.clone();
    if !pystr::strip(&r.stderr).is_empty() {
        s.push('\n');
        s.push_str(&r.stderr);
    }
    pystr::strip(&s).to_owned()
}

fn row_str(r: &Map<String, Value>, k: &str, default: &str) -> String {
    r.get(k).map_or_else(|| default.to_owned(), wsb::json_str)
}

impl OpenShellAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        OpenShellAdapter {
            ctx,
            containers: Mutex::new(
                OPENSHELL_CONTAINERS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ),
            gateway_name: "openshell".to_owned(),
            timing: Mutex::new(OpenShellTiming::default()),
            st: Mutex::new(St {
                version_cache: None,
                known: IndexMap::new(),
                baselined: false,
                gw_cache: None,
                ring: VecDeque::new(),
                seq: 0,
            }),
            snap: tokio::sync::Mutex::new(Snap::empty("not polled")),
        }
    }

    /// What the container's registry builds (A-7): `OpenShellAdapter(ctx)` with the configured containers.
    #[must_use]
    pub fn from_settings(ctx: &AdapterContext, cfg: &Settings) -> Self {
        let a = OpenShellAdapter::new(ctx.clone());
        a.set_containers(&cfg.openshell_containers);
        a
    }

    /// `adapter.containers = tuple(cfg.openshell_containers)`.
    pub fn set_containers(&self, names: &[String]) {
        *lock(&self.containers) = names.to_vec();
    }

    #[must_use]
    pub fn containers(&self) -> Vec<String> {
        lock(&self.containers).clone()
    }

    pub fn set_timing(&self, t: OpenShellTiming) {
        *lock(&self.timing) = t;
    }

    fn timing(&self) -> OpenShellTiming {
        *lock(&self.timing)
    }

    #[must_use]
    pub fn ctx(&self) -> &AdapterContext {
        &self.ctx
    }

    /// Forget the cached sandbox list (Python tests set `_snap.ts = 0.0`).
    pub async fn invalidate_snapshot(&self) {
        self.snap.lock().await.ts = None;
    }

    /// Forget the cached gateway state (Python tests set `_gw_cache = (0.0, False)`).
    pub fn invalidate_gw_cache(&self) {
        lock(&self.st).gw_cache = None;
    }

    /// Events held in memory (`len(_ring)`).
    #[must_use]
    pub fn ring_len(&self) -> usize {
        lock(&self.st).ring.len()
    }

    /// Sandboxes tracked (`len(_known)`).
    #[must_use]
    pub fn known_len(&self) -> usize {
        lock(&self.st).known.len()
    }

    fn gateway_ctr(&self) -> String {
        let c = self.containers();
        c.iter()
            .find(|n| n.contains("gateway"))
            .or_else(|| c.first())
            .cloned()
            .unwrap_or_default()
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        self.ctx.paths.get(key).cloned()
    }

    fn runner_dir(&self) -> Option<PathBuf> {
        self.path("openshell_runner_dir")
    }

    fn compose(&self) -> Option<PathBuf> {
        self.path("openshell_compose")
    }

    fn start_fix(&self) -> String {
        match self.compose() {
            Some(c) => format!(
                "docker compose -f {} up -d   # or run any data-designer-local command: it starts the gateway",
                c.display()
            ),
            None => {
                "run any data-designer-local command: it starts the gateway on demand".to_owned()
            }
        }
    }

    async fn run(&self, argv: &[String], opts: RunOpts) -> CommandResult {
        wsb::run(&self.ctx, argv, opts).await
    }

    async fn cli(&self, args: &[&str], timeout: f64, max_bytes: usize) -> CommandResult {
        let mut argv = wsb::argv(&[CLI, "-g", &self.gateway_name]);
        argv.extend(args.iter().map(|s| (*s).to_owned()));
        self.run(&argv, wsb::opts(timeout, max_bytes)).await
    }

    async fn cli_version(&self) -> Option<String> {
        if let Some((ts, Some(v))) = &lock(&self.st).version_cache
            && ts.elapsed() < Duration::from_secs_f64(VERSION_TTL)
        {
            return Some(v.clone());
        }
        let r = self
            .run(&wsb::argv(&[CLI, "--version"]), wsb::opts(15.0, 4096))
            .await;
        let version = if r.returncode == 0 {
            VERSION_RE.as_ref().and_then(|re| {
                re.find(&strip_ansi(&r.stdout))
                    .map(|m| m.as_str().to_owned())
            })
        } else {
            None
        };
        lock(&self.st).version_cache = Some((Instant::now(), version.clone()));
        version
    }

    // ── probes ──────────────────────────────────────────────────────────────────────────────
    /// (containers found by name, docker answered).
    async fn inspect(&self) -> (IndexMap<String, Ctr>, bool) {
        let containers = self.containers();
        if containers.is_empty() {
            return (IndexMap::new(), true);
        }
        let fmt = "{{.Name}}|{{.State.Status}}|{{.State.ExitCode}}|{{.State.OOMKilled}}|{{.State.StartedAt}}|{{.RestartCount}}|{{.Config.Image}}|{{json .NetworkSettings.Ports}}";
        let mut argv = wsb::argv(&["docker", "inspect", "--format", fmt]);
        argv.extend(containers);
        let r = self.run(&argv, wsb::opts(15.0, 64_000)).await;
        let mut found: IndexMap<String, Ctr> = IndexMap::new();
        for line in pystr::splitlines(&r.stdout) {
            let parts: Vec<&str> = line.splitn(8, '|').collect();
            if parts.len() != 8 {
                continue;
            }
            let name = parts[0].trim_start_matches('/').to_owned();
            let Ok(ports) = serde_json::from_str::<Value>(parts[7]) else {
                continue;
            };
            let (Some(code), Some(restarts)) = (py_int(parts[2]), py_int(parts[5])) else {
                continue;
            };
            let mut ips = Vec::new();
            if let Value::Object(m) = &ports {
                for bs in m.values() {
                    if let Value::Array(list) = bs {
                        for b in list {
                            if let Value::Object(bm) = b {
                                ips.push(bm.get("HostIp").map_or_else(String::new, wsb::json_str));
                            }
                        }
                    }
                }
            }
            found.insert(
                name.clone(),
                Ctr {
                    name,
                    state: parts[1].to_owned(),
                    exit_code: code,
                    oom: parts[3] == "true",
                    started: parts[4].to_owned(),
                    restarts,
                    image: parts[6].to_owned(),
                    host_ips: ips,
                },
            );
        }
        // docker answered when it listed something or said "no such object"; anything else (daemon down, rc 127) did not
        let answered = !found.is_empty() || r.stderr.to_lowercase().contains("no such");
        (found, answered)
    }

    /// True / False from `docker network inspect`; None when the network is missing or docker did not answer.
    async fn network_internal(&self) -> Option<bool> {
        let r = self
            .run(
                &wsb::argv(&[
                    "docker",
                    "network",
                    "inspect",
                    SANDBOX_NET,
                    "--format",
                    "{{.Internal}}",
                ]),
                wsb::opts(15.0, 4096),
            )
            .await;
        let out = pystr::strip(&r.stdout).to_lowercase();
        match (r.returncode, out.as_str()) {
            (0, "true") => Some(true),
            (0, "false") => Some(false),
            _ => None,
        }
    }

    async fn connection(&self) -> Conn {
        let r = self.cli(&["status"], 15.0, 16_000).await;
        let text = strip_ansi(&format!("{}\n{}", r.stdout, r.stderr));
        let mut kv: HashMap<String, String> = HashMap::new();
        if let Some(re) = KV_RE.as_ref() {
            for c in re.captures_iter(&text) {
                kv.insert(
                    c[1].to_owned(),
                    c.get(2).map_or("", |m| m.as_str()).to_owned(),
                );
            }
        }
        let status = kv.get("Status").cloned().unwrap_or_default();
        let version = VERSION_RE.as_ref().and_then(|re| {
            re.find(kv.get("Version").map_or("", String::as_str))
                .map(|m| m.as_str().to_owned())
        });
        if r.returncode == 127 || (status.is_empty() && pystr::strip(&text).is_empty()) {
            return Conn::none("the CLI did not run");
        }
        if status.is_empty() {
            let first = pystr::splitlines(pystr::strip(&text))
                .first()
                .map_or("no status line", |l| l)
                .to_owned();
            return Conn {
                ran: true,
                connected: false,
                text: pystr::head(&first, 160).to_owned(),
                server: String::new(),
                version: None,
            };
        }
        Conn {
            ran: true,
            connected: status.to_lowercase().starts_with("connected"),
            text: pystr::head(&status, 160).to_owned(),
            server: pystr::head(kv.get("Server").map_or("", String::as_str), 200).to_owned(),
            version,
        }
    }

    /// `sandbox list -o json`, coalesced: concurrent callers share one subprocess and a max_age-second cache.
    async fn sandboxes(&self, max_age: f64) -> Snap {
        let mut snap = self.snap.lock().await;
        if let Some(ts) = snap.ts
            && ts.elapsed().as_secs_f64() < max_age
        {
            return snap.clone();
        }
        let limit = MAX_SANDBOXES.to_string();
        let r = self
            .cli(
                &["sandbox", "list", "--limit", &limit, "-o", "json"],
                20.0,
                256_000,
            )
            .await;
        let mut rows: Vec<Map<String, Value>> = Vec::new();
        let (mut ok, mut error) = (false, String::new());
        if r.returncode == 0 {
            let text = strip_ansi(&r.stdout);
            match serde_json::from_str::<Value>(if text.is_empty() { "[]" } else { &text }) {
                Ok(data) => {
                    let data = match data {
                        Value::Object(m) => match m.get("sandboxes") {
                            Some(Value::Array(a)) => Value::Array(a.clone()),
                            _ => Value::Array(Vec::new()),
                        },
                        other => other,
                    };
                    if let Value::Array(items) = data {
                        for e in items.iter().take(MAX_SANDBOXES) {
                            if let Value::Object(row) = e
                                && let Some(Value::String(n)) = row.get("name")
                                && is(&NAME_RE, n)
                            {
                                rows.push(row.clone());
                            }
                        }
                        ok = true;
                    } else {
                        "unexpected JSON shape".clone_into(&mut error);
                    }
                }
                Err(_) => "the list was not JSON".clone_into(&mut error),
            }
        } else {
            let src = if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            };
            error = pystr::head(&redact(pystr::strip(&strip_ansi(src))), 200).to_owned();
            if error.is_empty() {
                error = format!("exit {}", r.returncode);
            }
        }
        *snap = Snap {
            ts: Some(Instant::now()),
            ok,
            rows,
            error,
        };
        if ok {
            self.track(&snap.rows);
        }
        snap.clone()
    }

    // ── observe ─────────────────────────────────────────────────────────────────────────────
    /// Hygiene derived from the read-only gateway.toml (the fix is a by-hand edit: ADR 0007).
    fn config_problems(&self) -> Result<Vec<Problem>, AdapterError> {
        let Some(p) = self.path("openshell_gateway_config") else {
            return Ok(Vec::new());
        };
        let unreadable = || {
            vec![Problem::new(
                Severity::Warn,
                "openshell.config.unreadable",
                format!(
                    "{} is missing or is not valid TOML; the gateway cannot start from it.",
                    p.display()
                ),
                Some(format!(
                    "restore {} from the lab repository (git -C <lab repo> checkout -- {})",
                    p.display(),
                    p.display()
                )),
            )]
        };
        let Ok(md) = std::fs::metadata(&p) else {
            return Ok(unreadable());
        };
        if md.len() > MAX_CONFIG_BYTES {
            return Ok(Vec::new());
        }
        let Ok(Ok(text)) = std::fs::read(&p).map(String::from_utf8) else {
            return Ok(unreadable());
        };
        let doc = match tomllib::loads(&text) {
            Ok(d) => tomllib::TomlVal::Table(d),
            Err(e) if e.is_decode() => return Ok(unreadable()),
            Err(e) => return Err(AdapterError::failed(e.class(), e.to_string())),
        };
        let docker = doc
            .get("openshell")
            .and_then(|o| o.get("drivers"))
            .and_then(|d| d.get("docker"));
        let docker = match docker {
            Some(d @ tomllib::TomlVal::Table(_)) => Some(d),
            _ => None,
        };
        let mut out = Vec::new();
        if docker.and_then(|d| d.get("image_pull_policy"))
            != Some(&tomllib::TomlVal::Str("Never".to_owned()))
        {
            out.push(Problem::new(
                Severity::Warn,
                "openshell.config.pull_policy",
                "gateway.toml does not set image_pull_policy = \"Never\": the Docker daemon could pull images at runtime (egress outside the lab allowlist).",
                Some(format!("set image_pull_policy = \"Never\" under [openshell.drivers.docker] in {}", p.display())),
            ));
        }
        let sup = docker
            .and_then(|d| d.get("supervisor_image"))
            .map_or_else(String::new, tomllib::TomlVal::py_str);
        if !sup.contains("@sha256:") {
            out.push(Problem::new(
                Severity::Warn,
                "openshell.config.supervisor_unpinned",
                "The sandbox supervisor image is not pinned by digest in gateway.toml.",
                Some(format!(
                    "pin supervisor_image to <image>@sha256:<digest> in {}",
                    p.display()
                )),
            ));
        }
        Ok(out)
    }

    // ── events: diffs of the sandbox list ───────────────────────────────────────────────────
    fn emit(st: &mut St, ts: f64, summary: &str, data: Value) {
        st.seq += 1;
        let seq = st.seq;
        st.ring.push_back((
            seq,
            SystemEvent {
                ts,
                system: SystemId::Openshell,
                kind: EVENT_KIND.to_owned(),
                summary: pystr::head(&redact(summary), 300).to_owned(),
                data: match data {
                    Value::Object(m) => m,
                    _ => Map::new(),
                },
            },
        ));
        while st.ring.len() > RING {
            st.ring.pop_front();
        }
    }

    /// Compare a SUCCESSFUL list with the last one (a failed list never reaches here, so an unreachable gateway
    /// cannot look like "every sandbox was deleted").
    fn track(&self, rows: &[Map<String, Value>]) {
        let now = now_ts();
        let mut current: IndexMap<String, (String, String)> = IndexMap::new();
        let mut by_name: HashMap<String, &Map<String, Value>> = HashMap::new();
        for row in rows {
            let name = row.get("name").map_or_else(String::new, wsb::json_str);
            let id =
                pystr::head(&row.get("id").map_or_else(String::new, wsb::json_str), 64).to_owned();
            let phase = pystr::head(
                &row.get("phase").map_or_else(String::new, wsb::json_str),
                24,
            )
            .to_owned();
            current.insert(name.clone(), (id, phase));
            by_name.insert(name, row);
        }
        let mut st = lock(&self.st);
        let first = !st.baselined;
        let known = st.known.clone();
        for (name, (sid, phase)) in &current {
            let old = known.get(name);
            let created = utc(by_name.get(name).and_then(|r| r.get("created_at")))
                .filter(|t| *t != 0.0)
                .unwrap_or(now);
            match old {
                Some((oid, _)) if oid == sid => {
                    if let Some((_, ophase)) = old
                        && ophase != phase
                    {
                        Self::emit(
                            &mut st,
                            now,
                            &format!("sandbox {name}: {ophase} -> {phase}"),
                            json!({"name": name, "id": sid, "phase": phase, "action": "phase", "previous_phase": ophase}),
                        );
                    }
                }
                _ => {
                    let action = if first { "present" } else { "created" };
                    Self::emit(
                        &mut st,
                        if first { created } else { now },
                        &format!("sandbox {name} {action} ({phase})"),
                        json!({"name": name, "id": sid, "phase": phase, "action": action, "previous_phase": null}),
                    );
                }
            }
        }
        for (name, (sid, phase)) in &known {
            if !current.contains_key(name) {
                Self::emit(
                    &mut st,
                    now,
                    &format!("sandbox {name} deleted (was {phase})"),
                    json!({"name": name, "id": sid, "phase": phase, "action": "deleted", "previous_phase": phase}),
                );
            }
        }
        st.known = current;
        st.baselined = true;
    }

    async fn observe(&self) {
        if self.gateway_running().await {
            self.sandboxes(SNAPSHOT_TTL).await;
        }
    }

    /// Cached for 10 s so the events poll does not run `docker inspect` every few seconds.
    pub async fn gateway_running(&self) -> bool {
        if let Some((ts, running)) = lock(&self.st).gw_cache
            && ts.elapsed() < Duration::from_secs(10)
        {
            return running;
        }
        let (ctrs, docker_ok) = self.inspect().await;
        let running = ctrs
            .get(&self.gateway_ctr())
            .map_or(!docker_ok, Ctr::running);
        lock(&self.st).gw_cache = Some((Instant::now(), running));
        running
    }

    // ── configure: trust files are READ-ONLY (ADR 0007) ─────────────────────────────────────
    /// id -> (label, path, format, description) for every configured trust file.
    fn files(
        &self,
    ) -> Vec<(
        &'static str,
        &'static str,
        PathBuf,
        &'static str,
        &'static str,
    )> {
        let mut out = Vec::new();
        if let Some(g) = self.path("openshell_gateway_config") {
            out.push((
                "gateway",
                "OpenShell gateway config (gateway.toml)",
                g,
                "toml",
                "Schema-v1 gateway config: compute driver, host_gateway_ip, the digest-pinned supervisor image and image_pull_policy = \"Never\".",
            ));
        }
        if let Some(p) = self.path("openshell_policy") {
            out.push((
                "policy",
                "Sandbox policy (policy.yaml)",
                p,
                "yaml",
                "The Landlock filesystem allowlist and the default-deny network policy attached to every Data Designer sandbox at create time.",
            ));
        }
        if let Some(rd) = self.runner_dir() {
            out.push((
                "provider-profile",
                "Provider profile (profile-lab-router.yaml)",
                rd.join(PROFILE_NAME),
                "yaml",
                "Declares the router credential by ENV NAME only, its endpoint binding and the binaries that may use it. The key itself lives in the gateway's store.",
            ));
        }
        if let Some(c) = self.compose() {
            out.push((
                "compose",
                "Gateway compose stack (docker-compose.yml)",
                c,
                "yaml",
                "The gateway and CLI-forwarder containers: cap_drop, no-new-privileges, the 127.0.0.1-only published port and the internal sandbox network.",
            ));
        }
        out
    }

    fn display(&self, p: &Path) -> String {
        let home = self
            .ctx
            .environ
            .get("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        match home.and_then(|h| p.strip_prefix(&h).ok().map(Path::to_path_buf)) {
            Some(rel) => format!("~/{}", rel.display()),
            None => p.display().to_string(),
        }
    }

    fn spec(&self, id: &str, label: &str, p: &Path, fmt: &str, desc: &str) -> ConfigFileSpec {
        ConfigFileSpec {
            id: id.to_owned(),
            label: label.to_owned(),
            description: desc.to_owned(),
            display_path: self.display(p),
            format: fmt.to_owned(),
            editable: false,
            why_readonly: Some("Trust and containment policy for the sandboxes that run untrusted dataset scripts. Weakening it is a security decision (ADR 0007), so the console only shows it; edit the file by hand in the lab repository, review the diff, and use the prove_policy action to check the result.".to_owned()),
            validators: vec![if fmt == "toml" { "TOML parse" } else { "YAML parse" }.to_owned()],
            authoritative: false,
            apply_effect: "read-only: nothing to apply".to_owned(),
        }
    }

    fn unreachable_problem(&self) -> Problem {
        Problem::new(
            Severity::Warn,
            "openshell.gateway.unreachable",
            "The gateway did not answer the CLI (it is probably stopped).",
            Some(self.start_fix()),
        )
    }

    /// (name, error): the name must look like a sandbox name AND be in the live list.
    async fn live_name(&self, params: &Map<String, Value>) -> Result<String, String> {
        let name = match params.get("name") {
            Some(Value::String(n)) if is(&NAME_RE, n) => n.clone(),
            _ => return Err("name must be a sandbox name (letters, digits and . _ -, starting with a letter or digit)".to_owned()),
        };
        let snap = self.sandboxes(0.0).await;
        if !snap.ok {
            return Err(format!("The sandbox list is unavailable: {}", snap.error));
        }
        let names: Vec<String> = snap
            .rows
            .iter()
            .map(|r| row_str(r, "name", "None"))
            .collect();
        if !names.contains(&name) {
            let live = names
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "no sandbox named {} (live: {})",
                str_repr(&name),
                if live.is_empty() { "none" } else { &live }
            ));
        }
        Ok(name)
    }

    /// The prover's credential descriptor derived from the provider profile: (yaml text) or the error.
    fn descriptor(&self, scopes: &[String]) -> Result<Result<String, String>, AdapterError> {
        let Some(rd) = self.runner_dir() else {
            return Ok(Err("no OpenShell runner directory is configured".to_owned()));
        };
        let prof = rd.join(PROFILE_NAME);
        if prof.is_symlink() {
            return Ok(Err(format!("{} is a symlink or too large", prof.display())));
        }
        let missing = || {
            Ok(Err(format!(
                "{} is missing or is not valid YAML",
                prof.display()
            )))
        };
        let Ok(md) = std::fs::metadata(&prof) else {
            return missing();
        };
        if md.len() > MAX_CONFIG_BYTES {
            return Ok(Err(format!("{} is a symlink or too large", prof.display())));
        }
        let Ok(bytes) = std::fs::read(&prof) else {
            return missing();
        };
        let text = String::from_utf8(bytes).map_err(|_| {
            AdapterError::failed(
                "UnicodeDecodeError",
                "'utf-8' codec can't decode the provider profile",
            )
        })?;
        let data = match pyyaml::safe_load_value(&text) {
            Ok(d) => d,
            Err(e) => match wsb::yaml_escaped(&e) {
                Some(err) => return Err(err),
                None => return missing(),
            },
        };
        let mut hosts: Vec<Yv> = Vec::new();
        if let Some(Yv::List(endpoints)) = data.get("endpoints") {
            for e in endpoints {
                if let Yv::Dict(_) = e
                    && let Some(h) = e.get("host").filter(|h| wsb::yaml_truthy(h))
                {
                    hosts.push(Yv::Str(h.py_str()));
                }
            }
        }
        let mut creds: Vec<Yv> = Vec::new();
        if let Some(Yv::List(cs)) = data.get("credentials") {
            for c in cs {
                let c = if matches!(c, Yv::Dict(_)) {
                    c.clone()
                } else {
                    Yv::Dict(Vec::new())
                };
                let envs: Vec<String> = match c.get("env_vars") {
                    Some(Yv::List(v)) => v.iter().map(Yv::py_str).collect(),
                    _ => Vec::new(),
                };
                if !c.get("name").is_some_and(wsb::yaml_truthy) || envs.is_empty() {
                    continue;
                }
                let auth = c
                    .get("auth_style")
                    .filter(|a| wsb::yaml_truthy(a))
                    .map_or_else(|| "bearer".to_owned(), Yv::py_str);
                let s = |x: &str| Yv::Str(x.to_owned());
                creds.push(Yv::Dict(vec![
                    (
                        s("name"),
                        Yv::Str(c.get("name").map(Yv::py_str).unwrap_or_default()),
                    ),
                    (s("type"), Yv::Str(auth)),
                    (
                        s("scopes"),
                        Yv::List(scopes.iter().map(|x| Yv::Str(x.clone())).collect()),
                    ),
                    (s("injected_via"), Yv::Str(envs[0].clone())),
                    (s("target_hosts"), Yv::List(hosts.clone())),
                ]));
            }
        }
        if creds.is_empty() {
            return Ok(Err(format!(
                "{} declares no credential with env_vars",
                prof.display()
            )));
        }
        Ok(Ok(pyyaml::safe_dump(
            &Yv::Dict(vec![(Yv::Str("credentials".into()), Yv::List(creds))]),
            &DumpOpts::SAFE_DUMP_UNSORTED,
        )))
    }

    async fn prove(
        &self,
        params: &Map<String, Value>,
        t0: Instant,
    ) -> Result<ActionResult, AdapterError> {
        let policy = self.path("openshell_policy");
        let Some(policy) = policy.filter(|p| p.is_file()) else {
            let shown = self
                .path("openshell_policy")
                .map_or_else(|| "None".to_owned(), |p| p.display().to_string());
            return Ok(done(
                t0,
                false,
                &format!("policy file not found: {shown}"),
                Vec::new(),
            ));
        };
        let raw = match params.get("scopes") {
            Some(v) if wsb::json_truthy(Some(v)) => wsb::json_str(v),
            _ => String::new(),
        };
        let scopes: Vec<String> = raw
            .split(',')
            .map(|s| pystr::strip(s).to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if scopes.len() > 16 || scopes.iter().any(|s| !is(&SCOPE_RE, s)) {
            return Ok(done(
                t0,
                false,
                "scopes must be a comma-separated list of lowercase words (letters, digits, . _ : -)",
                Vec::new(),
            ));
        }
        let compact = params.get("compact").cloned().unwrap_or(Value::Bool(true));
        let text = match self.descriptor(&scopes)? {
            Ok(t) => t,
            Err(e) => return Ok(done(t0, false, &e, Vec::new())),
        };
        let tmp_dir = self.ctx.state_dir.join("openshell").join("tmp");
        wsb::mkdir_leaf(&tmp_dir, 0o700)
            .map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let mut rnd = [0u8; 6];
        getrandom::fill(&mut rnd).map_err(|e| AdapterError::failed("OSError", e.to_string()))?;
        let tmp = tmp_dir.join(format!("credentials.{}.yaml", hex::encode(rnd)));
        let written = (|| -> std::io::Result<()> {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(text.as_bytes())
        })();
        let r = match written {
            Ok(()) => {
                let mut argv = wsb::argv(&[
                    CLI,
                    "policy",
                    "prove",
                    "--policy",
                    &policy.display().to_string(),
                    "--credentials",
                    &tmp.display().to_string(),
                ]);
                if compact != Value::Bool(false) {
                    argv.push("--compact".into());
                }
                Ok(self.run(&argv, wsb::opts(60.0, 256_000)).await)
            }
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_file(&tmp);
        let r = r.map_err(|e| {
            AdapterError::failed(
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "FileExistsError"
                } else {
                    "OSError"
                },
                e.to_string(),
            )
        })?;
        let out = pystr::strip(&strip_ansi(&{
            let mut s = r.stdout.clone();
            if !pystr::strip(&r.stderr).is_empty() {
                s.push('\n');
                s.push_str(&r.stderr);
            }
            s
        }))
        .to_owned();
        let policy_name = policy
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let header = format!(
            "Policy: {policy_name} · derived credential descriptor ({PROFILE_NAME}; scopes: {})\n\n",
            if scopes.is_empty() {
                "none".to_owned()
            } else {
                scopes.join(", ")
            }
        );
        if r.timed_out || r.returncode == 127 || is(&ERROR_LINE_RE, &out) {
            return Ok(done(t0, false, &format!("{header}{out}"), Vec::new()));
        }
        if r.returncode == 0 {
            return Ok(done(t0, true, &format!("{header}{out}"), Vec::new()));
        }
        let n = FINDING_RE
            .as_ref()
            .map_or(0, |re| re.find_iter(&out).count());
        Ok(done(
            t0,
            true,
            &format!("{header}{out}"),
            vec![Problem::new(
                Severity::Warn,
                "openshell.policy.findings",
                format!(
                    "The prover reports {} finding(s) for human review (exit {}).",
                    if n == 0 {
                        "some".to_owned()
                    } else {
                        n.to_string()
                    },
                    r.returncode
                ),
                Some(
                    "read them, then change policy.yaml by hand if a finding is not intended"
                        .to_owned(),
                ),
            )],
        ))
    }

    /// Re-pin the router at LITELLM_IP on openshell-docker, as ensure_gateway() does.
    async fn pin_router(&self) -> String {
        let insp = self
            .run(
                &wsb::argv(&[
                    "docker",
                    "inspect",
                    LITELLM_CONTAINER,
                    "--format",
                    "{{range $k,$v := .NetworkSettings.Networks}}{{$k}}={{$v.IPAddress}} {{end}}",
                ]),
                wsb::opts(30.0, 16_000),
            )
            .await;
        if insp.returncode != 0 {
            return format!(
                "router container {LITELLM_CONTAINER} not found: sandboxes cannot reach the router until it runs"
            );
        }
        if insp.stdout.contains(&format!("{SANDBOX_NET}={LITELLM_IP}")) {
            return format!("router already pinned at {LITELLM_IP} on {SANDBOX_NET}");
        }
        self.run(
            &wsb::argv(&[
                "docker",
                "network",
                "disconnect",
                SANDBOX_NET,
                LITELLM_CONTAINER,
            ]),
            RunOpts::timeout(30.0),
        )
        .await;
        let c = self
            .run(
                &wsb::argv(&[
                    "docker",
                    "network",
                    "connect",
                    "--ip",
                    LITELLM_IP,
                    "--alias",
                    "litellm",
                    SANDBOX_NET,
                    LITELLM_CONTAINER,
                ]),
                RunOpts::timeout(30.0),
            )
            .await;
        if c.returncode == 0 {
            format!("router pinned at {LITELLM_IP} on {SANDBOX_NET}")
        } else {
            format!(
                "could not attach the router to {SANDBOX_NET}: {}",
                pystr::head(pystr::strip(&c.stderr), 200)
            )
        }
    }

    async fn gateway_up(&self, t0: Instant) -> ActionResult {
        let compose = self.compose();
        let Some(compose) = compose.filter(|c| c.is_file()) else {
            let shown = self
                .compose()
                .map_or_else(|| "None".to_owned(), |c| c.display().to_string());
            return done(
                t0,
                false,
                &format!("compose file not found: {shown}"),
                Vec::new(),
            );
        };
        if self.network_internal().await != Some(true) {
            return done(
                t0,
                false,
                &format!(
                    "refusing to start: docker network {SANDBOX_NET} is missing or NOT internal (it would give the gateway, which holds the Docker socket, a route off the box)."
                ),
                vec![Problem::new(
                    Severity::Error,
                    "openshell.network.not_internal",
                    format!("{SANDBOX_NET} is missing or not internal."),
                    Some(format!(
                        "docker network create --internal --subnet {SANDBOX_SUBNET} {SANDBOX_NET}"
                    )),
                )],
            );
        }
        let mut lines: Vec<String> = Vec::new();
        let c = compose.display().to_string();
        let cwd = compose.parent().map(Path::to_path_buf).unwrap_or_default();
        let up = self
            .run(
                &wsb::argv(&["docker", "compose", "-f", &c, "up", "-d"]),
                RunOpts::timeout(150.0).cwd(cwd),
            )
            .await;
        lines.push(pystr::strip(&format!("{}\n{}", up.stdout, up.stderr)).to_owned());
        if up.returncode != 0 {
            return done(
                t0,
                false,
                &format!("gateway compose failed:\n{}", lines.join("\n")),
                Vec::new(),
            );
        }
        lines.push(self.pin_router().await);
        let timing = self.timing();
        let deadline = Instant::now() + Duration::from_secs_f64(timing.ready_timeout.max(0.0));
        let mut conn;
        loop {
            conn = self.connection().await;
            if conn.connected {
                lines.push(format!(
                    "openshell status: {} (gateway {})",
                    conn.text,
                    conn.version.as_deref().unwrap_or("?")
                ));
                return done(t0, true, &lines.join("\n"), Vec::new());
            }
            if Instant::now() >= deadline {
                break;
            }
            wsb::sleep_s(timing.poll_interval).await;
        }
        lines.push(format!(
            "the gateway did not become reachable within {:.0} s ({})",
            timing.ready_timeout, conn.text
        ));
        done(
            t0,
            false,
            &lines.join("\n"),
            vec![Problem::new(
                Severity::Warn,
                "openshell.gateway.disconnected",
                "The containers started but the CLI is not connected.",
                Some(format!("docker logs --tail 50 {}", self.gateway_ctr())),
            )],
        )
    }

    async fn gateway_stop(&self, params: &Map<String, Value>, t0: Instant) -> ActionResult {
        let compose = self.compose();
        let Some(compose) = compose.filter(|c| c.is_file()) else {
            let shown = self
                .compose()
                .map_or_else(|| "None".to_owned(), |c| c.display().to_string());
            return done(
                t0,
                false,
                &format!("compose file not found: {shown}"),
                Vec::new(),
            );
        };
        let snap = self.sandboxes(0.0).await;
        if snap.ok && !snap.rows.is_empty() && params.get("force") != Some(&Value::Bool(true)) {
            let names = snap
                .rows
                .iter()
                .take(10)
                .map(|r| row_str(r, "name", "None"))
                .collect::<Vec<_>>()
                .join(", ");
            return done(
                t0,
                false,
                &format!(
                    "{} sandbox(es) exist ({names}). Stopping the gateway would orphan a running Data Designer run. Wait for it, delete the sandboxes, or set force.",
                    snap.rows.len()
                ),
                vec![Problem::new(
                    Severity::Warn,
                    "openshell.stop.sandboxes_exist",
                    "Sandboxes exist; the gateway was not stopped.",
                    Some("run again with force enabled".to_owned()),
                )],
            );
        }
        let c = compose.display().to_string();
        let cwd = compose.parent().map(Path::to_path_buf).unwrap_or_default();
        let r = self
            .run(
                &wsb::argv(&["docker", "compose", "-f", &c, "stop"]),
                RunOpts::timeout(120.0).cwd(cwd),
            )
            .await;
        *self.snap.lock().await = Snap::empty("invalidated");
        let out = pystr::strip(&format!("{}\n{}", r.stdout, r.stderr)).to_owned();
        done(
            t0,
            r.returncode == 0,
            if out.is_empty() { "stopped" } else { &out },
            Vec::new(),
        )
    }

    fn table(rows: &[Map<String, Value>]) -> String {
        if rows.is_empty() {
            return "No sandboxes found.".to_owned();
        }
        let mut lines = vec![format!(
            "{} {} {} ID",
            pystr::ljust("NAME", 40),
            pystr::ljust("PHASE", 14),
            pystr::ljust("CREATED (UTC)", 20)
        )];
        for r in rows {
            lines.push(format!(
                "{} {} {} {}",
                pystr::ljust(pystr::head(&row_str(r, "name", "None"), 40), 40),
                pystr::ljust(pystr::head(&row_str(r, "phase", "?"), 14), 14),
                pystr::ljust(pystr::head(&row_str(r, "created_at", "?"), 20), 20),
                pystr::head(&row_str(r, "id", ""), 36)
            ));
        }
        lines.join("\n")
    }

    async fn status_impl(&self) -> Result<SystemStatus, AdapterError> {
        let (version, (ctrs, docker_ok), internal) =
            tokio::join!(self.cli_version(), self.inspect(), self.network_internal());
        let gw_name = self.gateway_ctr();
        let containers = self.containers();
        let mut problems: Vec<Problem> = Vec::new();
        let mut components: Vec<Component> = Vec::new();
        let gw = ctrs.get(&gw_name);
        let gw_running = gw.is_some_and(Ctr::running);
        let mut conn = Conn::none("not checked: the gateway container is not running");
        if version.is_some() && (gw_running || !docker_ok) {
            conn = self.connection().await;
        }
        let snap = if conn.connected {
            self.sandboxes(SNAPSHOT_TTL).await
        } else {
            Snap::empty("")
        };
        components.push(Component {
            kind: "process".into(),
            name: "openshell CLI".into(),
            state: if version.is_some() {
                "running"
            } else {
                "absent"
            }
            .into(),
            image: None,
            detail: Some(match &version {
                Some(v) => format!("openshell {v}"),
                None => "the openshell binary is not on the console's PATH".to_owned(),
            }),
        });
        for name in &containers {
            let Some(c) = ctrs.get(name) else {
                components.push(Component {
                    kind: "container".into(),
                    name: name.clone(),
                    state: if docker_ok { "absent" } else { "unknown" }.into(),
                    image: None,
                    detail: Some(
                        if docker_ok {
                            "container does not exist"
                        } else {
                            "docker did not answer"
                        }
                        .into(),
                    ),
                });
                continue;
            };
            let state = if c.running() {
                "running"
            } else if c.failed() {
                "failed"
            } else {
                "stopped"
            };
            let mut detail = c.state.clone();
            if c.state == "exited" {
                detail.push_str(&format!(", exit {}", c.exit_code));
            }
            if c.restarts != 0 {
                detail.push_str(&format!(", {} restart(s)", c.restarts));
            }
            components.push(Component {
                kind: "container".into(),
                name: name.clone(),
                state: state.into(),
                image: Some(short_image(&c.image)),
                detail: Some(detail),
            });
        }
        components.push(Component {
            kind: "http".into(),
            name: format!(
                "gateway connection {}",
                if conn.server.is_empty() {
                    DEFAULT_ENDPOINT
                } else {
                    &conn.server
                }
            ),
            state: if conn.connected {
                "running"
            } else if conn.ran {
                "stopped"
            } else {
                "unknown"
            }
            .into(),
            image: None,
            detail: Some(conn.text.clone()),
        });

        // problems ------------------------------------------------------------------------------
        if version.is_none() {
            problems.push(Problem::new(
                Severity::Error,
                "openshell.cli.missing",
                "The openshell CLI is not installed (or not on the console's PATH).",
                Some("install openshell 0.0.71 into ~/.local/bin (see clients/data-designer/README.md and runner/IMPLEMENTATION.md section 2 for the pinned pair)".to_owned()),
            ));
        }
        if !docker_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "openshell.docker.unavailable",
                "Docker did not answer, so the gateway containers could not be inspected.",
                Some(
                    "docker info   # the rootless daemon must be running for the console's user"
                        .to_owned(),
                ),
            ));
        }
        if internal == Some(false) {
            let compose = self.compose().map_or_else(
                || "<runner>/docker-compose.yml".to_owned(),
                |c| c.display().to_string(),
            );
            problems.push(Problem::new(
                Severity::Error,
                "openshell.network.not_internal",
                format!(
                    "Docker network {SANDBOX_NET} is NOT internal: the gateway (it holds the Docker socket) and every sandbox supervisor have a route off the box, outside the lab egress allowlist. data-designer-local refuses to run until this is fixed."
                ),
                Some(format!(
                    "docker compose -f {compose} stop && docker network disconnect {SANDBOX_NET} {LITELLM_CONTAINER}; docker network rm {SANDBOX_NET} && docker network create --internal --subnet {SANDBOX_SUBNET} {SANDBOX_NET}   # data-designer-local re-pins the router on its next run"
                )),
            ));
        } else if internal.is_none() && docker_ok {
            problems.push(Problem::new(
                Severity::Warn,
                "openshell.network.missing",
                format!("Docker network {SANDBOX_NET} does not exist; the gateway cannot start without it."),
                Some(format!("docker network create --internal --subnet {SANDBOX_SUBNET} {SANDBOX_NET}")),
            ));
        }
        for c in ctrs.values() {
            let mut exposed: Vec<&String> = c
                .host_ips
                .iter()
                .filter(|ip| !LOOPBACK.contains(&ip.as_str()))
                .collect();
            if !exposed.is_empty() {
                exposed.sort();
                exposed.dedup();
                let compose = self.compose().map_or_else(
                    || "the runner docker-compose.yml".to_owned(),
                    |p| p.display().to_string(),
                );
                problems.push(Problem::new(
                    Severity::Error,
                    "openshell.port.exposed",
                    format!(
                        "{} publishes a port on {}: the gateway (mTLS only) is reachable beyond this machine. It must be published on 127.0.0.1 only.",
                        c.name,
                        exposed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    Some(format!("change the ports: entry of {} to 127.0.0.1:<port>:8080 in {compose} and recreate the container", c.name)),
                ));
            }
        }
        let gw_failed = gw.is_some_and(Ctr::failed);
        let gw_off = docker_ok
            && !gw_failed
            && !gw_running
            && gw.is_none_or(|g| matches!(g.state.as_str(), "exited" | "created" | "paused"));
        if gw_off && gw.is_none() {
            problems.push(Problem::new(
                Severity::Info,
                "openshell.gateway.off",
                "The OpenShell gateway container does not exist yet. It is created on demand by data-designer-local (or the gateway_up action).",
                Some(self.start_fix()),
            ));
        } else if gw_off {
            problems.push(Problem::new(
                Severity::Info,
                "openshell.gateway.off",
                "The OpenShell gateway is stopped. That is normal when no Data Designer run needs it: data-designer-local starts it on demand.",
                Some(self.start_fix()),
            ));
        }
        if gw_failed && let Some(g) = gw {
            problems.push(Problem::new(
                Severity::Error,
                "openshell.gateway.failed",
                format!(
                    "{} is {} (exit {}, {} restart(s)).",
                    g.name, g.state, g.exit_code, g.restarts
                ),
                Some(format!("docker logs --tail 50 {}", g.name)),
            ));
        }
        let fwd: Vec<&Ctr> = ctrs
            .iter()
            .filter(|(n, _)| **n != gw_name)
            .map(|(_, c)| c)
            .collect();
        if gw_running && fwd.iter().any(|c| !c.running()) {
            problems.push(Problem::new(
                Severity::Warn,
                "openshell.forwarder.stopped",
                "The CLI forwarder container is not running: the openshell CLI cannot reach the gateway on 127.0.0.1.",
                Some(self.start_fix()),
            ));
        }
        if gw_running && conn.ran && !conn.connected {
            if conn.text.to_lowercase().contains("no gateway configured") {
                problems.push(Problem::new(
                    Severity::Warn,
                    "openshell.gateway.unregistered",
                    format!(
                        "The CLI has no gateway registered as {}.",
                        str_repr(&self.gateway_name)
                    ),
                    Some(format!(
                        "openshell gateway add {DEFAULT_ENDPOINT} --local --name {}",
                        self.gateway_name
                    )),
                ));
            } else {
                problems.push(Problem::new(
                    Severity::Warn,
                    "openshell.gateway.disconnected",
                    format!(
                        "The gateway container runs but the CLI is not connected ({}).",
                        conn.text
                    ),
                    Some(format!(
                        "openshell -g {} status   # then: docker logs --tail 50 {gw_name}",
                        self.gateway_name
                    )),
                ));
            }
        }
        if let (Some(v), Some(cv)) = (&version, &conn.version)
            && v != cv
        {
            problems.push(Problem::new(
                Severity::Info,
                "openshell.version.skew",
                format!("CLI {v} talks to gateway {cv}. This pair is pinned on purpose (the 0.1.x gateway needs a newer CLI)."),
                Some("no action; bump both together and re-scan (runner/IMPLEMENTATION.md section 2)".to_owned()),
            ));
        }
        problems.extend(self.config_problems()?);

        // health ----------------------------------------------------------------------------------
        let health = if version.is_none() || gw_failed {
            Health::Down
        } else if gw_off {
            Health::Off
        } else if gw_running || !docker_ok {
            let degraded = !conn.connected
                || internal == Some(false)
                || problems.iter().any(|p| p.severity == Severity::Error)
                || fwd.iter().any(|c| !c.running());
            if !docker_ok && !conn.ran {
                Health::Unknown
            } else if degraded {
                Health::Degraded
            } else {
                Health::Up
            }
        } else {
            Health::Unknown
        };
        let gver = conn.version.clone();
        let mut summary = if conn.connected {
            if snap.ok {
                format!(
                    "connected · gateway {} · {} sandbox(es)",
                    gver.as_deref().unwrap_or("?"),
                    snap.rows.len()
                )
            } else {
                format!(
                    "connected · gateway {} · sandbox list unavailable",
                    gver.as_deref().unwrap_or("?")
                )
            }
        } else if health == Health::Off {
            "gateway stopped (starts on demand with Data Designer runs)".to_owned()
        } else {
            pystr::head(&format!("gateway {}", conn.text), 120).to_owned()
        };
        if let Some(v) = &version {
            summary.push_str(&format!(" · CLI {v}"));
        }
        Ok(SystemStatus {
            system: SystemId::Openshell,
            health,
            version: gver.or(version),
            summary: pystr::head(&summary, 300).to_owned(),
            endpoints: vec![Endpoint {
                name: "gateway (mTLS, CLI only)".into(),
                url: loopback(&conn.server),
                role: "api".into(),
                auth: "none".into(),
            }],
            components,
            problems,
            checked_at: now_ts(),
        })
    }
}

#[async_trait]
impl SystemAdapter for OpenShellAdapter {
    fn id(&self) -> SystemId {
        SystemId::Openshell
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        self.status_impl().await
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        let now = now_ts();
        if self.cli_version().await.is_none() {
            return Ok(MetricsSnapshot {
                system: SystemId::Openshell,
                ts: now,
                available: false,
                points: Vec::new(),
                error: Some("the openshell CLI is not installed".into()),
            });
        }
        let ((ctrs, docker_ok), internal) = tokio::join!(self.inspect(), self.network_internal());
        let gw = ctrs.get(&self.gateway_ctr()).cloned();
        let mut points: Vec<MetricPoint> = Vec::new();
        let p = |key: &str, value: f64, unit: &str| MetricPoint {
            key: key.into(),
            value,
            unit: unit.into(),
            labels: IndexMap::new(),
        };
        let mut conn: Option<Conn> = None;
        if gw.as_ref().is_some_and(Ctr::running) || !docker_ok {
            conn = Some(self.connection().await);
        }
        match &conn {
            Some(c) if c.ran => {
                points.push(p("os.gateway_up", if c.connected { 1.0 } else { 0.0 }, "1"));
            }
            _ if docker_ok => points.push(p("os.gateway_up", 0.0, "1")),
            _ => {}
        }
        if let Some(i) = internal {
            points.push(p("os.network_internal", if i { 1.0 } else { 0.0 }, "1"));
        }
        if let Some(g) = gw.as_ref().filter(|g| g.running()) {
            if let Some(started) = iso(&g.started) {
                points.push(p("os.gateway_uptime_s", (now - started).max(0.0), "s"));
            }
            #[allow(clippy::cast_precision_loss)]
            points.push(p("os.gateway_restarts", g.restarts as f64, "1"));
        }
        if conn.as_ref().is_some_and(|c| c.connected) {
            let snap = self.sandboxes(SNAPSHOT_TTL).await;
            if snap.ok {
                #[allow(clippy::cast_precision_loss)]
                points.push(p("os.sandboxes", snap.rows.len() as f64, "1"));
                let mut counts: IndexMap<&str, usize> = PHASES.iter().map(|ph| (*ph, 0)).collect();
                for row in &snap.rows {
                    let b = phase_bucket(&row_str(row, "phase", ""));
                    if let Some(n) = counts.get_mut(b.as_str()) {
                        *n += 1;
                    }
                }
                for ph in PHASES {
                    let mut labels = IndexMap::new();
                    labels.insert("phase".to_owned(), ph.to_owned());
                    #[allow(clippy::cast_precision_loss)]
                    points.push(MetricPoint {
                        key: "os.sandboxes_by_phase".into(),
                        value: counts[ph] as f64,
                        unit: "1".into(),
                        labels,
                    });
                }
                let ages: Vec<f64> = snap
                    .rows
                    .iter()
                    .filter_map(|r| utc(r.get("created_at")))
                    .map(|t| now - t)
                    .collect();
                if let Some(oldest) = ages.iter().copied().reduce(f64::max) {
                    points.push(p("os.sandbox_oldest_age_s", oldest.max(0.0), "s"));
                }
            }
            let r = self
                .cli(&["provider", "list", "-o", "json"], 15.0, 64_000)
                .await;
            if r.returncode == 0 {
                let text = strip_ansi(&r.stdout);
                if let Ok(Value::Array(provs)) =
                    serde_json::from_str::<Value>(if text.is_empty() { "[]" } else { &text })
                {
                    #[allow(clippy::cast_precision_loss)]
                    points.push(p("os.providers", provs.len() as f64, "1"));
                }
            }
        }
        Ok(MetricsSnapshot {
            system: SystemId::Openshell,
            ts: now,
            available: true,
            points,
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        self.observe().await;
        Ok(lock(&self.st)
            .ring
            .iter()
            .rev()
            .take(limit)
            .map(|(_, e)| e.clone())
            .collect())
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        Box::pin(async_stream::stream! {
            // Cursor = ring sequence number. With since_ts, replay buffered events newer than it first; without
            // it, start at "now". Memory is the shared ring plus one integer per consumer.
            self.observe().await;
            let mut cursor = if since_ts.is_none() { lock(&self.st).seq } else { 0 };
            loop {
                let ring: Vec<(u64, SystemEvent)> = lock(&self.st).ring.iter().cloned().collect();
                for (seq, ev) in ring {
                    if seq > cursor {
                        cursor = seq;
                        if since_ts.is_none_or(|s| ev.ts > s) {
                            yield Ok(ev);
                        }
                    }
                }
                wsb::sleep_s(self.timing().follow_interval).await;
                self.observe().await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        Ok(self
            .files()
            .iter()
            .map(|(id, label, p, fmt, desc)| self.spec(id, label, p, fmt, desc))
            .collect())
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        let files = self.files();
        let Some((_, _, raw, _, _)) = files.into_iter().find(|f| f.0 == file_id) else {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        };
        // containment: the real file must stay inside the runner directory (no symlink out of it)
        if let Some(rd) = self.runner_dir()
            && !resolve(&raw).starts_with(resolve(&rd))
        {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        Ok(raw)
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.config_path(file_id)?;
        let spec = self
            .config_files()?
            .into_iter()
            .find(|s| s.id == file_id)
            .ok_or_else(|| AdapterError::UnknownId(file_id.to_owned()))?;
        let nf = || AdapterError::FileNotFound(file_id.to_owned());
        let md = std::fs::metadata(&p).map_err(|_| nf())?;
        if md.len() > MAX_CONFIG_BYTES {
            return Err(nf());
        }
        let raw = std::fs::read(&p).map_err(|_| nf())?;
        Ok(ConfigFile {
            spec,
            content: redact(&String::from_utf8_lossy(&raw)),
            sha256: sha256_hex(&raw),
            mtime: crate::services::config_store::mtime_f64(&md).unwrap_or(0.0),
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        let Some((_, _, _, fmt, _)) = self.files().into_iter().find(|f| f.0 == file_id) else {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        };
        let mut diags: Vec<Diagnostic> = Vec::new();
        let upper = fmt.to_uppercase();
        let err_text = if fmt == "toml" {
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
        if let Some(t) = err_text {
            diags.push(Diagnostic {
                severity: Severity::Error,
                message: format!("{upper} syntax error: {}", pystr::head(&t, 300)),
                line: None,
            });
        }
        Ok(ValidationResult {
            ok: diags.is_empty(),
            validator: format!("{upper} parse"),
            authoritative: false,
            diagnostics: diags,
            note: Some("Read-only in the console (ADR 0007); this parse only reviews a proposed diff. It does not run `openshell policy prove` or start a sandbox.".into()),
        })
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        if !self.files().iter().any(|f| f.0 == file_id) {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        Ok(ApplyResult::new(
            false,
            "read-only: the console never writes OpenShell trust files",
        ))
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let name_param = || {
            let mut p = ParamSpec::new(
                "name",
                "Sandbox name",
                "A name from the sandbox list.",
                ParamType::String,
            );
            p.requires_restart = false;
            p
        };
        let lines_param = || {
            let mut p = ParamSpec::new(
                "lines",
                "Lines",
                "Newest lines to return (1-500).",
                ParamType::Int,
            );
            p.default = json!(100);
            p.min = Some(1.0);
            p.max = Some(500.0);
            p.requires_restart = false;
            p
        };
        let with = |mut a: ActionSpec, params: Vec<ParamSpec>| {
            a.params = params;
            a
        };
        let mut scopes = ParamSpec::new(
            "scopes",
            "Declared scopes",
            "Comma-separated scopes the key is MEANT to have (e.g. read). Empty = none declared. Anything the policy allows beyond them is reported.",
            ParamType::String,
        );
        scopes.requires_restart = false;
        let mut compact = ParamSpec::new(
            "compact",
            "Compact output",
            "One line per finding.",
            ParamType::Bool,
        );
        compact.default = json!(true);
        compact.requires_restart = false;
        let mut force = ParamSpec::new(
            "force",
            "Stop even with sandboxes",
            "Stop although sandboxes exist.",
            ParamType::Bool,
        );
        force.default = json!(false);
        force.requires_restart = false;
        Ok(vec![
            ActionSpec::new(
                "doctor",
                "Run doctor",
                "`openshell doctor check`: validates the local prerequisites for a gateway (Docker reachable, DOCKER_HOST). Read-only.",
                Danger::Safe,
            ),
            ActionSpec::new(
                "list_sandboxes",
                "List sandboxes",
                "`openshell sandbox list`: name, phase and creation time of every sandbox on the gateway. Data Designer creates one per run and deletes it afterwards.",
                Danger::Safe,
            ),
            with(
                ActionSpec::new(
                    "sandbox_get",
                    "Show a sandbox",
                    "`openshell sandbox get NAME`: phase and the active policy of one sandbox.",
                    Danger::Safe,
                ),
                vec![name_param()],
            ),
            with(
                ActionSpec::new(
                    "sandbox_logs",
                    "Sandbox logs",
                    "`openshell logs NAME -n N`: the supervisor and gateway log lines of one sandbox (OCSF events: SSH opens, Landlock, policy decisions). Redacted.",
                    Danger::Safe,
                ),
                vec![name_param(), lines_param()],
            ),
            with(
                ActionSpec::new(
                    "gateway_logs",
                    "Gateway container logs",
                    "`docker logs --tail N` of the gateway container. Redacted (tokens, JWTs).",
                    Danger::Safe,
                ),
                vec![lines_param()],
            ),
            with(
                ActionSpec::new(
                    "prove_policy",
                    "Prove sandbox policy",
                    "`openshell policy prove --policy policy.yaml --credentials <descriptor>`: the formal prover reports what the policy lets the credentialed binary do. The prover cannot run without a credential descriptor, so one is derived from the provider profile (credential name, auth style, the ENV NAME it is injected through, the endpoint hosts) plus the scopes you declare below; no key is read. Findings are items for a human to review, not failures.",
                    Danger::Safe,
                ),
                vec![scopes, compact],
            ),
            ActionSpec::new(
                "gateway_up",
                "Start the gateway",
                "Mirrors data-designer-local's ensure_gateway(): refuses unless openshell-docker is an INTERNAL network, runs `docker compose -f runner/docker-compose.yml up -d`, re-pins the router at 172.16.0.5 on that network, then waits up to 90 s for `openshell status` to say Connected.",
                Danger::Disruptive,
            ),
            with(
                ActionSpec::new(
                    "gateway_stop",
                    "Stop the gateway",
                    "`docker compose -f runner/docker-compose.yml stop` (never `down -v`): stops the gateway and the CLI forwarder, keeping the containers and the gateway's state. Refused while sandboxes exist unless `force` is set: a stopped gateway orphans a running Data Designer run.",
                    Danger::Disruptive,
                ),
                vec![force],
            ),
            with(
                ActionSpec::new(
                    "delete_sandbox",
                    "Delete a sandbox",
                    "`openshell sandbox delete NAME`: destroys the sandbox container and its workspace. A Data Designer run that is using it fails. The name must be in the live sandbox list.",
                    Danger::Destructive,
                ),
                vec![name_param()],
            ),
        ])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        const KNOWN: [&str; 9] = [
            "doctor",
            "list_sandboxes",
            "sandbox_get",
            "sandbox_logs",
            "gateway_logs",
            "prove_policy",
            "gateway_up",
            "gateway_stop",
            "delete_sandbox",
        ];
        if !KNOWN.contains(&action_id) {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        if !matches!(action_id, "gateway_logs" | "gateway_up" | "gateway_stop")
            && self.cli_version().await.is_none()
        {
            return Ok(done(
                t0,
                false,
                "The openshell CLI is not installed.",
                vec![Problem::new(
                    Severity::Error,
                    "openshell.cli.missing",
                    "The openshell CLI is not on the console's PATH.",
                    Some("install openshell 0.0.71 into ~/.local/bin (runner/IMPLEMENTATION.md section 2)".to_owned()),
                )],
            ));
        }
        match action_id {
            "doctor" => {
                let r = self
                    .run(
                        &wsb::argv(&[CLI, "doctor", "check"]),
                        wsb::opts(60.0, 64_000),
                    )
                    .await;
                Ok(done(t0, r.returncode == 0, &combined(&r), Vec::new()))
            }
            "list_sandboxes" => {
                let snap = self.sandboxes(0.0).await;
                if !snap.ok {
                    return Ok(done(
                        t0,
                        false,
                        &format!("The sandbox list is unavailable: {}", snap.error),
                        vec![self.unreachable_problem()],
                    ));
                }
                Ok(done(t0, true, &Self::table(&snap.rows), Vec::new()))
            }
            "sandbox_get" => match self.live_name(params).await {
                Err(e) => Ok(done(t0, false, &e, Vec::new())),
                Ok(name) => {
                    let r = self.cli(&["sandbox", "get", &name], 30.0, 256_000).await;
                    Ok(done(t0, r.returncode == 0, &combined(&r), Vec::new()))
                }
            },
            "sandbox_logs" => {
                let name = match self.live_name(params).await {
                    Err(e) => return Ok(done(t0, false, &e, Vec::new())),
                    Ok(n) => n,
                };
                let Some(n) = int_param(Some(params.get("lines").unwrap_or(&json!(100))), 1, 500)
                else {
                    return Ok(done(
                        t0,
                        false,
                        "lines must be an integer from 1 to 500",
                        Vec::new(),
                    ));
                };
                let r = self
                    .cli(&["logs", &name, "-n", &n.to_string()], 30.0, 512_000)
                    .await;
                Ok(done(
                    t0,
                    r.returncode == 0,
                    pystr::tail(&combined(&r), 19_000),
                    Vec::new(),
                ))
            }
            "gateway_logs" => {
                let Some(n) = int_param(Some(params.get("lines").unwrap_or(&json!(100))), 1, 500)
                else {
                    return Ok(done(
                        t0,
                        false,
                        "lines must be an integer from 1 to 500",
                        Vec::new(),
                    ));
                };
                let r = self
                    .run(
                        &wsb::argv(&[
                            "docker",
                            "logs",
                            "--tail",
                            &n.to_string(),
                            &self.gateway_ctr(),
                        ]),
                        wsb::opts(30.0, 512_000),
                    )
                    .await;
                let text = combined(&r);
                let tail = pystr::tail(&text, 19_000);
                Ok(done(
                    t0,
                    r.returncode == 0,
                    if tail.is_empty() {
                        "(no log lines)"
                    } else {
                        tail
                    },
                    Vec::new(),
                ))
            }
            "prove_policy" => self.prove(params, t0).await,
            "gateway_up" => Ok(self.gateway_up(t0).await),
            "gateway_stop" => Ok(self.gateway_stop(params, t0).await),
            _ => {
                // delete_sandbox
                let name = match self.live_name(params).await {
                    Err(e) => return Ok(done(t0, false, &e, Vec::new())),
                    Ok(n) => n,
                };
                let r = self.cli(&["sandbox", "delete", &name], 60.0, 256_000).await;
                *self.snap.lock().await = Snap::empty("invalidated"); // next read re-lists
                let mut out = combined(&r);
                if out.is_empty() {
                    out = format!("deleted {name}");
                }
                Ok(done(t0, r.returncode == 0, &out, Vec::new()))
            }
        }
    }
}

/// Build the adapter (Python: registry `build_adapter` + `container._configure_openshell`).
pub fn build(ctx: &AdapterContext, cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(OpenShellAdapter::from_settings(ctx, cfg)))
}
