//! Integration rows (written after the adapter merges) of backend/tests/core/test_deployment_settings.py (PORTING.md section 10): the
//! settings' defaults equal the adapters' former module constants, and the registry configures every adapter from
//! the settings. No downcasting (A-6): each `build` is `Adapter::from_settings`, whose fields and accessors are
//! checked here, and the container-built adapters are checked by behaviour through the `SystemAdapter` trait.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::FakeRunner;
use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, CommandRunner};
use nemo_console::adapters::dcgm::{DcgmAdapter, DcgmNames};
use nemo_console::adapters::dynamo::{self, DynamoAdapter};
use nemo_console::adapters::guardrails::{self, GuardrailsAdapter};
use nemo_console::adapters::relay::{self, RelayAdapter};
use nemo_console::adapters::switchyard::{self, SwitchyardAdapter};
use nemo_console::config::{DEFAULT_LAYOUT, Settings, settings_for_test};
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::SystemId;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Value, json};

fn closed_urls() -> Value {
    let keys = [
        "switchyard",
        "relay",
        "dynamo",
        "dynamo_worker_metrics",
        "litellm",
        "dcgm",
        "prometheus",
        "guardrails",
    ];
    Value::Object(
        keys.iter()
            .map(|k| ((*k).to_owned(), json!("http://127.0.0.1:9")))
            .collect(),
    )
}

fn ctx(tmp: &std::path::Path, runner: Arc<FakeRunner>) -> AdapterContext {
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls: IndexMap::new(),
        paths: IndexMap::new(),
        secret_env: IndexMap::new(),
        state_dir: tmp.join("state"),
        environ: Environ::from_pairs(&[]),
    }
}

fn s(v: &[(&str, &str)]) -> IndexMap<String, String> {
    v.iter()
        .map(|(k, x)| ((*k).to_owned(), (*x).to_owned()))
        .collect()
}

#[test]
fn test_defaults_equal_the_adapters_former_constants() {
    let s0 = settings_for_test(json!({}));
    assert_eq!(s0.switchyard_container, switchyard::CONTAINER);
    assert_eq!(
        (
            s0.switchyard_compose_project.as_str(),
            s0.switchyard_compose_service.as_str()
        ),
        (switchyard::COMPOSE_PROJECT, switchyard::COMPOSE_SERVICE)
    );
    assert_eq!(
        (
            s0.switchyard_image.as_str(),
            s0.switchyard_log_path.as_str()
        ),
        (switchyard::FALLBACK_IMAGE, switchyard::LOG_PATH)
    );
    assert_eq!(s0.relay_unit, relay::UNIT);
    assert_eq!(s0.dynamo_compose_project, dynamo::PROJECT);
    assert_eq!(
        s0.dynamo_containers,
        s(&[
            ("frontend", dynamo::FRONTEND),
            ("worker", dynamo::WORKER),
            ("mocker", dynamo::MOCKER),
            ("sv_frontend", dynamo::SV_FRONTEND)
        ])
    );
    assert_eq!(
        s0.dynamo_services,
        s(&[
            ("frontend", dynamo::SVC_FRONTEND),
            ("worker", dynamo::SVC_WORKER),
            ("mocker", dynamo::SVC_MOCKER),
            ("sv_frontend", dynamo::SVC_SV_FRONTEND)
        ])
    );
    assert_eq!(s0.guardrails_compose_project, guardrails::COMPOSE_PROJECT);
    assert_eq!(
        s0.dcgm_compose_project,
        DcgmNames::default().compose_project
    );
    assert_eq!(s0.layout, s(&DEFAULT_LAYOUT));
    assert!(!s0.capable_probe);
}

fn configured(tmp: &std::path::Path) -> Settings {
    settings_for_test(json!({
        "state_dir": tmp.join("state").to_string_lossy(), "background_tasks": false, "urls": closed_urls(),
        "switchyard_container": "sy-1", "switchyard_compose_project": "sy", "switchyard_compose_service": "svc",
        "switchyard_image": "sy:1", "switchyard_log_path": "/logs/r.jsonl", "relay_unit": "relay.service",
        "dynamo_compose_project": "dyn", "dynamo_containers": {"worker": "w-1", "frontend": "f-1"},
        "dynamo_services": {"worker": "w", "frontend": "f"}, "guardrails_compose_project": "gr",
        "dcgm_compose_project": "obs",
    }))
}

#[tokio::test]
async fn test_container_configures_the_registry_built_adapters() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = configured(tmp.path());
    let runner = Arc::new(FakeRunner::default());
    let c = ctx(tmp.path(), runner.clone());
    // what every registry `build` returns
    let sy = SwitchyardAdapter::from_settings(&c, &cfg);
    assert_eq!(
        (
            sy.container.as_str(),
            sy.compose_project.as_str(),
            sy.compose_service.as_str(),
            sy.fallback_image.as_str(),
            sy.log_path.as_str()
        ),
        ("sy-1", "sy", "svc", "sy:1", "/logs/r.jsonl")
    );
    assert_eq!(RelayAdapter::from_settings(&c, &cfg).unit, "relay.service");
    let dy = DynamoAdapter::from_settings(&c, &cfg);
    assert_eq!(
        (
            dy.names.project.as_str(),
            dy.names.worker.as_str(),
            dy.names.frontend.as_str(),
            dy.names.svc_worker.as_str()
        ),
        ("dyn", "w-1", "f-1", "w")
    );
    assert_eq!(dy.names.mocker, dynamo::MOCKER); // unset roles keep the default
    assert_eq!(dy.policy.own, dy.names.all());
    assert!(dy.policy.own.contains(&"w-1".to_owned()));
    assert_eq!(
        GuardrailsAdapter::from_settings(&c, &cfg).compose_project(),
        "gr"
    );
    assert_eq!(
        DcgmAdapter::from_settings(&c, &cfg).names().compose_project,
        "obs"
    );

    // ... and the container's real registry uses them: behaviour through the trait
    let ov = Overrides {
        adapters: std::collections::HashMap::new(),
        runner: Some(runner.clone() as Arc<dyn CommandRunner>),
        http: Some(Http::new().unwrap()),
        registry: None,
        environ: Some(Environ::from_pairs(&[])),
    };
    let state = build_container(cfg, ov).unwrap();
    for sid in [SystemId::Switchyard, SystemId::Relay, SystemId::Dynamo] {
        let _ = state.adapter(sid).unwrap().status().await;
    }
    let calls = runner.calls();
    let has = |needle: &str| calls.iter().any(|argv| argv.iter().any(|a| a == needle));
    assert!(
        has("sy-1"),
        "switchyard did not inspect its configured container: {calls:?}"
    );
    assert!(
        has("relay.service"),
        "relay did not ask about its configured unit: {calls:?}"
    );
    assert!(
        has("w-1") || has("f-1"),
        "dynamo did not inspect its configured containers: {calls:?}"
    );
}
