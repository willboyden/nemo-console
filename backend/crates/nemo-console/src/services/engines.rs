//! One shared, cached, never-failing engine-container probe (topology + the Switchyard capable-tier check), and the
//! opt-in active capable-tier probe (addendum 9.11). Port of `services/engines.py`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::json;

use crate::adapters::base::{CommandRunner, RunOpts};
use crate::adapters::switchyard_capable::capable_models_text;
use crate::domain::models::{Problem, Severity, now_ts};
use crate::services::common::KNOWN;
use crate::services::common::RecoverLock as _;
use crate::services::environ::Environ;
use crate::services::http::Http;

/// "absent": no such container (not deployed on this host); "stopped": it exists and is not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    Running,
    Stopped,
    Absent,
    Unknown,
}

#[allow(clippy::expect_used)]
pub static CONTAINER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9_.\-]{0,127}\z").expect("static regex"));
pub const TTL: f64 = 5.0;

pub struct EngineProbe {
    runner: Arc<dyn CommandRunner>,
    ttl: f64,
    cache: Mutex<HashMap<String, (Instant, EngineState)>>,
}

impl EngineProbe {
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        EngineProbe {
            runner,
            ttl: TTL,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn state(&self, container: &str) -> EngineState {
        if !CONTAINER_RE.is_match(container) {
            return EngineState::Unknown;
        }
        if let Ok(c) = self.cache.lock_recovering()
            && let Some((t, st)) = c.get(container)
            && t.elapsed().as_secs_f64() < self.ttl
        {
            return *st;
        }
        let argv: Vec<String> = ["docker", "inspect", "-f", "{{.State.Status}}", container]
            .map(str::to_owned)
            .to_vec();
        let st = match self
            .runner
            .run(&argv, RunOpts::timeout(5.0).max_bytes(200))
            .await
        {
            Ok(r) if r.timed_out => EngineState::Unknown,
            Ok(r) if r.returncode != 0 => {
                // "No such object" = absent (not deployed); anything else (daemon down, ...) is unknown
                if format!("{}{}", r.stderr, r.stdout)
                    .to_lowercase()
                    .contains("no such")
                {
                    EngineState::Absent
                } else {
                    EngineState::Unknown
                }
            }
            Ok(r) => {
                if r.stdout.trim() == "running" {
                    EngineState::Running
                } else {
                    EngineState::Stopped
                }
            }
            Err(_) => EngineState::Unknown,
        };
        if let Ok(mut c) = self.cache.lock_recovering() {
            c.insert(container.to_owned(), (Instant::now(), st));
        }
        st
    }
}

/// ai-lab-<project>-<service>-<n> -> inference/<project> when that directory exists.
#[must_use]
pub fn compose_dir_hint(repo_root: &Path, container: &str) -> Option<String> {
    #[allow(clippy::expect_used)]
    static TRAILING_NUM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"-\d+\n?\z").expect("static regex"));
    let name = container.strip_prefix("ai-lab-").unwrap_or(container);
    let name = TRAILING_NUM.replace(name, "");
    let parts: Vec<&str> = name.split('-').collect();
    for i in (1..=parts.len()).rev() {
        let cand = parts[..i].join("-");
        if repo_root.join("inference").join(&cand).is_dir() {
            return Some(format!("inference/{cand}"));
        }
    }
    None
}

/// The copy-pasteable remedy for a stopped engine: its configured start command, else the compose-dir guess.
#[must_use]
pub fn start_fix(
    repo_root: &Path,
    container: &str,
    engine_start: &IndexMap<String, String>,
) -> String {
    if let Some(cmd) = engine_start.get(container).filter(|c| !c.is_empty()) {
        return format!("cd {} && {cmd}", repo_root.display());
    }
    let where_ = match compose_dir_hint(repo_root, container) {
        Some(h) => format!("cd {h}"),
        None => "cd the engine's compose directory".to_owned(),
    };
    format!("{where_} && docker compose up -d")
}

fn which_routes(routes: &[String]) -> String {
    if routes.len() == 1 {
        format!("route {}", routes[0])
    } else {
        format!("routes {}", routes.join(", "))
    }
}

/// capable_dark (warn) / capable_not_deployed (info) per probed alias a stage_router route escalates to.
pub async fn capable_tier_problems(
    probe: &EngineProbe,
    engine_probes: &IndexMap<String, String>,
    routes_text: &str,
    repo_root: &Path,
    engine_start: &IndexMap<String, String>,
) -> Vec<Problem> {
    let Some(capable) = capable_models_text(routes_text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (alias, container) in engine_probes {
        let Some(routes) = capable.get(alias).filter(|r| !r.is_empty()) else {
            continue;
        };
        let state = probe.state(container).await;
        let which = which_routes(routes);
        match state {
            EngineState::Stopped => out.push(Problem::new(
                Severity::Warn,
                "switchyard.tier.capable_dark",
                format!(
                    "Capable tier target {alias} is not answering: engine container {container} is not running, so {which} cannot escalate to it"
                ),
                Some(start_fix(repo_root, container, engine_start)),
            )),
            EngineState::Absent => out.push(Problem::new(
                Severity::Info,
                "switchyard.tier.capable_not_deployed",
                format!(
                    "Capable tier target {alias} is not deployed here (no container {container}), so {which} cannot escalate to it"
                ),
                Some(format!(
                    "point {which} at a deployed engine in routes.toml, or deploy {alias} if that tier should exist (start: {})",
                    start_fix(repo_root, container, engine_start)
                )),
            )),
            EngineState::Running | EngineState::Unknown => {}
        }
    }
    out
}

/// (alias, container, routes) for each probed alias a stage_router route escalates to; [] if unparseable.
#[must_use]
pub fn capable_targets(
    engine_probes: &IndexMap<String, String>,
    routes_text: &str,
) -> Vec<(String, String, Vec<String>)> {
    let Some(capable) = capable_models_text(routes_text) else {
        return Vec::new();
    };
    engine_probes
        .iter()
        .filter_map(|(a, c)| {
            capable
                .get(a)
                .filter(|r| !r.is_empty())
                .map(|r| (a.clone(), c.clone(), r.clone()))
        })
        .collect()
}

// ── active capable-tier probe (addendum 9.11, opt-in) ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    Ok,
    Broken,
    Denied,
    Unconfigured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub verdict: ProbeVerdict,
    /// Status only: an HTTP code, an exception class, or "no choices"; never model text.
    pub detail: String,
    /// `time.time()` of the request.
    pub at: f64,
}

/// Python `f"{x:.0f}"`.
fn f0(x: f64) -> String {
    format!("{x:.0}")
}

/// One tiny real completion per capable-tier alias through the router, at most once per interval per alias, one
/// request at a time, in the background. See the Python docstring for the why.
pub struct CapableProbe {
    http: Http,
    pub url: String,
    pub key_env: String,
    pub interval_s: f64,
    pub timeout_s: f64,
    pub retry_delay_s: f64,
    environ: Environ,
    results: Mutex<HashMap<String, ProbeResult>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub const PROBE_PROMPT: &str = "ping";

impl CapableProbe {
    #[must_use]
    pub fn new(
        http: Http,
        router_url: &str,
        key_env: &str,
        interval_s: f64,
        timeout_s: f64,
        retry_delay_s: f64,
        environ: Environ,
    ) -> Self {
        CapableProbe {
            http,
            url: format!("{}/v1/chat/completions", router_url.trim_end_matches('/')),
            key_env: key_env.to_owned(),
            interval_s,
            timeout_s,
            retry_delay_s,
            environ,
            results: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
        }
    }

    fn key(&self) -> Option<String> {
        let v = self.environ.get(&self.key_env).filter(|v| !v.is_empty())?;
        KNOWN.add(&v);
        Some(v)
    }

    #[must_use]
    pub fn results(&self) -> HashMap<String, ProbeResult> {
        self.results
            .lock_recovering()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Test seam (Python tests set `probe._results[alias]`).
    pub fn set_result(&self, alias: &str, res: ProbeResult) {
        if let Ok(mut r) = self.results.lock_recovering() {
            r.insert(alias.to_owned(), res);
        }
    }

    /// True when a background pass has been started (Python `probe._task is not None`).
    #[must_use]
    pub fn has_task(&self) -> bool {
        self.task
            .lock_recovering()
            .map(|t| t.is_some())
            .unwrap_or(false)
    }

    /// Wait for the current background pass to finish (Python `await probe._task`).
    pub async fn join(&self) {
        let handle = self.task.lock_recovering().ok().and_then(|mut t| t.take());
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    fn due(&self, aliases: &[String], now: f64) -> Vec<String> {
        let r = self.results();
        aliases
            .iter()
            .filter(|a| r.get(*a).is_none_or(|x| now - x.at >= self.interval_s))
            .cloned()
            .collect()
    }

    /// Start one background pass over the aliases that are due, unless a pass is already running.
    pub fn schedule(self: &Arc<Self>, aliases: &[String]) {
        let Ok(mut t) = self.task.lock_recovering() else {
            return;
        };
        if t.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let due = self.due(aliases, now_ts());
        if !due.is_empty() {
            let me = Arc::clone(self);
            *t = Some(tokio::spawn(async move {
                crate::services::common::contained("capable-tier probe", me.run(due)).await;
            }));
        }
    }

    async fn run(&self, aliases: Vec<String>) {
        for alias in aliases {
            let mut res = self.probe(&alias).await;
            if res.verdict == ProbeVerdict::Broken {
                tokio::time::sleep(Duration::from_secs_f64(self.retry_delay_s)).await;
                res = self.probe(&alias).await;
                if res.verdict == ProbeVerdict::Broken {
                    res = ProbeResult {
                        verdict: ProbeVerdict::Broken,
                        detail: format!("{}, twice {} s apart", res.detail, f0(self.retry_delay_s)),
                        at: res.at,
                    };
                }
            }
            if let Ok(mut r) = self.results.lock_recovering() {
                r.insert(alias, res);
            }
        }
    }

    pub async fn probe(&self, alias: &str) -> ProbeResult {
        let now = now_ts();
        let Some(key) = self.key() else {
            return ProbeResult {
                verdict: ProbeVerdict::Unconfigured,
                detail: format!("env var {} is not set in the console process", self.key_env),
                at: now,
            };
        };
        let body = json!({"model": alias, "messages": [{"role": "user", "content": PROBE_PROMPT}], "max_tokens": 1,
                          "temperature": 0, "stream": false, "cache": {"no-cache": true}});
        let started = Instant::now();
        let auth = format!("Bearer {key}");
        let res = match self
            .http
            .post_json(
                &self.url,
                &body,
                &[("authorization", auth.as_str())],
                self.timeout_s,
            )
            .await
        {
            Err(e) if e.is_timeout() => ProbeResult {
                verdict: ProbeVerdict::Broken,
                detail: format!("no answer within {} s", f0(self.timeout_s)),
                at: now,
            },
            Err(e) => ProbeResult {
                verdict: ProbeVerdict::Broken,
                detail: format!("router unreachable ({})", e.class),
                at: now,
            },
            Ok(resp) if resp.status == 401 || resp.status == 403 => ProbeResult {
                verdict: ProbeVerdict::Denied,
                detail: format!("router answered {}", resp.status),
                at: now,
            },
            Ok(resp) if resp.status >= 400 => ProbeResult {
                verdict: ProbeVerdict::Broken,
                detail: format!("router answered {}", resp.status),
                at: now,
            },
            Ok(resp) => {
                let ok = resp
                    .json()
                    .ok()
                    .and_then(|d| {
                        d.get("choices")
                            .and_then(|c| c.as_array())
                            .map(|a| !a.is_empty())
                    })
                    .unwrap_or(false);
                ProbeResult {
                    verdict: if ok {
                        ProbeVerdict::Ok
                    } else {
                        ProbeVerdict::Broken
                    },
                    detail: if ok {
                        "answered"
                    } else {
                        "no choices in the answer"
                    }
                    .to_owned(),
                    at: now,
                }
            }
        };
        tracing::info!(
            "capable probe {alias}: {} ({}, {:.1} s)",
            verdict_str(res.verdict),
            res.detail,
            started.elapsed().as_secs_f64()
        );
        res
    }

    pub async fn aclose(&self) {
        let handle = self.task.lock_recovering().ok().and_then(|mut t| t.take());
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
    }
}

#[must_use]
pub fn verdict_str(v: ProbeVerdict) -> &'static str {
    match v {
        ProbeVerdict::Ok => "ok",
        ProbeVerdict::Broken => "broken",
        ProbeVerdict::Denied => "denied",
        ProbeVerdict::Unconfigured => "unconfigured",
    }
}

/// Python `str.capitalize()`: first character upper, the rest LOWER.
#[must_use]
pub fn py_capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// Problems from the cached probe results for the capable targets whose container is RUNNING.
#[must_use]
pub fn probe_problems(
    results: &HashMap<String, ProbeResult>,
    running: &[(String, String, Vec<String>)],
    repo_root: &Path,
    engine_start: &IndexMap<String, String>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut config_issue: Option<&ProbeResult> = None;
    for (alias, container, routes) in running {
        let Some(res) = results.get(alias) else {
            continue;
        };
        match res.verdict {
            ProbeVerdict::Ok => continue,
            ProbeVerdict::Unconfigured | ProbeVerdict::Denied => {
                config_issue = config_issue.or(Some(res));
                continue;
            }
            ProbeVerdict::Broken => {}
        }
        let which = which_routes(routes);
        #[allow(clippy::cast_possible_truncation)]
        let age = ((now_ts() - res.at) as i64).max(0);
        out.push(Problem::new(
            Severity::Warn,
            "switchyard.tier.capable_broken",
            format!(
                "Capable tier target {alias} runs (container {container}) but did not answer a 1-token test completion through the router {age} s ago: {}. {} cannot escalate to it until it answers.",
                res.detail,
                py_capitalize(&which)
            ),
            Some(format!(
                "docker logs --tail 80 {container}   # then restart it: {}",
                start_fix(repo_root, container, engine_start)
            )),
        ));
    }
    if let Some(ci) = config_issue {
        let code = if ci.verdict == ProbeVerdict::Unconfigured {
            "switchyard.tier.probe_unconfigured"
        } else {
            "switchyard.tier.probe_denied"
        };
        out.push(Problem::new(
            Severity::Info,
            code,
            format!("The active capable-tier probe is on but cannot run: {}.", ci.detail),
            Some(
                "export the router key in the console's environment (setting capable_probe_key_env names the variable), or turn the probe off: NEMO_CONSOLE_CAPABLE_PROBE=false"
                    .to_owned(),
            ),
        ));
    }
    out
}
