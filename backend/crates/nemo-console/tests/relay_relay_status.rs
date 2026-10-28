//! Port of backend/tests/relay/test_relay_status.py.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod relay_support;
mod switchyard_support;

use std::collections::{BTreeMap, BTreeSet};

use nemo_console::adapters::base::SystemAdapter;
use nemo_console::domain::models::{Health, Severity, SystemStatus};
use relay_support::{Router, fx, make_adapter, okr, standard_runner, with_path};
use switchyard_support::{PrefixRunner, Reply};

fn weak() -> String {
    fx("plugins.toml").replace(
        "attestation = \"signature_required\"",
        "attestation = \"integrity_only\"",
    )
}

fn no_exporters() -> String {
    fx("plugins.toml")
        .replace(
            "[components.config.atof]\nenabled = true",
            "[components.config.atof]\nenabled = false",
        )
        .replace(
            "[components.config.atif]\nenabled = true",
            "[components.config.atif]\nenabled = false",
        )
        .replace(
            "[components.config.opentelemetry]\nenabled = true",
            "[components.config.opentelemetry]\nenabled = false",
        )
}

fn codes(st: &SystemStatus) -> BTreeSet<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

#[tokio::test]
async fn test_status_healthy_treats_404_as_up() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let st = m.a.status().await.unwrap();
    assert!(st.health == Health::Up && st.system.as_str() == "relay");
    assert_eq!(st.version.as_deref(), Some("0.9.2"));
    assert_eq!(
        st.components
            .iter()
            .map(|c| c.state.as_str())
            .collect::<Vec<_>>(),
        ["running", "running", "running"]
    );
    let d1 = st.components[1].detail.clone().unwrap();
    assert!(d1.contains("404") && d1.contains("UP"));
    assert_eq!(
        st.components[2].name,
        format!("upstream {}", r.litellm.uri())
    );
    assert!(
        st.components[2]
            .detail
            .as_deref()
            .unwrap()
            .contains("liveliness")
    );
    assert_eq!(st.endpoints[0].url, r.gateway.uri());
    assert!(!st.problems.iter().any(|p| p.severity == Severity::Error));
    assert!(!codes(&st).contains("relay.config.newer_than_gateway"));
    let want: Vec<String> = [
        "systemctl",
        "--user",
        "--timestamp=unix",
        "show",
        "nemo-relay-gateway.service",
        "-p",
        "ActiveEnterTimestamp,ActiveState,SubState,MainPID,NRestarts,Result,LoadState,UnitFileState",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert!(m.run.calls().contains(&want));
}

#[tokio::test]
async fn test_status_flags_weak_attestation_with_fix() {
    let r = Router::new().await;
    let w = weak();
    let m = make_adapter(&r, None, Some(&w), None, |_| {});
    let st = m.a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "relay.plugin.attestation_weak")
        .unwrap();
    let fix = p.fix.clone().unwrap();
    assert!(
        p.severity == Severity::Error
            && fix.contains("signature_required")
            && fix.contains("human decision")
    );
}

#[tokio::test]
async fn test_status_flags_exporters_off_and_missing_pii() {
    let r = Router::new().await;
    let ne = no_exporters();
    let m = make_adapter(&r, None, Some(&ne), None, |_| {});
    assert!(codes(&m.a.status().await.unwrap()).contains("relay.exporters.off"));
    let no_pii = fx("plugins.toml").replace(
        "kind = \"pii_redaction\"\nenabled = true",
        "kind = \"pii_redaction\"\nenabled = false",
    );
    let m2 = make_adapter(&r, None, Some(&no_pii), None, |_| {});
    assert!(codes(&m2.a.status().await.unwrap()).contains("relay.pii_redaction.off"));
}

#[tokio::test]
async fn test_status_gateway_down_is_data_not_exception() {
    let r = Router::new().await;
    r.gateway.get("/health", Reply::Close);
    let inactive = "ActiveState=inactive\nSubState=dead\nLoadState=loaded\nMainPID=0\nNRestarts=0\nResult=success\nUnitFileState=enabled\n";
    let m = make_adapter(
        &r,
        Some(standard_runner(Some(inactive))),
        None,
        None,
        |_| {},
    );
    let st = m.a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "relay.gateway.down")
        .unwrap();
    assert_eq!(
        p.fix.as_deref(),
        Some("systemctl --user restart nemo-relay-gateway.service")
    );
    let mm = m.a.metrics().await.unwrap();
    assert!(!mm.available && mm.points.is_empty() && mm.error.is_some());
}

#[tokio::test]
async fn test_status_active_but_silent_is_degraded_and_5xx_is_degraded() {
    let r = Router::new().await;
    // Python uses a ReadTimeout; a dropped connection is the same "unreachable" branch without a 3 s wait
    r.gateway.get("/health", Reply::Close);
    let m = make_adapter(&r, None, None, None, |_| {});
    assert_eq!(m.a.status().await.unwrap().health, Health::Degraded);
    r.gateway.get("/health", Reply::status(502));
    let st = m.a.status().await.unwrap();
    assert!(st.health == Health::Degraded && codes(&st).contains("relay.gateway.error"));
}

#[tokio::test]
async fn test_status_upstream_down_is_degraded_with_its_own_problem() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    r.litellm.get("/health/liveliness", Reply::Close);
    let st = m.a.status().await.unwrap();
    assert!(st.health == Health::Degraded && codes(&st).contains("relay.gateway.upstream_error"));
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "relay.gateway.upstream_error")
        .unwrap();
    assert!(
        p.fix
            .as_deref()
            .unwrap()
            .contains(&format!("{}/health/liveliness", r.litellm.uri()))
    );
    r.litellm.get("/health/liveliness", Reply::status(503));
    assert!(codes(&m.a.status().await.unwrap()).contains("relay.gateway.upstream_error"));
    r.litellm.get("/health/liveliness", Reply::status(200));
    let st = m.a.status().await.unwrap();
    assert!(st.health == Health::Up && !codes(&st).contains("relay.gateway.upstream_error"));
}

#[tokio::test]
async fn test_polling_never_forwards_a_keyless_request_to_litellm() {
    // Regression (2026-10-04): liveness used GET /v1/models, which the gateway forwards to LiteLLM; every keyless poll
    // became a 401 that LiteLLM logged to Langfuse as an ERROR trace. Status and metrics must only touch the
    // gateway's own /health and LiteLLM's unauthenticated /health/liveliness.
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    for _ in 0..3 {
        m.a.status().await.unwrap();
        m.a.metrics().await.unwrap();
    }
    assert_eq!(r.gateway.count("GET", "/v1/models"), 0);
    assert!(r.gateway.count("GET", "/health") >= 6);
    assert!(r.litellm.count("GET", "/health/liveliness") >= 3);
}

#[tokio::test]
async fn test_status_survives_every_command_failing() {
    let r = Router::new().await;
    let m = make_adapter(&r, Some(PrefixRunner::new()), None, None, |_| {}); // every argv -> rc 127
    let st = m.a.status().await.unwrap();
    assert_eq!(st.health, Health::Up); // HTTP still answers
    assert_eq!(st.version, None);
    assert!(codes(&st).contains("relay.plugins.list_unavailable"));
}

#[tokio::test]
async fn test_status_plugin_disabled_and_config_newer_than_gateway() {
    let r = Router::new().await;
    let disabled = fx("plugins_list.json").replace("\"enabled\": true", "\"enabled\": false");
    let run = standard_runner(None);
    run.on(&["nemo-relay", "plugins", "list", "--json"], okr(&disabled));
    let m = make_adapter(&r, Some(run), None, None, |_| {});
    assert!(codes(&m.a.status().await.unwrap()).contains("relay.plugin.disabled"));
    // gateway started long before the (fresh tmp) plugins.toml was written
    let old = fx("systemctl_show_active.txt").replace("@1790223677", "@1000");
    let m2 = make_adapter(&r, Some(standard_runner(Some(&old))), None, None, |_| {});
    assert!(codes(&m2.a.status().await.unwrap()).contains("relay.config.newer_than_gateway"));
    let mm = m2.a.metrics().await.unwrap();
    assert_eq!(
        mm.points
            .iter()
            .find(|p| p.key == "relay.plugin_loaded")
            .unwrap()
            .value,
        0.0
    );
}

#[tokio::test]
async fn test_metrics_canonical_keys_and_values() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let mm = m.a.metrics().await.unwrap();
    assert!(mm.available && mm.system.as_str() == "relay");
    let got: BTreeMap<String, f64> = mm.points.iter().map(|p| (p.key.clone(), p.value)).collect();
    assert_eq!(got["relay.gateway_up"], 1.0);
    assert!(got["relay.plugin_loaded"] == 1.0 || got["relay.plugin_loaded"] == 0.0);
    assert_eq!(got["relay.atof_files"], 2.0);
    #[allow(clippy::cast_precision_loss)]
    let bytes = fx("atof_sample.jsonl").len() as f64;
    assert_eq!(got["relay.atof_bytes"], bytes);
    assert_eq!(got["relay.atif_trajectories"], 2.0); // notes.txt is not a trajectory
    assert_eq!(got["relay.exporters_enabled"], 3.0);
    let allowed = [
        "relay.gateway_up",
        "relay.plugin_loaded",
        "relay.atof_files",
        "relay.atof_bytes",
        "relay.atif_trajectories",
        "relay.exporters_enabled",
    ];
    assert!(got.keys().all(|k| allowed.contains(&k.as_str())));
}

#[tokio::test]
async fn test_metrics_omit_what_cannot_be_read() {
    let r = Router::new().await;
    let m = make_adapter(&r, Some(PrefixRunner::new()), None, None, |_| {});
    let a = with_path(&m, "relay_atof_dir", m.path("relay_atof_dir").join("gone"));
    let keys: BTreeSet<String> = a
        .metrics()
        .await
        .unwrap()
        .points
        .iter()
        .map(|p| p.key.clone())
        .collect();
    assert!(!keys.contains("relay.atof_files") && !keys.contains("relay.plugin_loaded"));
    assert!(keys.contains("relay.gateway_up") && keys.contains("relay.exporters_enabled"));
}
