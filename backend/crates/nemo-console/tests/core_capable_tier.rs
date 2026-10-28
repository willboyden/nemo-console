//! Port of backend/tests/core/test_capable_tier.py: the Switchyard capable-tier check (engine probe + parsed routes).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
mod support_b;

use std::sync::Arc;

use async_trait::async_trait;
use common::Env;
use indexmap::IndexMap;
use nemo_console::adapters::base::{CommandResult, CommandRunner, RunOpts, RunnerError};
use nemo_console::adapters::switchyard_capable::capable_models;
use nemo_console::config::{
    BOLT, DEFAULT_ENGINE_PROBES, DEFAULT_ENGINE_START, DYNAMO_WORKER, GLM, LIGHTNING, Settings,
};
use nemo_console::domain::models::SystemId;
use nemo_console::services::engines::{
    CONTAINER_RE, EngineProbe, EngineState, capable_tier_problems, compose_dir_hint,
};
use nemo_console::services::pytoml::{Table, TomlVal, loads};
use serde_json::{Map, Value};
use support_b::{HookRunner, use_runner};

const ALIAS: &str = "qwen3.8-27b-1m"; // the 27B: still probed, dark by default since 2026-09-26
const NAME: &str = "ai-lab-vllm-qwen38-vllm-1";
const FN_ALIAS: &str = "qwen3.8-flash-next"; // the default engine and capable tier since 2026-09-26
const FN_NAME: &str = "ai-lab-sglang-flashnext-flashnext-1";

const ROUTES: &str = r#"
[targets.lightning]
id = "nemotron-lightning"
llm_client = "litellm"

[targets.qwen-27b-1m]
id = "qwen3.8-27b-1m"
llm_client = "litellm"

[targets.flash-next]
id = "qwen3.8-flash-next"
llm_client = "litellm"

[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "CAPABLE"
efficient_target = "lightning"

[routes.direct-27b]
id = "direct-27b"
type = "passthrough"
target = "qwen-27b-1m"
"#;

fn routes(capable: &str) -> String {
    ROUTES.replace("CAPABLE", capable)
}

async fn status(env: &Env) -> Value {
    env.state.status.invalidate(SystemId::Switchyard);
    env.get("/api/v1/systems/switchyard/status")
        .send()
        .await
        .json()
}

fn codes(st: &Value) -> Vec<String> {
    st["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["code"].as_str().unwrap().to_owned())
        .collect()
}

fn dark(st: &Value) -> Vec<Value> {
    st["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["code"] == "switchyard.tier.capable_dark")
        .cloned()
        .collect()
}

fn write(env: &Env, text: &str) {
    std::fs::write(&env.cfg_file, text).unwrap();
}

fn root(env: &Env) -> String {
    env.settings.repo_root.display().to_string()
}

#[tokio::test]
async fn test_running_engine_no_problem() {
    let env = Env::new(None);
    write(&env, &routes("flash-next"));
    let st = status(&env).await;
    assert!(
        !codes(&st).contains(&"switchyard.tier.capable_dark".to_owned()) && st["health"] == "up"
    );
}

#[tokio::test]
async fn test_stopped_capable_engine_degrades_with_its_own_start_command() {
    let env = Env::new(None);
    write(&env, &routes("flash-next"));
    env.runner.set(Ok(CommandResult::new(0, "exited\n", ""))); // EVERY probed engine reads stopped
    let st = status(&env).await;
    let d = dark(&st);
    assert_eq!(d.len(), 1, "{st}"); // only the capable target, not the passthrough-only 27B
    let p = &d[0];
    assert_eq!(p["severity"], "warn");
    assert_eq!(
        p["message"],
        format!(
            "Capable tier target {FN_ALIAS} is not answering: engine container {FN_NAME} is not running, so route auto-local cannot escalate to it"
        )
    );
    let fix = p["fix"].as_str().unwrap();
    assert!(fix.starts_with(&format!(
        "cd {} && inference/sglang-flashnext/flashnext.sh up   # only if",
        root(&env)
    )));
    assert!(!fix.contains("docker compose up"));
    assert_eq!(st["health"], "degraded");
    assert!(
        st["summary"]
            .as_str()
            .unwrap()
            .starts_with("capable tier dark")
    );
}

#[tokio::test]
async fn test_passthrough_only_alias_is_not_a_capable_tier() {
    // the old check was `alias in routes_text`, which the 27B always satisfies through direct-27b
    let mut env = Env::new(None);
    let text = routes("flash-next");
    assert!(text.contains(ALIAS));
    write(&env, &text);
    let runner = HookRunner::states(&[(FN_NAME, "running"), (NAME, "exited")]);
    use_runner(&mut env, runner.clone());
    let st = status(&env).await;
    assert!(dark(&st).is_empty() && st["health"] == "up");
    let inspect_27b: Vec<String> = ["docker", "inspect", "-f", "{{.State.Status}}", NAME]
        .map(str::to_owned)
        .to_vec();
    assert!(!runner.calls().contains(&inspect_27b)); // not even probed
}

#[tokio::test]
async fn test_27b_as_capable_target_points_at_use_model() {
    let mut env = Env::new(None);
    write(&env, &routes("qwen-27b-1m"));
    use_runner(
        &mut env,
        HookRunner::states(&[(FN_NAME, "running"), (NAME, "exited")]),
    );
    let d = dark(&status(&env).await);
    assert_eq!(d.len(), 1);
    assert!(d[0]["message"].as_str().unwrap().starts_with(&format!(
        "Capable tier target {ALIAS} is not answering: engine container {NAME}"
    )));
    let fix = d[0]["fix"].as_str().unwrap();
    assert!(fix.starts_with(&format!(
        "cd {} && scripts/use-model.sh qwen3.8-27b-1m",
        root(&env)
    )));
    assert!(!fix.contains("docker compose up"));
}

#[tokio::test]
async fn test_absent_container_is_not_deployed_information_not_a_dark_tier() {
    let env = Env::new(None);
    write(&env, &routes("flash-next"));
    env.runner
        .set(Ok(CommandResult::new(1, "", "Error: No such object: x\n")));
    let st = status(&env).await;
    assert!(!codes(&st).contains(&"switchyard.tier.capable_dark".to_owned()));
    assert_eq!(st["health"], "up");
    assert_eq!(st["summary"], "fake");
    let nd: Vec<&Value> = st["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["code"] == "switchyard.tier.capable_not_deployed")
        .collect();
    assert_eq!(nd.len(), 1);
    let p = nd[0];
    assert_eq!(p["severity"], "info");
    let msg = p["message"].as_str().unwrap();
    assert!(msg.contains(&format!("no container {FN_NAME}")) && msg.contains("route auto-local"));
    assert!(
        p["fix"]
            .as_str()
            .unwrap()
            .starts_with("point route auto-local at a deployed engine in routes.toml, or deploy")
    );
}

#[tokio::test]
async fn test_stopped_but_alias_not_in_routes_no_problem() {
    let env = Env::new(None);
    write(&env, "id = \"something-else\"\n");
    env.runner.set(Ok(CommandResult::new(0, "exited\n", "")));
    let st = status(&env).await;
    assert!(
        !codes(&st).contains(&"switchyard.tier.capable_dark".to_owned()) && st["health"] == "up"
    );
}

#[tokio::test]
async fn test_unparseable_routes_no_problem() {
    let env = Env::new(None);
    write(&env, &(routes("flash-next") + "\n[routes.broken\n"));
    env.runner.set(Ok(CommandResult::new(0, "exited\n", "")));
    let st = status(&env).await;
    assert!(
        !codes(&st).contains(&"switchyard.tier.capable_dark".to_owned()) && st["health"] == "up"
    );
}

#[tokio::test]
async fn test_probe_error_is_not_a_problem_and_never_raises() {
    let mut env = Env::new(None);
    write(&env, &routes("flash-next"));
    use_runner(
        &mut env,
        HookRunner::new(|_| Err(RunnerError::Os("OSError", "no docker".to_owned()))),
    );
    let st = status(&env).await;
    assert!(
        !codes(&st).contains(&"switchyard.tier.capable_dark".to_owned()) && st["health"] == "up"
    );
    let failing: Arc<dyn CommandRunner> = HookRunner::new(|_| {
        Ok(CommandResult::new(
            1,
            "",
            "Cannot connect to the Docker daemon",
        ))
    });
    let p = EngineProbe::new(failing);
    assert_eq!(p.state(NAME).await, EngineState::Unknown);
    assert_eq!(p.state("bad name; rm -rf").await, EngineState::Unknown);
}

#[tokio::test]
async fn test_topology_and_status_share_one_probe() {
    let mut env = Env::new(None);
    write(&env, &routes("flash-next"));
    let runner = HookRunner::states(&[(FN_NAME, "exited"), (NAME, "exited")]);
    use_runner(&mut env, runner.clone());
    env.get("/api/v1/systems/switchyard/status").send().await;
    let topo = env.get("/api/v1/topology").send().await.json();
    let node = |id: &str| -> Value {
        topo["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .cloned()
            .unwrap()
    };
    // flashnext and the 27B are opt-in now (2026-10-17): stopped is off, not red
    assert_eq!(node("sglang-flashnext")["health"], "off");
    assert_eq!(node("vllm-qwen38")["health"], "off");
    assert!(
        node("sglang-flashnext")["label"]
            .as_str()
            .unwrap()
            .contains("both cards")
    );
    assert!(
        topo["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["src"] == "litellm"
                && e["dst"] == "sglang-flashnext"
                && e["active"] == false)
    );
    let mut inspected: Vec<String> = runner
        .calls()
        .iter()
        .filter(|x| x.len() >= 2 && x[0] == "docker" && x[1] == "inspect")
        .map(|x| x.last().unwrap().clone())
        .collect();
    inspected.sort();
    let mut want: Vec<String> = [FN_NAME, NAME, LIGHTNING, BOLT, GLM]
        .map(str::to_owned)
        .to_vec();
    want.sort();
    // one call per container: status and topology share it
    assert_eq!(inspected, want);
}

struct Stopped;

#[async_trait]
impl CommandRunner for Stopped {
    async fn run(&self, _argv: &[String], _opts: RunOpts) -> Result<CommandResult, RunnerError> {
        Ok(CommandResult::new(0, "exited\n", ""))
    }
}

#[tokio::test]
async fn test_routes_listed_and_generic_hint_for_unknown_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(root.join("inference").join("myengine")).unwrap();
    let text = routes("flash-next")
        + "\n[routes.auto-two]\nid = \"auto-two\"\ntype = \"stage_router\"\ncapable_target = \"flash-next\"\n";
    let probe = EngineProbe::new(Arc::new(Stopped));
    let probes: IndexMap<String, String> =
        [(FN_ALIAS.to_owned(), "ai-lab-myengine-srv-1".to_owned())]
            .into_iter()
            .collect();
    let ps = capable_tier_problems(&probe, &probes, &text, &root, &IndexMap::new()).await;
    assert_eq!(ps.len(), 1);
    assert!(
        ps[0]
            .message
            .ends_with("so routes auto-local, auto-two cannot escalate to it")
    );
    assert_eq!(
        ps[0].fix.as_deref(),
        Some("cd inference/myengine && docker compose up -d")
    ); // no start command
    let probes: IndexMap<String, String> = [(FN_ALIAS.to_owned(), FN_NAME.to_owned())]
        .into_iter()
        .collect();
    let start: IndexMap<String, String> = DEFAULT_ENGINE_START
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let qs = capable_tier_problems(&probe, &probes, &text, &root, &start).await;
    assert_eq!(qs.len(), 1);
    assert!(qs[0].fix.as_deref().unwrap().starts_with(&format!(
        "cd {} && inference/sglang-flashnext/flashnext.sh up   # only if",
        root.display()
    )));
}

#[test]
fn test_capable_models_parses_only_stage_router_capable_targets() {
    let parse = |t: &str| -> Table { loads(t).unwrap() };
    let one = |k: &str| -> IndexMap<String, Vec<String>> {
        [(k.to_owned(), vec!["auto-local".to_owned()])]
            .into_iter()
            .collect()
    };
    assert_eq!(capable_models(&parse(&routes("flash-next"))), one(FN_ALIAS));
    assert_eq!(capable_models(&parse(&routes("qwen-27b-1m"))), one(ALIAS));
    assert!(capable_models(&parse(&routes("no-such-target"))).is_empty());
    // malformed shapes never fail
    let mut bad = Table::new();
    bad.insert("routes".to_owned(), TomlVal::Array(vec![]));
    bad.insert("targets".to_owned(), TomlVal::Int(5));
    assert!(capable_models(&bad).is_empty());
    let bad2: Table =
        loads("[routes.r]\ntype = \"stage_router\"\ncapable_target = [\"x\"]\n[targets]\n")
            .unwrap();
    assert!(capable_models(&bad2).is_empty());
}

#[tokio::test]
async fn test_dir_hint_and_validation() {
    let env = Env::new(None);
    let root = env.tmp().join("repo");
    std::fs::create_dir_all(root.join("inference").join("vllm-qwen38")).unwrap();
    assert_eq!(
        compose_dir_hint(&root, NAME).as_deref(),
        Some("inference/vllm-qwen38")
    );
    assert_eq!(compose_dir_hint(&root, "ai-lab-zzz-1"), None);
    assert!(!CONTAINER_RE.is_match("a\n"));
}

fn env_settings(pairs: &[(&str, &str)]) -> Settings {
    let env: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Settings::from_sources(None, &env, Map::new()).unwrap()
}

fn defaults(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn test_engine_probes_env_merges() {
    let want: IndexMap<String, String> = defaults(&[
        ("nemotron-3.5-super-vl", DYNAMO_WORKER),
        ("nemotron-3.5-bolt", BOLT),
        ("glm-5.3-flash-next-local", GLM),
        (FN_ALIAS, FN_NAME),
        (ALIAS, NAME),
    ]);
    assert_eq!(defaults(&DEFAULT_ENGINE_PROBES), want);
    // env keys are lower-cased by pydantic-settings
    let s = env_settings(&[("NEMO_CONSOLE_ENGINE_PROBES__other", "ai-lab-x-1")]);
    let mut merged = defaults(&DEFAULT_ENGINE_PROBES);
    merged.insert("other".to_owned(), "ai-lab-x-1".to_owned());
    assert_eq!(s.engine_probes, merged);
    // the JSON form allows dotted aliases; still merged
    let s = env_settings(&[("NEMO_CONSOLE_ENGINE_PROBES", r#"{"a.b": "ai-lab-y-1"}"#)]);
    assert_eq!(s.engine_probes["a.b"], "ai-lab-y-1");
    assert!(s.engine_probes.contains_key(ALIAS) && s.engine_probes.contains_key(FN_ALIAS));
}

#[test]
fn test_engine_start_env_merges() {
    let keys: std::collections::BTreeSet<&str> =
        DEFAULT_ENGINE_START.iter().map(|(k, _)| *k).collect();
    let want: std::collections::BTreeSet<&str> = [DYNAMO_WORKER, BOLT, GLM, FN_NAME, NAME]
        .into_iter()
        .collect();
    assert_eq!(keys, want);
    assert!(
        DEFAULT_ENGINE_START
            .iter()
            .all(|(_, c)| !c.contains("docker compose up"))
    );
    assert!(DEFAULT_ENGINE_START.iter().all(|(_, c)| c.contains('#'))); // each says what it stops or needs
    let s = env_settings(&[(
        "NEMO_CONSOLE_ENGINE_START",
        r#"{"ai-lab-x-1": "scripts/start-x.sh"}"#,
    )]);
    let mut merged = defaults(&DEFAULT_ENGINE_START);
    merged.insert("ai-lab-x-1".to_owned(), "scripts/start-x.sh".to_owned());
    assert_eq!(s.engine_start, merged);
}
