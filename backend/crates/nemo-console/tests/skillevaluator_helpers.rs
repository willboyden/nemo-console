//! Test helpers for the SkillEvaluator adapter (port of backend/tests/skillevaluator/skilleval_helpers.py +
//! conftest.py): a fake runner that matches argv by prefix (longest first) and writes real-shaped reports, a skills
//! tree, and a wiremock "router". Everything offline; the router URL is a local mock or a CLOSED port.
//! Included by the skillevaluator_* test files with `#[path]`; compiled on its own it is an empty test target.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::new_without_default
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use nemo_console::adapters::base::{
    AdapterContext, CommandResult, CommandRunner, RunOpts, RunnerError,
};
use nemo_console::adapters::skillevaluator::SkillEvaluatorAdapter;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::pyfmt::{JsonOpts, json_dumps_with};
use serde_json::json;

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skillevaluator");
pub const T3_SKILL: &str = "calculator";
pub const T3_RUN: &str = "20261017_091635_7_46d5ccd742f5";
pub const T3_JOB: &str = "20261017-051633-16fc9065";
/// A port nothing listens on (the hermetic default for every upstream).
pub const CLOSED: &str = "http://127.0.0.1:9";

pub fn fx(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

pub type Resp = Arc<
    dyn Fn(Vec<String>) -> BoxFuture<'static, Result<CommandResult, RunnerError>> + Send + Sync,
>;

pub fn res(stdout: &str, rc: i32, stderr: &str) -> CommandResult {
    CommandResult::new(rc, stdout, stderr)
}

/// A fixed answer.
pub fn ok(stdout: &str) -> Resp {
    ok_rc(stdout, 0, "")
}

pub fn ok_rc(stdout: &str, rc: i32, stderr: &str) -> Resp {
    let r = res(stdout, rc, stderr);
    Arc::new(move |_| {
        let r = r.clone();
        Box::pin(async move { Ok(r) })
    })
}

/// A synchronous responder.
pub fn func(f: impl Fn(&[String]) -> CommandResult + Send + Sync + 'static) -> Resp {
    let f = Arc::new(f);
    Arc::new(move |argv| {
        let r = f(&argv);
        Box::pin(async move { Ok(r) })
    })
}

/// A responder that raises (Python `raise RuntimeError(...)` inside the fake runner).
pub fn raises(class: &'static str, msg: &str) -> Resp {
    let msg = msg.to_owned();
    Arc::new(move |_| {
        let m = msg.clone();
        Box::pin(async move { Err(RunnerError::Os(class, m)) })
    })
}

/// Python's `asyncio.Event` gate.
#[derive(Clone)]
pub struct Gate(Arc<tokio::sync::watch::Sender<bool>>);

impl Gate {
    pub fn new() -> Gate {
        Gate(Arc::new(tokio::sync::watch::channel(false).0))
    }

    pub fn set(&self) {
        self.0.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}

/// A responder that blocks until the gate opens.
pub fn gated(inner: Resp, gate: &Gate) -> Resp {
    let gate = gate.clone();
    Arc::new(move |argv| {
        let gate = gate.clone();
        let inner = Arc::clone(&inner);
        Box::pin(async move {
            gate.wait().await;
            inner(argv).await
        })
    })
}

/// Matches argv by prefix (longest first). Unmatched argv -> rc 127 so a missing stub is loud, not silent.
#[derive(Default)]
pub struct FakeRunner {
    rules: Mutex<Vec<(Vec<String>, Resp)>>,
    pub calls: Mutex<Vec<Vec<String>>>,
    pub cwds: Mutex<Vec<Option<PathBuf>>>,
    pub timeouts: Mutex<Vec<f64>>,
}

impl FakeRunner {
    pub fn new() -> Arc<FakeRunner> {
        Arc::new(FakeRunner::default())
    }

    pub fn on(&self, prefix: &[&str], r: Resp) -> &Self {
        let p: Vec<String> = prefix.iter().map(|s| (*s).to_owned()).collect();
        let mut rules = self.rules.lock().unwrap();
        rules.retain(|(q, _)| *q != p);
        rules.push((p, r));
        self
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }

    pub fn cwds(&self) -> Vec<Option<PathBuf>> {
        self.cwds.lock().unwrap().clone()
    }

    pub fn timeouts(&self) -> Vec<f64> {
        self.timeouts.lock().unwrap().clone()
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, argv: &[String], opts: RunOpts) -> Result<CommandResult, RunnerError> {
        self.calls.lock().unwrap().push(argv.to_vec());
        self.cwds.lock().unwrap().push(opts.cwd.clone());
        self.timeouts.lock().unwrap().push(opts.timeout);
        let resp = {
            let mut rules: Vec<(Vec<String>, Resp)> = self.rules.lock().unwrap().clone();
            rules.sort_by_key(|r| std::cmp::Reverse(r.0.len()));
            rules
                .into_iter()
                .find(|(p, _)| argv.len() >= p.len() && argv[..p.len()] == p[..])
                .map(|(_, r)| r)
        };
        match resp {
            Some(r) => r(argv.to_vec()).await,
            None => Ok(res("", 127, &format!("no fake for {argv:?}"))),
        }
    }
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let i = argv.iter().position(|a| a == flag).unwrap();
    &argv[i + 1]
}

/// Like the real CLI: write the JSON (+ markdown) report into the -o dir, exit 1 when not all checks passed.
pub fn writes_report(name: &'static str, fixture: &'static str, rc: i32) -> Resp {
    func(move |argv| {
        let out = PathBuf::from(after(argv, "-o"));
        std::fs::write(out.join(format!("skillevaluator-{name}.json")), fx(fixture)).unwrap();
        if name == "tier1" {
            std::fs::write(
                out.join("skillevaluator-tier1.md"),
                "# SkillEvaluator Validation Report\n\n**Status:** INCOMPLETE\n",
            )
            .unwrap();
        }
        res(
            "progress lines",
            rc,
            if rc != 0 {
                "Error: Tier 1 checks failed or were incomplete"
            } else {
                ""
            },
        )
    })
}

/// The launcher's final stdout with --quiet: one indent=2 JSON object.
pub fn tier3_stdout(execution_status: Option<&str>, error: Option<&str>, rc: i32) -> String {
    let t3 = match error {
        Some(e) => json!({"execution_status": "error", "error": e}),
        None => {
            json!({"execution_status": execution_status, "scored_attempts": 4, "expected_attempts": 4})
        }
    };
    json_dumps_with(
        &json!({
            "job": T3_JOB,
            "job_dir": format!("/home/user/.local/share/skilleval-mcp/jobs/cli/{T3_JOB}"),
            "container_exit": rc,
            "summary": {"skill": T3_SKILL, "tiers": {"3": t3}},
        }),
        JsonOpts {
            indent: Some(2),
            ..JsonOpts::default()
        },
    )
}

/// Like `skillevaluator-local tier3 SKILL ... --results-dir D --quiet`.
pub fn writes_tier3(fixture: Option<&'static str>, rc: i32, stderr: &str) -> Resp {
    let stderr = if stderr.is_empty() {
        format!("launch: job {T3_JOB} ({T3_SKILL}, tier3)")
    } else {
        stderr.to_owned()
    };
    func(move |argv| {
        let out = PathBuf::from(after(argv, "--results-dir"));
        if let Some(f) = fixture {
            let run = out.join(T3_SKILL).join(T3_RUN);
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(run.join("result.json"), fx(f)).unwrap();
            std::fs::write(
                run.join("report.html"),
                "<!doctype html><title>report</title>\n",
            )
            .unwrap();
        }
        let status = fixture.map(|_| "succeeded");
        res(&tier3_stdout(status, None, rc), rc, &stderr)
    })
}

pub fn standard_runner(launcher: &str) -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on(
        &[launcher, "--version"],
        ok("skillevaluator, version 0.5.0\n"),
    );
    r.on(
        &[launcher, "tier1"],
        writes_report("tier1", "tier1_incomplete.json", 1),
    );
    r.on(
        &[launcher, "tier2"],
        writes_report("tier2", "tier2_passed.json", 0),
    );
    r.on(
        &[launcher, "tier3"],
        writes_tier3(Some("tier3_result.json"), 0, ""),
    );
    r.on(
        &[launcher, "doctor"],
        ok("CLI package pass skillevaluator 0.5.0\nPublic LLM provider pass openai-compatible\n"),
    );
    r
}

/// Idempotent: a second adapter in the same test (a "restarted console") reuses the same tree and state dir.
pub fn build_skills(tmp: &Path) -> PathBuf {
    let root = tmp.join("repo").join(".claude").join("skills");
    if root.is_dir() {
        return root;
    }
    for name in ["robust-scripts", "serve-model"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(
            root.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n# {name}\n"),
        )
        .unwrap();
    }
    std::fs::create_dir(root.join("no-skill-md")).unwrap();
    std::fs::write(root.join("README.md"), "not a directory").unwrap();
    let outside = tmp.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("SKILL.md"), "---\nname: outside\n---\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    root
}

/// A wiremock router whose /health/liveliness answers `status`.
pub async fn router(status: u16) -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/health/liveliness"))
        .respond_with(wiremock::ResponseTemplate::new(status).set_body_json("I'm alive!"))
        .mount(&server)
        .await;
    server
}

pub fn make_ctx(
    tmp: &Path,
    runner: Arc<dyn CommandRunner>,
    litellm: &str,
    skills: bool,
    repo: Option<&Path>,
) -> AdapterContext {
    let mut paths = IndexMap::new();
    if skills {
        paths.insert("skills_dir".to_owned(), build_skills(tmp));
    }
    if let Some(r) = repo {
        paths.insert("repo".to_owned(), r.to_path_buf());
    }
    let mut urls = IndexMap::new();
    urls.insert("litellm".to_owned(), litellm.to_owned());
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls,
        paths,
        secret_env: IndexMap::new(),
        state_dir: tmp.join("state"),
        environ: Environ::fixed(HashMap::new()),
    }
}

/// Python's `make_adapter` fixture: one temp dir and one mock router per test; every adapter of the test shares
/// them (a second adapter = a restarted console over the same state).
pub struct Fx {
    pub tmp: tempfile::TempDir,
    pub router: wiremock::MockServer,
}

impl Fx {
    pub async fn new() -> Fx {
        Fx {
            tmp: tempfile::tempdir().unwrap(),
            router: router(200).await,
        }
    }

    /// `tmp_path`, resolved (macOS-style /tmp symlinks never differ from what the adapter resolves).
    pub fn path(&self) -> PathBuf {
        nemo_console::services::support::realpath(self.tmp.path())
    }

    pub fn adapter(&self, runner: &Arc<FakeRunner>) -> SkillEvaluatorAdapter {
        self.adapter_with(runner, true, None)
    }

    pub fn adapter_with(
        &self,
        runner: &Arc<FakeRunner>,
        skills: bool,
        repo: Option<&Path>,
    ) -> SkillEvaluatorAdapter {
        let a = SkillEvaluatorAdapter::new(make_ctx(
            &self.path(),
            Arc::clone(runner) as Arc<dyn CommandRunner>,
            &self.router.uri(),
            skills,
            repo,
        ));
        a.set_follow_interval(0.01);
        a
    }
}

/// `{p.code for p in problems}`.
pub fn codes(
    problems: &[nemo_console::domain::models::Problem],
) -> std::collections::BTreeSet<String> {
    problems.iter().map(|p| p.code.clone()).collect()
}

/// Mode bits of a path.
pub fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

pub fn run_json(a: &SkillEvaluatorAdapter, run_id: &str) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(a.runs_dir().join(run_id).join("run.json")).unwrap(),
    )
    .unwrap()
}

pub fn params(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().unwrap().clone()
}
