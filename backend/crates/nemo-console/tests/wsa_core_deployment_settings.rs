//! workstream A's share of backend/tests/core/test_deployment_settings.py: the Switchyard, Relay and Dynamo adapters act on
//! the CONFIGURED deployment names (addendum 9.11), not the old module constants.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::FakeRunner;
use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, CommandResult};
use nemo_console::adapters::dynamo::{
    DynamoAdapter, DynamoNames, merged_worker_uuids, render_override,
};
use nemo_console::adapters::relay::RelayAdapter;
use nemo_console::adapters::switchyard::SwitchyardAdapter;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::json;

fn ctx(
    tmp: &Path,
    runner: Arc<FakeRunner>,
    paths: &[(&str, std::path::PathBuf)],
) -> AdapterContext {
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls: IndexMap::new(),
        paths: paths
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        secret_env: IndexMap::new(),
        state_dir: tmp.join("state"),
        environ: Environ::from_pairs(&[]),
    }
}

#[tokio::test]
async fn test_switchyard_checks_the_configured_compose_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = Arc::new(FakeRunner::default());
    runner.set(Ok(CommandResult::new(0, "running||img:1|sy|svc\n", "")));
    let a = SwitchyardAdapter::new(ctx(tmp.path(), runner.clone(), &[]));
    assert_eq!(a.inspect().await.unwrap(), None); // default labels do not match sy/svc
    let mut b = SwitchyardAdapter::new(ctx(tmp.path(), runner.clone(), &[]));
    b.container = "sy-1".into();
    b.compose_project = "sy".into();
    b.compose_service = "svc".into();
    assert_eq!(
        b.inspect().await.unwrap(),
        Some(("running".into(), String::new(), "img:1".into()))
    );
    assert_eq!(runner.calls().last().unwrap().last().unwrap(), "sy-1");
}

#[tokio::test]
async fn test_relay_asks_systemd_about_the_configured_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = Arc::new(FakeRunner::default());
    let mut a = RelayAdapter::new(ctx(tmp.path(), runner.clone(), &[]));
    a.unit = "relay@main.service".into();
    a.unit_state().await;
    assert_eq!(
        runner.calls().last().unwrap()[..5],
        [
            "systemctl",
            "--user",
            "--timestamp=unix",
            "show",
            "relay@main.service"
        ]
    );
}

#[tokio::test]
async fn test_dynamo_inspects_the_configured_containers() {
    let tmp = tempfile::tempdir().unwrap();
    let compose = tmp.path().join("dyn").join("docker-compose.yml");
    std::fs::create_dir(compose.parent().unwrap()).unwrap();
    std::fs::write(&compose, "services: {}\n").unwrap();
    let runner = Arc::new(FakeRunner::default());
    runner.set(Ok(CommandResult::new(0, "[]", "")));
    let mut a = DynamoAdapter::new(ctx(
        tmp.path(),
        runner.clone(),
        &[("dynamo_compose", compose)],
    ));
    a.names = DynamoNames {
        project: "dyn".into(),
        frontend: "f-1".into(),
        worker: "w-1".into(),
        mocker: "m-1".into(),
        sv_frontend: "s-1".into(),
        ..DynamoNames::default()
    };
    a.compose_files(None).await.unwrap();
    assert_eq!(
        runner.calls().last().unwrap(),
        &["docker", "inspect", "w-1", "f-1", "s-1", "m-1"]
    );
}

#[test]
fn test_render_and_gpu_parse_use_the_configured_services() {
    let base: Vec<String> = ["python3", "-m", "dynamo.vllm", "--model", "m"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let values = json!({"max_model_len": 4096});
    let (text, _) = render_override(values.as_object().unwrap(), &base, None, "w", "f").unwrap();
    assert!(text.contains("\n  w:\n") && !text.contains("dynamo-worker"));
    let doc = r#"{"services": {"w": {"deploy": {"resources": {"reservations": {"devices": [{"device_ids": ["GPU-12345678-1234-1234-1234-123456789abc"]}]}}}}}}"#;
    assert_eq!(
        merged_worker_uuids(doc, "w"),
        ["GPU-12345678-1234-1234-1234-123456789abc"]
    );
    assert!(merged_worker_uuids(doc, "dynamo-worker").is_empty()); // the default service is not in this doc
}
