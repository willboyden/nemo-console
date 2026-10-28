//! Port of backend/tests/core/test_deployment_settings.py (the core-owned ids): deployment identities and the repo
//! layout are settings, validated, defaulting to the old constants. The ids that need the real adapters live in
//! tests/wsa_core_deployment_settings.rs (workstream A), tests/wsb_core_deployment_settings.rs (workstream B) and
//! tests/integration_core_deployment_settings.rs (integration, after the merges).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use nemo_console::config::{DYNAMO_WORKER, Settings};
use serde_json::{Map, Value, json};

fn settings(env: &[(&str, &str)], init: Value) -> Result<Settings, String> {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let init = match init {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    Settings::from_sources(None, &env, init).map_err(|e| e.to_string())
}

fn root(tmp: &Path) -> Value {
    Value::String(tmp.to_string_lossy().into_owned())
}

#[test]
fn test_default_layout_reproduces_the_old_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let p = settings(&[], json!({"repo_root": root(t)}))
        .unwrap()
        .resolved_paths();
    assert_eq!(
        p["switchyard_routes"],
        t.join("inference/switchyard/routes.toml")
    );
    assert_eq!(p["relay_routes"], t.join("clients/nemo-relay/routes.toml"));
    assert_eq!(
        p["dynamo_compose"],
        t.join("inference/dynamo/docker-compose.yml")
    );
    assert_eq!(p["skills_dir"], t.join(".claude/skills"));
    assert_eq!(
        p["openshell_policy"],
        t.join("clients/data-designer/runner/policy.yaml")
    );
    assert_eq!(
        p["dcgm_counters"],
        t.join("observability/dcgm/counters.csv")
    );
    assert_eq!(
        p["guardrails_config_dir"],
        t.join("security/nemo-guardrails/config")
    );
    assert_eq!(
        p["evaluator_suites"],
        t.join("clients/nemo-evaluator/suites.yaml")
    );
}

#[test]
fn test_env_overrides_single_keys_and_keeps_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let s = settings(
        &[
            ("NEMO_CONSOLE_SWITCHYARD_CONTAINER", "sy-1"),
            ("NEMO_CONSOLE_RELAY_UNIT", "relay@main.service"),
            ("NEMO_CONSOLE_DYNAMO_CONTAINERS__WORKER", "my-worker"),
            ("NEMO_CONSOLE_LAYOUT__SWITCHYARD", "deploy/switchyard"),
        ],
        json!({"repo_root": root(t)}),
    )
    .unwrap();
    assert_eq!(
        (s.switchyard_container.as_str(), s.relay_unit.as_str()),
        ("sy-1", "relay@main.service")
    );
    assert_eq!(s.dynamo_containers["worker"], "my-worker");
    // dynamo_mod.FRONTEND in Python: the default container of the frontend role
    assert_eq!(
        s.dynamo_containers["frontend"],
        "ai-lab-dynamo-dynamo-frontend-1"
    );
    assert_ne!(s.dynamo_containers["worker"], DYNAMO_WORKER);
    let p = s.resolved_paths();
    assert_eq!(
        p["switchyard_routes"],
        t.join("deploy/switchyard/routes.toml")
    );
    assert_eq!(p["relay_routes"], t.join("clients/nemo-relay/routes.toml")); // untouched keys keep defaults
}

#[test]
fn test_repo_marker_follows_the_dynamo_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    std::fs::create_dir_all(t.join("stacks/dynamo")).unwrap();
    std::fs::write(t.join("stacks/dynamo/docker-compose.yml"), "services: {}\n").unwrap();
    let ok = settings(
        &[],
        json!({"repo_root": root(t), "layout": {"dynamo": "stacks/dynamo"}}),
    )
    .unwrap();
    assert_eq!(ok.repo_root_problem(), None);
    let problem = settings(&[], json!({"repo_root": root(t)}))
        .unwrap()
        .repo_root_problem();
    assert!(
        problem
            .unwrap()
            .contains("inference/dynamo/docker-compose.yml")
    );
}

#[test]
fn test_invalid_identifiers_are_refused() {
    let cases: Vec<(&str, Value)> = vec![
        ("switchyard_container", json!("-rm")),
        ("switchyard_container", json!("a b")),
        ("switchyard_compose_project", json!("Bad")),
        ("switchyard_compose_project", json!("-p")),
        ("switchyard_compose_service", json!("-x")),
        ("switchyard_image", json!("UPPER:1")),
        ("switchyard_log_path", json!("relative/log")),
        ("switchyard_log_path", json!("/var/../etc/passwd")),
        ("switchyard_log_path", json!("/a b")),
        ("relay_unit", json!("nemo-relay-gateway")),
        ("relay_unit", json!("-x.service")),
        ("dynamo_compose_project", json!("x;y")),
        ("guardrails_compose_project", json!("")),
        ("dcgm_compose_project", json!("UP")),
        ("capable_probe_key_env", json!("HOME")),
        ("capable_probe_key_env", json!("AWS_SECRET_ACCESS_KEY")),
        ("capable_probe_interval_s", json!(60.0)),
    ];
    for (field, value) in cases {
        let mut init = Map::new();
        init.insert(field.to_owned(), value.clone());
        assert!(
            settings(&[], Value::Object(init)).is_err(),
            "{field} = {value} was accepted"
        );
    }
}

#[test]
fn test_bad_layout_entries_are_refused() {
    for layout in [
        json!({"switchyard": "../outside"}),
        json!({"switchyard": "/abs/path"}),
        json!({"switchyard": "a//b"}),
        json!({"switchyard": "a/./b"}),
        json!({"switchyard": "a\\b"}),
        json!({"nope": "x"}),
        json!({"relay": ""}),
    ] {
        assert!(
            settings(&[], json!({"layout": layout})).is_err(),
            "{layout} was accepted"
        );
    }
}

#[test]
fn test_bad_dynamo_maps_are_refused() {
    let cases: Vec<(&str, Value)> = vec![
        ("dynamo_containers", json!({"router": "x"})),
        ("dynamo_containers", json!({"worker": "-x"})),
        (
            "dynamo_containers",
            json!({"worker": "ai-lab-dynamo-dynamo-frontend-1"}),
        ), // two roles, one container
        ("dynamo_services", json!({"worker": "--rm"})),
        ("dynamo_services", json!({"gateway": "x"})),
    ];
    for (field, value) in cases {
        let mut init = Map::new();
        init.insert(field.to_owned(), value.clone());
        assert!(
            settings(&[], Value::Object(init)).is_err(),
            "{field} = {value} was accepted"
        );
    }
}
