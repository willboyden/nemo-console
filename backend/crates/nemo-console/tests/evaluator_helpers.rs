//! Test helpers for the NeMo Evaluator adapter (port of backend/tests/evaluator/ev_helpers.py + conftest.py): a fake
//! runner that answers like `evaluator-local` (outputs captured live, in fixtures/evaluator) and a context with a
//! suites file. The generic fakes (prefix-matching runner, gates, mock router) are skillevaluator_helpers.rs,
//! included here as `fakes`.
//! Everything offline; the router is a local mock or a CLOSED port.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, CommandRunner};
use nemo_console::adapters::evaluator::EvaluatorAdapter;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Value, json};

#[path = "skillevaluator_helpers.rs"]
pub mod fakes;

#[allow(unused_imports)]
pub use fakes::{
    CLOSED, FakeRunner, Gate, Resp, codes, func, gated, mode_of, ok, ok_rc, params, raises, res,
    router,
};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/evaluator");
pub const LAUNCHER: &str = "evaluator-local";

pub fn fx(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let i = argv.iter().position(|a| a == flag).unwrap();
    &argv[i + 1]
}

/// `evaluator-local run --suite S --output-dir D`: writes the captured summary.json under D (like the wrapper),
/// plus one of nel's heavy files the adapter must never read.
pub fn writes_summary(name: &'static str, rc: i32, stdout: Option<&'static str>) -> Resp {
    func(move |argv| {
        let out = PathBuf::from(after(argv, "--output-dir"));
        std::fs::create_dir_all(&out).unwrap();
        let mut doc: Value = serde_json::from_str(&fx(name)).unwrap();
        doc["suite"] = json!(after(argv, "--suite"));
        std::fs::write(out.join("summary.json"), doc.to_string()).unwrap();
        std::fs::create_dir_all(out.join("gsm8k")).unwrap();
        std::fs::write(
            out.join("gsm8k").join("results.jsonl"),
            "{\"prompt\": \"SECRET-QUESTION\", \"model_response\": \"SECRET-ANSWER\"}\n",
        )
        .unwrap();
        let so = stdout.map_or_else(|| fx("nel_tail_smoke.txt"), str::to_owned);
        res(&so, rc, "")
    })
}

pub fn standard_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on(&[LAUNCHER, "--version"], ok(&fx("version.txt")));
    r.on(&[LAUNCHER, "cached"], ok(&fx("cached_all.json")));
    r.on(
        &[LAUNCHER, "suites"],
        ok("smoke-lightning  aitop/nemotron-lightning  gsm8kx10\n"),
    );
    r.on(
        &[LAUNCHER, "run"],
        writes_summary("summary_smoke.json", 0, None),
    );
    r
}

pub fn make_ctx(
    tmp: &Path,
    runner: Arc<dyn CommandRunner>,
    litellm: &str,
    suites: Option<&str>,
    key_file: bool,
    repo: Option<&Path>,
) -> AdapterContext {
    let edir = tmp.join("repo").join("clients").join("nemo-evaluator");
    std::fs::create_dir_all(&edir).unwrap();
    let spath = edir.join("suites.yaml");
    if suites.is_some() || !spath.exists() {
        std::fs::write(
            &spath,
            suites.map_or_else(|| fx("suites_good.yaml"), str::to_owned),
        )
        .unwrap();
    }
    let key = tmp
        .join("home")
        .join(".config")
        .join("nemo-evaluator")
        .join("litellm.key");
    if key_file {
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "sk-FAKE-not-a-real-key-0123456789\n").unwrap();
    } else {
        let _ = std::fs::remove_file(&key);
    }
    let mut paths = IndexMap::new();
    paths.insert("evaluator_dir".to_owned(), edir);
    paths.insert("evaluator_suites".to_owned(), spath);
    paths.insert("evaluator_key_file".to_owned(), key);
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

/// Python's `make_adapter` fixture.
pub struct Fx {
    pub tmp: tempfile::TempDir,
    pub router: wiremock::MockServer,
}

impl Fx {
    pub async fn new() -> Fx {
        Fx::with_router(200).await
    }

    pub async fn with_router(status: u16) -> Fx {
        Fx {
            tmp: tempfile::tempdir().unwrap(),
            router: router(status).await,
        }
    }

    pub fn path(&self) -> PathBuf {
        nemo_console::services::support::realpath(self.tmp.path())
    }

    pub fn adapter(&self, runner: &Arc<FakeRunner>) -> EvaluatorAdapter {
        self.adapter_with(runner, None, true, None)
    }

    pub fn adapter_with(
        &self,
        runner: &Arc<FakeRunner>,
        suites: Option<&str>,
        key_file: bool,
        repo: Option<&Path>,
    ) -> EvaluatorAdapter {
        let a = EvaluatorAdapter::new(make_ctx(
            &self.path(),
            Arc::clone(runner) as Arc<dyn CommandRunner>,
            &self.router.uri(),
            suites,
            key_file,
            repo,
        ));
        a.set_follow_interval(0.01);
        a
    }
}

/// A stored run (Python `put_run`).
pub fn put_run(a: &EvaluatorAdapter, run_id: &str, meta: Value) -> PathBuf {
    let d = a.runs_dir().join(run_id);
    std::fs::create_dir_all(&d).unwrap();
    let suite = run_id.splitn(3, '-').nth(2).unwrap();
    let mut base = json!({"run_id": run_id, "suite": suite, "model": "aitop/nemotron-lightning",
                          "ts": 1_792_841_333.0, "verdict": "succeeded", "mode": "run"});
    for (k, v) in meta.as_object().unwrap() {
        base[k] = v.clone();
    }
    std::fs::write(d.join("run.json"), base.to_string()).unwrap();
    d
}

/// The newest run directory.
pub fn only_run_dir(a: &EvaluatorAdapter) -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(a.runs_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    dirs.pop().unwrap()
}

pub fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}
