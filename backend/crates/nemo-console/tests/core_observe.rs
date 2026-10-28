//! Port of backend/tests/core/test_observe.py (health, overview, topology, metrics history, events, routing
//! summary, audit, settings, /metrics). `test_real_adapters_with_no_paths_give_problems_not_500` is an
//! integration test (tests/integration_core_observe.rs, written after the adapter workstreams merged).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use common::{Env, TestReq, env_with_router};
use nemo_console::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandRunner, SystemAdapter,
};
use nemo_console::config::Settings;
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::*;
use nemo_console::services::metrics::MetricsPoller;
use nemo_console::services::missing::MissingAdapter;
use nemo_console::services::routing::summarize;
use serde_json::{Value, json};

const ALL_IDS: [&str; 10] = [
    "switchyard",
    "relay",
    "dynamo",
    "skillevaluator",
    "datadesigner",
    "openshell",
    "dcgm",
    "aiperf",
    "guardrails",
    "evaluator",
];

#[tokio::test]
async fn test_health_overview_systems_topology() {
    let (env, _router) = env_with_router().await;
    let h = env.get("/api/v1/health").send().await.json();
    assert_eq!(h["status"], "ok");
    assert_eq!(h["trust"], "loopback-admin");
    let systems: BTreeSet<&str> = h["systems"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(systems, ALL_IDS.into_iter().collect());
    let o = env.get("/api/v1/overview").send().await.json();
    assert_eq!(o["systems"].as_array().unwrap().len(), 10);
    let nodes: BTreeSet<&str> = o["topology"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(
        ["litellm", "switchyard", "relay"]
            .iter()
            .all(|n| nodes.contains(n))
    );
    assert_eq!(
        env.get("/api/v1/systems")
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        env.get("/api/v1/systems/dynamo/status").send().await.json()["system"],
        "dynamo"
    );
    let t = env.get("/api/v1/topology").send().await.json();
    let litellm = t["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "litellm")
        .unwrap();
    assert_eq!(litellm["health"], "up");
    // a docker inspect (engine probes) must be among the runner calls
    let calls = env.runner.calls();
    assert!(
        !calls.is_empty()
            && calls
                .iter()
                .any(|c| c.len() >= 2 && c[0] == "docker" && c[1] == "inspect")
    );
}

#[tokio::test]
async fn test_down_system_is_200_not_5xx() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Dynamo).knobs().health = Some(Health::Down);
    env.state.status.invalidate(SystemId::Dynamo);
    let r = env.get("/api/v1/systems/dynamo/status").send().await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["health"], "down");
    assert_eq!(
        env.get("/api/v1/health").send().await.json()["status"],
        "degraded"
    );
}

#[tokio::test]
async fn test_overall_health_ignores_systems_that_are_off() {
    // Addendum 9.4: an opt-in system that is off on purpose (Dynamo) must not keep /health `degraded` for good.
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Dynamo).knobs().health = Some(Health::Off);
    env.state.status.invalidate(SystemId::Dynamo);
    let h = env.get("/api/v1/health").send().await.json();
    assert_eq!(h["systems"]["dynamo"], "off");
    assert_eq!(h["status"], "ok");
    env.a(SystemId::Relay).knobs().health = Some(Health::Degraded);
    env.state.status.invalidate(SystemId::Relay);
    assert_eq!(
        env.get("/api/v1/health").send().await.json()["status"],
        "degraded"
    ); // off is ignored, nothing else
}

#[tokio::test]
async fn test_topology_probe_failures_are_data() {
    // Python: the router mock raises ConnectError; here the router URL is a closed port
    let env = Env::new(Some("http://127.0.0.1:9"));
    let t = env.get("/api/v1/topology").send().await.json();
    let litellm = t["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "litellm")
        .unwrap();
    assert_eq!(litellm["health"], "down");
}

/// Python monkeypatches `_REGISTRY["relay"]` to a module that does not exist; the other systems are built by the
/// registry. Here every other system is a MissingAdapter too, so nothing can reach a real service.
fn registry_without_relay(
    sid: SystemId,
    _ctx: &AdapterContext,
    _cfg: &Settings,
) -> Result<AdapterRef, AdapterError> {
    if sid == SystemId::Relay {
        return Err(AdapterError::failed(
            "ModuleNotFoundError",
            "No module named 'nemo_console.adapters.does_not_exist'",
        ));
    }
    Ok(Arc::new(MissingAdapter {
        system: sid,
        reason: "not built in this test".to_owned(),
    }))
}

#[tokio::test]
async fn test_missing_adapter_module_is_unknown_with_problem() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let mut settings = env.settings.clone();
    settings.state_dir = env.tmp().join("state2");
    let mut adapters: HashMap<SystemId, AdapterRef> = HashMap::new();
    for sid in [SystemId::Switchyard, SystemId::Dynamo] {
        adapters.insert(sid, Arc::clone(env.a(sid)) as Arc<dyn SystemAdapter>);
    }
    let st = build_container(
        settings,
        Overrides {
            adapters,
            runner: Some(Arc::clone(&env.runner) as Arc<dyn CommandRunner>),
            http: Some(env.http.clone()),
            registry: Some(registry_without_relay),
            environ: Some(env.environ.clone()),
        },
    )
    .unwrap();
    let get = |p: &str| TestReq::new(Arc::clone(&st), "GET", p).send();
    let s = get("/api/v1/systems/relay/status").await.json();
    assert_eq!(s["health"], "unknown");
    assert!(
        s["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "console.adapter.missing")
    );
    assert_eq!(
        get("/api/v1/systems/relay/metrics").await.json()["available"],
        false
    );
    assert_eq!(get("/api/v1/systems/relay/events").await.json(), json!([]));
    assert_eq!(get("/api/v1/systems/relay/config").await.json(), json!([]));
    assert_eq!(get("/api/v1/systems/relay/actions").await.json(), json!([]));
    assert_eq!(
        get("/api/v1/systems/relay/params").await.json()["catalog"],
        json!([])
    );
    assert_eq!(
        get("/api/v1/health").await.json()["systems"]["relay"],
        "unknown"
    );
    assert_eq!(get("/api/v1/overview").await.status, 200);
}

#[tokio::test]
async fn test_adapter_exception_becomes_unknown_health() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Relay).knobs().status_error = Some(AdapterError::Invalid("bad".to_owned()));
    env.state.status.invalidate(SystemId::Relay);
    assert_eq!(
        env.get("/api/v1/systems/relay/status").send().await.json()["health"],
        "unknown"
    );
}

fn point(key: &str, value: f64, labels: &[(&str, &str)]) -> MetricPoint {
    let mut p = MetricPoint::new(key, value);
    for (k, v) in labels {
        p.labels.insert((*k).to_owned(), (*v).to_owned());
    }
    p
}

fn values(h: &Value) -> Vec<f64> {
    h["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[1].as_f64().unwrap())
        .collect()
}

#[tokio::test]
async fn test_metrics_live_and_history() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let p = &env.state.metrics;
    // inside ONE past minute bucket, so the rollup average is deterministic
    let base = (now_ts() / 60.0).floor() * 60.0 - 120.0;
    for i in 0..5u32 {
        p.record(
            SystemId::Switchyard,
            base + f64::from(i) * 2.0,
            &[
                point("sy.tokens", 1.0, &[("kind", "prompt")]),
                point("sy.tokens", 2.0, &[("kind", "completion")]),
                point("sy.requests_total", f64::from(i), &[]),
            ],
        );
    }
    let live = env
        .get("/api/v1/systems/switchyard/metrics")
        .send()
        .await
        .json();
    assert!(live["available"].as_bool().unwrap() && !live["points"].as_array().unwrap().is_empty());
    let h = env
        .get("/api/v1/systems/switchyard/metrics/history?window=15m&key=sy.requests_total")
        .send()
        .await
        .json();
    assert_eq!(values(&h), [0.0, 1.0, 2.0, 3.0, 4.0]);
    let summed = env
        .get("/api/v1/systems/switchyard/metrics/history?key=sy.tokens")
        .send()
        .await
        .json();
    assert_eq!(values(&summed), [3.0; 5]); // label variants summed
    let one = env
        .get("/api/v1/systems/switchyard/metrics/history?key=sy.tokens{kind=completion}")
        .send()
        .await
        .json();
    assert_eq!(values(&one), [2.0; 5]);
    let hr = env
        .get("/api/v1/systems/switchyard/metrics/history?window=1h&key=sy.requests_total")
        .send()
        .await
        .json();
    let pts = hr["points"].as_array().unwrap();
    assert!(!pts.is_empty() && pts[pts.len() - 1][1] == json!(2.0)); // 1-min rollup average of 0..4
    let long = format!("window=15m&key={}", "a".repeat(300));
    for bad in ["window=2h&key=sy.x", "key=Bad Key", "key=", long.as_str()] {
        let r = env
            .get(&format!("/api/v1/systems/switchyard/metrics/history?{bad}"))
            .send()
            .await;
        assert_eq!(r.status, 422, "{bad}");
    }
}

#[tokio::test]
async fn test_ring_buffer_and_series_are_bounded() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let p = MetricsPoller::new(
        Arc::clone(&env.state.status),
        Arc::clone(&env.state.store),
        2.0,
        10,
    );
    for i in 0..50u32 {
        p.record(
            SystemId::Dynamo,
            1000.0 + f64::from(i),
            &[point("dyn.up", 1.0, &[])],
        );
    }
    assert_eq!(p.ring_len(SystemId::Dynamo, "dyn.up"), Some(p.maxlen()));
    assert_eq!(p.maxlen(), 6);
    for i in 0..400 {
        let id = i.to_string();
        p.record(
            SystemId::Dynamo,
            2000.0,
            &[point("dyn.m", 1.0, &[("id", id.as_str())])],
        );
    }
    assert!(p.series_for(SystemId::Dynamo) <= 256);
}

#[tokio::test]
async fn test_poller_tick_uses_adapters() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.state.metrics.tick().await;
    assert!(
        env.state
            .metrics
            .ring_len(SystemId::Switchyard, "fake.requests_total")
            .is_some()
    );
}

#[tokio::test]
async fn test_events_endpoint_limit_and_validation() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().events = (0..10)
        .map(|i| SystemEvent {
            ts: f64::from(i),
            system: SystemId::Switchyard,
            kind: "k".to_owned(),
            summary: "s".to_owned(),
            data: serde_json::Map::new(),
        })
        .collect();
    let r = env
        .get("/api/v1/systems/switchyard/events?limit=3")
        .send()
        .await;
    assert_eq!(r.json().as_array().unwrap().len(), 3);
    assert_eq!(
        env.get("/api/v1/systems/switchyard/events?limit=0")
            .send()
            .await
            .status,
        422
    );
    assert_eq!(
        env.get("/api/v1/systems/switchyard/events?limit=99999")
            .send()
            .await
            .status,
        422
    );
}

fn dec(route: &str, model: &str, tier: &str, p: i64, c: i64, ts: f64, extra: Value) -> SystemEvent {
    let mut data = json!({"route": route, "model": model, "tier": tier, "prompt_tokens": p, "completion_tokens": c});
    if let (Some(d), Value::Object(e)) = (data.as_object_mut(), extra) {
        d.extend(e);
    }
    SystemEvent {
        ts,
        system: SystemId::Switchyard,
        kind: "routing.decision".to_owned(),
        summary: "x".to_owned(),
        data: data.as_object().unwrap().clone(),
    }
}

#[tokio::test]
async fn test_routing_summary() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let now = now_ts();
    env.a(SystemId::Switchyard).knobs().events = vec![
        dec(
            "auto-local",
            "qwen",
            "capable",
            10,
            5,
            now - 10.0,
            json!({}),
        ),
        dec(
            "auto-local",
            "nemo",
            "efficient",
            3,
            1,
            now - 20.0,
            json!({}),
        ),
        dec(
            "direct-27b",
            "qwen",
            "capable",
            1,
            1,
            now - 30.0,
            json!({"status": 500}),
        ),
        dec("old", "qwen", "capable", 100, 100, now - 7200.0, json!({})),
        SystemEvent {
            ts: now,
            system: SystemId::Switchyard,
            kind: "other".to_owned(),
            summary: "ignored".to_owned(),
            data: serde_json::Map::new(),
        },
    ];
    let s = env
        .get("/api/v1/routing/summary?window=1h")
        .send()
        .await
        .json();
    assert_eq!(
        (s["requests"].as_i64(), s["errors"].as_i64()),
        (Some(3), Some(1))
    );
    assert_eq!(s["by_route"], json!({"auto-local": 2, "direct-27b": 1}));
    assert_eq!(s["by_model"], json!({"qwen": 2, "nemo": 1}));
    assert_eq!(s["by_tier"], json!({"capable": 2, "efficient": 1}));
    assert_eq!(
        s["tokens"],
        json!({"prompt": 14, "completion": 7, "total": 21})
    );
    assert_eq!(
        env.get("/api/v1/routing/summary?window=24h")
            .send()
            .await
            .json()["requests"],
        4
    );
    assert_eq!(
        env.get("/api/v1/routing/summary?window=9y")
            .send()
            .await
            .status,
        422
    );
    assert_eq!(summarize(&[], "1h", None)["requests"], 0);
}

#[tokio::test]
async fn test_audit_is_append_only_and_records_mutations() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.put("/api/v1/settings", json!({"poll_interval_s": 5}))
        .send()
        .await;
    env.post(
        "/api/v1/systems/dynamo/actions/reset",
        json!({"confirm": false}),
    )
    .send()
    .await;
    let rows = env.get("/api/v1/audit").send().await.json();
    let statuses: Vec<i64> = rows
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .map(|r| r["status"].as_i64().unwrap())
        .collect();
    assert_eq!(statuses, [400, 200]);
    assert_eq!(
        (rows[0]["system"].as_str(), rows[0]["actor"].as_str()),
        (Some("dynamo"), Some("loopback"))
    );
    assert_eq!(
        env.get("/api/v1/audit?system=dynamo")
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(env.get("/api/v1/audit?system=zzz").send().await.status, 422);
    assert_eq!(env.get("/api/v1/audit?limit=501").send().await.status, 422);
    assert!(env.state.store.execute("DELETE FROM audit", []).is_err());
    assert!(
        env.state
            .store
            .execute("UPDATE audit SET status=1", [])
            .is_err()
    );
}

#[tokio::test]
async fn test_settings_roundtrip_and_validation() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let v = env.get("/api/v1/settings").send().await.json();
    assert_eq!(v["poll_interval_s"], json!(2.0));
    assert!(
        v["urls"]["switchyard"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1")
    );
    let r = env
        .put(
            "/api/v1/settings",
            json!({"poll_interval_s": 7, "urls": {"dynamo": "http://localhost:8021"},
                   "playground_key_env": {"switchyard": "MY_API_KEY"}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["restart_required"], true);
    let v = env.get("/api/v1/settings").send().await.json();
    assert_eq!(v["poll_interval_s"], json!(7.0));
    assert_eq!(v["urls"]["dynamo"], "http://localhost:8021");
    assert_eq!(v["playground_key_env"]["switchyard"], "MY_API_KEY");
    assert_eq!(v["tokens"]["switchyard"], "[unset]");
    for bad in [
        json!({"urls": {"dynamo": "http://evil.example:80"}}),
        json!({"urls": {"dynamo": "https://127.0.0.1"}}),
        json!({"urls": {"nope": "http://127.0.0.1:1"}}),
        json!({"poll_interval_s": 0.01}),
        json!({"playground_key_env": {"a": "lower"}}),
        json!({"urls": {"dynamo": "http://user:pw@127.0.0.1:1"}}),
        json!({"state_dir": "/tmp"}),
    ] {
        assert_eq!(
            env.put("/api/v1/settings", bad.clone()).send().await.status,
            422,
            "{bad}"
        );
    }
    assert!((env.state.metrics.interval() - 7.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn test_console_metrics_endpoint() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.get("/api/v1/systems").send().await;
    let r = env.get("/metrics").send().await;
    assert_eq!(r.status, 200);
    assert!(r.text().contains("nemo_console_http_requests_total"));
    assert_eq!(
        env.get("/metrics").client("10.0.0.9").send().await.status,
        401
    );
}

#[tokio::test]
async fn test_health_reports_caller_role() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, vk) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, ak) = env.state.auth.mint(Role::Admin, "").unwrap();
    assert_eq!(
        env.get("/api/v1/health").send().await.json()["role"],
        "admin"
    ); // loopback trust
    assert_eq!(
        env.get("/api/v1/health")
            .client("10.0.0.9")
            .token(&vk)
            .send()
            .await
            .json()["role"],
        "viewer"
    );
    assert_eq!(
        env.get("/api/v1/health")
            .client("10.0.0.9")
            .token(&ak)
            .send()
            .await
            .json()["role"],
        "admin"
    );
}
