//! Port of backend/tests/core/test_dynamo_policy.py (core-owned ids): the Dynamo card policy defaults come from
//! Settings, and the topology draws opt-in engines as off. `test_settings_reach_the_registry_built_dynamo_adapter`
//! belongs to workstream A (tests/wsa_core_dynamo_policy.rs).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
mod support_b;

use std::sync::Arc;

use common::Env;
use indexmap::IndexMap;
use nemo_console::adapters::base::CommandResult;
use nemo_console::config::{Settings, TopologyEngine};
use nemo_console::domain::models::{Health, SystemId};
use nemo_console::services::engines::EngineProbe;
use nemo_console::services::status::StatusService;
use nemo_console::services::topology::TopologyService;
use serde_json::{Map, Value};
use support_b::{HookRunner, http, use_runner};

const FN: &str = "ai-lab-sglang-flashnext-flashnext-1";
const QWEN: &str = "ai-lab-vllm-qwen38-vllm-1";
const LIGHTNING: &str = "ai-lab-vllm-nemotron-lightning-nemotron-lightning-1";
const BOLT: &str = "ai-lab-vllm-nemotron-bolt-nemotron-bolt-1";
const GLM: &str = "lil-glm53-tp2";

fn env_settings(pairs: &[(&str, &str)]) -> Result<Settings, String> {
    let env: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Settings::from_sources(None, &env, Map::new()).map_err(|e| e.to_string())
}

#[test]
fn test_defaults_describe_the_lab_and_lists_replace() {
    let s = env_settings(&[]).unwrap();
    assert!(s.dynamo_opt_in);
    assert_eq!(s.dynamo_start, "scripts/use-model.sh qwen3.8-27b-dynamo");
    // neutral: without an alias the script only lists the engines (no fix may tear down Super VL)
    assert!(
        s.default_model_start
            .starts_with("scripts/use-model.sh   # lists the engines")
    );
    assert_eq!(s.dynamo_exclusive_engines, [BOLT, GLM, FN, QWEN]);
    assert_eq!(
        s.dynamo_coexisting_engines,
        [
            LIGHTNING,
            "ai-lab-ollama-ollama-1",
            "ai-lab-chatterbox-tts-tts-1"
        ]
    );
    assert_eq!(
        s.dynamo_ignored_containers,
        ["ai-lab-observability-dcgm-exporter-1"]
    );
    // Python also asserts `CardPolicy() == CardPolicy(<these settings>)`: CardPolicy is the Dynamo adapter's type,
    // so that half is asserted by workstream A next to the adapter (tests/dynamo_*.rs); the settings half is here.
    let s2 = env_settings(&[
        (
            "NEMO_CONSOLE_DYNAMO_EXCLUSIVE_ENGINES",
            r#"["only-this-1"]"#,
        ),
        ("NEMO_CONSOLE_DYNAMO_OPT_IN", "false"),
    ])
    .unwrap();
    assert_eq!(s2.dynamo_exclusive_engines, ["only-this-1"]); // replaced, not merged
    assert!(!s2.dynamo_opt_in);
}

fn nodes(topo: &Value) -> IndexMap<String, Value> {
    topo["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap().to_owned(), n.clone()))
        .collect()
}

fn edges(topo: &Value) -> IndexMap<(String, String), Value> {
    topo["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                (
                    e["src"].as_str().unwrap().to_owned(),
                    e["dst"].as_str().unwrap().to_owned(),
                ),
                e.clone(),
            )
        })
        .collect()
}

fn key(a: &str, b: &str) -> (String, String) {
    (a.to_owned(), b.to_owned())
}

#[tokio::test]
async fn test_topology_draws_opt_in_engines_off_and_shows_lightning() {
    let mut env = Env::new(None);
    env.a(SystemId::Dynamo).knobs().health = Some(Health::Off);
    // e.g. capable tier dark: still routing, still the default path
    env.a(SystemId::Switchyard).knobs().health = Some(Health::Degraded);
    use_runner(
        &mut env,
        HookRunner::states(&[(FN, "running"), (QWEN, "exited"), (LIGHTNING, "running")]),
    );
    let topo = env.get("/api/v1/topology").send().await.json();
    let n = nodes(&topo);
    assert_eq!(n["dynamo-frontend"]["health"], "off");
    assert_eq!(n["vllm-qwen38"]["health"], "off");
    assert_eq!(n["sglang-flashnext"]["health"], "up");
    assert_eq!(n["vllm-lightning"]["health"], "up");
    assert!(
        n["vllm-lightning"]["label"]
            .as_str()
            .unwrap()
            .contains("efficient")
    );
    let e = edges(&topo);
    assert_eq!(e[&key("litellm", "vllm-lightning")]["active"], true);
    assert_eq!(e[&key("litellm", "vllm-qwen38")]["active"], false);
    assert_eq!(e[&key("litellm", "dynamo-frontend")]["active"], false);
    let sy = &e[&key("clients", "switchyard")];
    assert!(
        sy["label"] == "default"
            && sy["active"] == true
            && sy["label"].as_str().unwrap().len() <= 9
    );
    // the always-on engine stopped is a fault; the opt-in ones are only off (fresh caches: a rebuilt state)
    use_runner(
        &mut env,
        HookRunner::states(&[(FN, "exited"), (QWEN, "exited"), (LIGHTNING, "exited")]),
    );
    let n = nodes(&env.get("/api/v1/topology").send().await.json());
    assert_eq!(n["vllm-lightning"]["health"], "down");
    assert_eq!(n["sglang-flashnext"]["health"], "off");
    assert_eq!(n["vllm-qwen38"]["health"], "off");
}

#[tokio::test]
async fn test_topology_shows_missing_containers_as_not_deployed_never_red() {
    let mut env = Env::new(None);
    use_runner(
        &mut env,
        HookRunner::new(|argv| {
            let last = argv.last().map_or("", String::as_str);
            if [FN, QWEN, LIGHTNING].contains(&last) {
                Ok(CommandResult::new(
                    1,
                    "",
                    &format!("Error: No such object: {last}\n"),
                ))
            } else {
                Ok(CommandResult::new(0, "running\n", ""))
            }
        }),
    );
    let topo = env.get("/api/v1/topology").send().await.json();
    let n = nodes(&topo);
    for nid in ["sglang-flashnext", "vllm-qwen38", "vllm-lightning"] {
        assert_eq!(n[nid]["health"], "off", "{nid}");
        assert!(
            n[nid]["label"]
                .as_str()
                .unwrap()
                .ends_with("(not deployed)"),
            "{nid}"
        );
    }
    assert_eq!(n["vllm-bolt"]["health"], "up");
    assert!(
        !n["vllm-bolt"]["label"]
            .as_str()
            .unwrap()
            .contains("not deployed")
    );
    let e = edges(&topo);
    assert_eq!(e[&key("litellm", "vllm-lightning")]["active"], false);
    assert_eq!(e[&key("litellm", "vllm-bolt")]["active"], true);
}

#[test]
fn test_topology_engines_are_a_setting() {
    let s = env_settings(&[(
        "NEMO_CONSOLE_TOPOLOGY_ENGINES",
        r#"[{"id": "my-engine", "label": "My engine", "container": "my-engine-1", "opt_in": false}, {"id": "litellm", "label": "shadow", "container": "x-1"}, {"id": "my-engine", "label": "dup", "container": "y-1"}]"#,
    )])
    .unwrap();
    let ids: Vec<&str> = s.topology_engines.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["my-engine", "litellm", "my-engine"]); // replaced, not merged
    let runner = HookRunner::states(&[]);
    let status = Arc::new(StatusService::new(
        IndexMap::new(),
        2.0,
        8.0,
        vec![],
        std::collections::HashMap::new(),
    ));
    let t = TopologyService::new(
        status,
        http(),
        Arc::new(EngineProbe::new(runner)),
        "http://127.0.0.1:4000",
        &s.topology_engines,
    );
    let kept: Vec<(&str, &str, bool)> = t
        .engines
        .iter()
        .map(|e: &TopologyEngine| (e.id.as_str(), e.container.as_str(), e.opt_in))
        .collect();
    assert_eq!(kept, [("my-engine", "my-engine-1", false)]);
    assert!(
        env_settings(&[(
            "NEMO_CONSOLE_TOPOLOGY_ENGINES",
            r#"[{"id": "Bad Id", "label": "x", "container": "c-1"}]"#
        )])
        .is_err()
    );
}
