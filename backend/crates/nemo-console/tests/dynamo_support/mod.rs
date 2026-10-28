//! Dynamo test helpers (port of backend/tests/dynamo/{conftest,helpers}.py and the shared helpers of
//! test_adapter.py): fixture docs, the fake runner wiring, the fake frontend, and the `make` fixture.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::path::PathBuf;
use std::sync::Arc;

use nemo_console::adapters::base::CommandResult;
use nemo_console::adapters::dynamo::DynamoAdapter;
use nemo_console::domain::models::SystemStatus;
use serde_json::{Value, json};

use crate::switchyard_support::{FakeHttp, FakeRunner, Reply, ctx, fixture, ok};

pub const FE: &str = "ai-lab-dynamo-dynamo-frontend-1";
pub const WK: &str = "ai-lab-dynamo-dynamo-worker-1";
pub const ENG: &str = "ai-lab-vllm-qwen38-vllm-1";
pub const SV: &str = "ai-lab-dynamo-dynamo-sv-frontend-1";
pub const UUID: &str = "GPU-00000000-0000-0000-0000-000000000001";
pub const OTHER_UUID: &str = "GPU-00000000-0000-0000-0000-000000000002";
pub const FN: &str = "ai-lab-sglang-flashnext-flashnext-1";
pub const LIGHTNING: &str = "ai-lab-vllm-nemotron-lightning-nemotron-lightning-1";
pub const OLLAMA: &str = "ai-lab-ollama-ollama-1";
pub const DCGM: &str = "ai-lab-observability-dcgm-exporter-1";
pub const START: &str = "scripts/use-model.sh qwen3.8-27b-dynamo";
/// The captured containers' compose labels name this directory; the fake runner maps it to the test's copy.
pub const CAPTURED_DIR: &str = "/home/user/lab/inference/dynamo";

pub fn fx(name: &str) -> String {
    fixture("dynamo", name)
}

pub fn fxj(name: &str) -> Value {
    serde_json::from_str(&fx(name)).unwrap()
}

pub fn worker_doc() -> Value {
    fxj("worker_inspect.json")[0].clone()
}

pub fn frontend_doc() -> Value {
    fxj("frontend_inspect.json")[0].clone()
}

pub fn name_of(d: &Value) -> String {
    d["Name"]
        .as_str()
        .unwrap()
        .trim_start_matches('/')
        .to_owned()
}

/// Synthesised from the REAL worker inspect shape (the live engine is stopped, so none could be captured).
pub fn engine_doc(running: bool, name: &str, project: &str, uuid: &str) -> Value {
    let mut d = worker_doc();
    d["Name"] = Value::from(format!("/{name}"));
    d["State"]["Running"] = Value::Bool(running);
    d["State"]["Status"] = Value::from(if running { "running" } else { "exited" });
    let lb = &mut d["Config"]["Labels"];
    lb["com.docker.compose.project"] = Value::from(project);
    lb["com.docker.compose.service"] = Value::from("vllm");
    lb["com.docker.compose.project.working_dir"] = Value::from("/repo/inference/vllm-qwen38");
    lb["com.docker.compose.project.config_files"] =
        Value::from("/repo/inference/vllm-qwen38/docker-compose.yml");
    d["HostConfig"]["DeviceRequests"][0]["DeviceIDs"] = json!([uuid]);
    d["Config"]["Image"] = Value::from("vllm/vllm-openai:v0.27.1");
    d
}

pub fn engine(running: bool) -> Value {
    engine_doc(running, ENG, "ai-lab-vllm-qwen38", UUID)
}

/// A running GPU container shaped like the real inspect.
pub fn gpu_doc(
    name: &str,
    uuids: Option<&[&str]>,
    all_gpus: bool,
    running: bool,
    project: &str,
) -> Value {
    let mut d = engine_doc(running, name, project, UUID);
    let req = &mut d["HostConfig"]["DeviceRequests"][0];
    if all_gpus {
        req["DeviceIDs"] = Value::Null;
        req["Count"] = Value::from(-1);
    } else {
        req["DeviceIDs"] = json!(uuids.unwrap_or(&[UUID]));
        req["Count"] = Value::from(0);
    }
    d
}

pub fn gpu(name: &str) -> Value {
    gpu_doc(name, None, false, true, "other")
}

pub fn gpu_all(name: &str) -> Value {
    gpu_doc(name, None, true, true, "other")
}

pub fn flashnext_doc(running: bool, uuids: Option<&[&str]>) -> Value {
    gpu_doc(
        FN,
        Some(uuids.unwrap_or(&[OTHER_UUID, UUID])),
        false,
        running,
        "ai-lab-sglang-flashnext",
    )
}

pub fn stopped(mut doc: Value, code: i64) -> Value {
    doc["State"]["Running"] = Value::Bool(false);
    doc["State"]["Restarting"] = Value::Bool(false);
    doc["State"]["Status"] = Value::from("exited");
    doc["State"]["ExitCode"] = Value::from(code);
    doc
}

/// Docker reports Running=true while a container sits in its restart back-off.
pub fn restarting(mut doc: Value) -> Value {
    doc["State"]["Running"] = Value::Bool(true);
    doc["State"]["Restarting"] = Value::Bool(true);
    doc["State"]["Status"] = Value::from("restarting");
    doc["State"]["ExitCode"] = Value::from(1);
    doc["RestartCount"] = Value::from(57);
    doc
}

pub fn inspect_json(docs: &[Value]) -> CommandResult {
    ok(&Value::Array(docs.to_vec()).to_string())
}

pub fn is_docker(sub: &'static str) -> impl Fn(&[String]) -> bool + Send + Sync + 'static {
    move |a: &[String]| a.len() >= 2 && a[0] == "docker" && a[1] == sub
}

pub fn is_logs(a: &[String]) -> bool {
    a.len() >= 2 && a[0] == "docker" && a[1] == "logs"
}

/// `wire(runner, docs, ps)`.
pub fn wire(runner: &FakeRunner, docs: Vec<Value>, ps: &str) {
    runner.on_fn(is_docker("inspect"), move |argv: &[String]| {
        let want: Vec<&String> = argv[2..].iter().collect();
        let hit: Vec<Value> = docs
            .iter()
            .filter(|d| want.iter().any(|w| **w == name_of(d)))
            .cloned()
            .collect();
        inspect_json(&hit)
    });
    runner.on(is_docker("ps"), ok(ps));
}

pub fn wire_worker_exec(runner: &FakeRunner, health: Option<&str>) {
    let h = health.map_or_else(|| fx("worker_health.json"), str::to_owned);
    let body = format!("{}\n---METRICS---\n{}", h.trim(), fx("worker_metrics.prom"));
    runner.on(is_docker("exec"), ok(&body));
}

pub fn http_live(mock: &FakeHttp, models: bool) {
    mock.get("/health", Reply::text(200, &fx("frontend_health.json")));
    mock.get(
        "/v1/models",
        if models {
            Reply::json(200, &fxj("frontend_models.json"))
        } else {
            Reply::json(200, &json!({"object": "list", "data": []}))
        },
    );
    mock.get("/metrics", Reply::text(200, &fx("frontend_metrics.prom")));
}

pub fn codes(st: &SystemStatus) -> Vec<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

/// Lifecycle and validation compose calls; the read-only GPU probe (`config --format json`) is not one.
pub fn compose_calls(runner: &FakeRunner) -> Vec<Vec<String>> {
    runner
        .argvs()
        .into_iter()
        .filter(|a| {
            a.len() >= 2
                && a[0] == "docker"
                && a[1] == "compose"
                && a[a.len().saturating_sub(3)..] != ["config", "--format", "json"]
        })
        .collect()
}

pub fn files_of(argv: &[String]) -> Vec<String> {
    argv.iter()
        .enumerate()
        .filter(|(_, a)| *a == "-f")
        .map(|(i, _)| argv[i + 1].clone())
        .collect()
}

/// The conftest fixtures: the compose copy, the runner (label dir rewritten), the fake frontend and `make`.
pub struct Fix {
    pub tmp: tempfile::TempDir,
    pub compose: PathBuf,
    pub runner: Arc<FakeRunner>,
    pub mock: FakeHttp,
}

impl Fix {
    pub async fn new() -> Fix {
        let tmp = tempfile::tempdir().unwrap();
        let compose = tmp.path().join("repo").join("docker-compose.yml");
        std::fs::create_dir(compose.parent().unwrap()).unwrap();
        std::fs::write(&compose, fx("base_compose.yml")).unwrap();
        let runner = FakeRunner::new();
        *runner.rewrite.lock().unwrap() = Some((
            CAPTURED_DIR.to_owned(),
            compose.parent().unwrap().display().to_string(),
        ));
        let mock = FakeHttp::start().await;
        Fix {
            tmp,
            compose,
            runner,
            mock,
        }
    }

    pub fn state(&self) -> PathBuf {
        self.tmp.path().join("state")
    }

    /// `make(**kw)`: poll 0, health_timeout 0.2, follow 0, plus the caller's changes.
    pub fn make_with(&self, f: impl FnOnce(&mut DynamoAdapter)) -> Arc<DynamoAdapter> {
        let state = self.state();
        std::fs::create_dir_all(&state).unwrap();
        let c = ctx(
            self.runner.clone(),
            &[("dynamo", &self.mock.uri())],
            &[("dynamo_compose", self.compose.clone())],
            &state,
        );
        let mut a = DynamoAdapter::new(c);
        a.poll_interval = 0.0;
        a.health_timeout = 0.2;
        a.follow_interval = 0.0;
        f(&mut a);
        Arc::new(a)
    }

    pub fn make(&self) -> Arc<DynamoAdapter> {
        self.make_with(|_| {})
    }
}

pub fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}
