//! Port of backend/tests/dcgm/test_status_metrics.py: status / metrics / events against the captured real /metrics,
//! Prometheus JSON and docker inspect (fake exporter + fake Prometheus on ephemeral ports).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/dcgm/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use env::{CONTAINER, Env, is_image_inspect, is_inspect, xid_metrics};
use futures::StreamExt;
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::dcgm::{
    exporter_version, parse_exposition, short_model, version_tuple,
};
use nemo_console::domain::models::{Health, Severity, SystemStatus};
use serde_json::Value;
use support::{CLOSED, FakeRunner, fixed, fx, ok, rc};

fn codes(st: &SystemStatus) -> HashSet<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

#[test]
fn test_parse_exposition_real_scrape() {
    let s = parse_exposition(&fx("dcgm", "metrics.txt"));
    assert_eq!(s["DCGM_FI_DEV_GPU_TEMP"].len(), 2);
    let (labels, value) = &s["DCGM_FI_DEV_FB_USED"][0];
    assert!(["0", "1"].contains(&labels["gpu"].as_str()) && *value > 1000.0);
    assert!(labels["modelName"].starts_with("NVIDIA RTX PRO 6000"));
    assert!(!s.contains_key("DCGM_FI_DEV_XID_ERRORS")); // absent while no XID happened (live observation)
    assert!(s.contains_key("DCGM_FI_PROF_GR_ENGINE_ACTIVE"));
}

#[test]
fn test_parse_skips_garbage_and_nan() {
    let s =
        parse_exposition("# HELP x y\nfoo{a=\"b} 1\nbar NaN\nbaz{gpu=\"0\"} 2.5e+3\nqux +Inf\n");
    assert!(!s.contains_key("bar") && !s.contains_key("qux"));
    assert_eq!(s["baz"][0].1, 2500.0);
}

#[test]
fn test_exporter_version() {
    for (image, ver) in [
        (
            Some("nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4@sha256:148b"),
            Some("4.8.4"),
        ),
        (
            Some("nvcr.io/nvidia/k8s/dcgm-exporter:4.5.2-4.8.1"),
            Some("4.8.1"),
        ),
        (Some("nvcr.io/nvidia/k8s/dcgm-exporter:latest"), None),
        (None, None),
    ] {
        assert_eq!(exporter_version(image).as_deref(), ver, "{image:?}");
    }
}

#[test]
fn test_versions_and_names() {
    assert!(
        version_tuple("4.8.10") > version_tuple("4.8.3")
            && version_tuple("4.8.3") > version_tuple("4.8.1")
    );
    assert_eq!(
        short_model("NVIDIA RTX PRO 6000 Blackwell Max-Q Workstation Edition"),
        "RTX PRO 6000 Blackwell Max-Q Workstation"
    );
}

#[tokio::test]
async fn test_status_healthy_fixed_exporter() {
    let env = Env::new().await;
    let st = env.adapter().status().await.unwrap();
    assert!(st.health == Health::Up, "{:?}", st.problems);
    assert_eq!(st.version.as_deref(), Some("4.8.4"));
    assert!(st.problems.is_empty());
    let names: HashSet<&str> = st.components.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(CONTAINER)
            && names.contains("node1 (dcgm-sparks)")
            && names.contains("local (dcgm)")
    );
    let cont = st
        .components
        .iter()
        .find(|c| c.kind == "container")
        .unwrap();
    assert_eq!(cont.state, "running");
    assert_eq!(
        cont.image.as_deref(),
        Some("nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4")
    );
    assert!(
        st.summary.contains("2 GPU(s) local") && st.summary.contains("6/6"),
        "{}",
        st.summary
    );
    // nothing identifying leaks into the status
    let blob = serde_json::to_string(&st).unwrap();
    assert!(!blob.contains("GPU-") && !blob.contains("10.100"));
}

#[tokio::test]
async fn test_status_old_exporter_is_vulnerable_with_exact_fix() {
    let env = Env::new().await;
    env.runner.insert_first(
        is_inspect,
        fixed(ok("running|true|nvcr.io/nvidia/k8s/dcgm-exporter:4.5.2-4.8.1@sha256:aaaa|2026-10-01T00:00:00Z|0\n")),
    );
    env.pprof.set(200, "Types of profiles available"); // what 4.8.1 did
    let st = env.adapter().status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    assert_eq!(st.version.as_deref(), Some("4.8.1"));
    let vuln = st
        .problems
        .iter()
        .find(|p| p.code == "dcgm.exporter.vulnerable")
        .unwrap();
    let fix = vuln.fix.clone().unwrap_or_default();
    assert!(vuln.severity == Severity::Error && fix.contains("DCGM_VERSION=4.6.1-4.8.4"));
    assert!(
        fix.contains("observability/.env") && fix.contains("--profile gpu up -d dcgm-exporter")
    );
    let exposed = st
        .problems
        .iter()
        .find(|p| p.code == "dcgm.exporter.pprof_exposed")
        .unwrap();
    assert_eq!(exposed.severity, Severity::Error);
}

#[tokio::test]
async fn test_pprof_exposed_even_when_version_unreadable() {
    let env = Env::new().await;
    env.pprof.set(200, "x");
    let st = env.adapter().status().await.unwrap();
    assert!(
        codes(&st).contains("dcgm.exporter.pprof_exposed")
            && !codes(&st).contains("dcgm.exporter.vulnerable")
    );
}

#[tokio::test]
async fn test_exporter_down_is_data_not_exception() {
    let env = Env::new().await;
    env.runner
        .insert_first(is_inspect, fixed(rc(1, "", "Error: No such object: x")));
    let a = env.adapter_with(Arc::clone(&env.runner), CLOSED, &env.url()); // the exporter refuses connections
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Down && codes(&st).contains("dcgm.exporter.down"));
    assert_eq!(
        st.components
            .iter()
            .find(|c| c.kind == "container")
            .unwrap()
            .state,
        "absent"
    );
    let snap = a.metrics().await.unwrap();
    assert!(
        snap.available
            && snap
                .points
                .iter()
                .all(|p| p.key.starts_with("dcgm.fleet") || p.key.starts_with("dcgm.host"))
    );
    assert!(snap.error.unwrap_or_default().contains("not answering"));
}

#[tokio::test]
async fn test_docker_missing_is_unknown_not_crash() {
    let env = Env::new().await;
    let a = env.adapter_with(FakeRunner::new(), &env.url(), &env.url()); // every command: rc 127
    let st = a.status().await.unwrap();
    assert_eq!(
        st.components
            .iter()
            .find(|c| c.kind == "container")
            .unwrap()
            .state,
        "unknown"
    );
    assert!(matches!(st.health, Health::Up | Health::Degraded));
}

#[tokio::test]
async fn test_a_down_spark_degrades_with_warn() {
    let env = Env::new().await;
    let mut d: Value = serde_json::from_str(&fx("dcgm", "prom_up.json")).unwrap();
    for r in d["data"]["result"].as_array_mut().unwrap() {
        if r["metric"].get("host").and_then(Value::as_str) == Some("node3") {
            r["value"][1] = Value::String("0".into());
        }
    }
    env.prom.set("up{", &d.to_string());
    let st = env.adapter().status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dcgm.host.down")
        .unwrap();
    assert!(
        p.severity == Severity::Warn && p.message.contains("node3") && !p.message.contains("node1")
    );
    assert_eq!(
        st.components
            .iter()
            .find(|c| c.name.starts_with("node3"))
            .unwrap()
            .state,
        "stopped"
    );
}

#[tokio::test]
async fn test_prometheus_down_is_a_warning() {
    let env = Env::new().await;
    let a = env.adapter_with(Arc::clone(&env.runner), &env.url(), CLOSED);
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Degraded && codes(&st).contains("dcgm.prometheus.unreachable"));
    let snap = a.metrics().await.unwrap();
    assert!(
        snap.available
            && snap
                .error
                .clone()
                .unwrap_or_default()
                .contains("Prometheus")
    );
    assert!(!snap.points.iter().any(|p| p.key.starts_with("dcgm.fleet")));
}

#[tokio::test]
async fn test_missing_job_target_is_reported() {
    let env = Env::new().await;
    let mut d: Value = serde_json::from_str(&fx("dcgm", "prom_up.json")).unwrap();
    let kept: Vec<Value> = d["data"]["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["metric"]["job"] != "dcgm-aitop100")
        .cloned()
        .collect();
    d["data"]["result"] = Value::Array(kept);
    env.prom.set("up{", &d.to_string());
    let st = env.adapter().status().await.unwrap();
    assert!(
        st.problems
            .iter()
            .any(|p| p.message.contains("dcgm-aitop100 (no target)"))
    );
}

#[tokio::test]
async fn test_diag_image_missing_is_info() {
    let env = Env::new().await;
    env.runner
        .insert_first(is_image_inspect, fixed(rc(1, "", "Error: No such image")));
    let st = env.adapter().status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dcgm.diag.image_missing")
        .unwrap();
    assert!(
        p.severity == Severity::Info
            && p.fix
                .clone()
                .unwrap()
                .starts_with("docker pull nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1")
    );
    assert_eq!(st.health, Health::Up);
}

#[tokio::test]
async fn test_metrics_real_scrape() {
    let env = Env::new().await;
    let snap = env.adapter().metrics().await.unwrap();
    assert!(snap.available && snap.error.is_none());
    let by: HashMap<(String, String), _> = snap
        .points
        .iter()
        .filter(|p| p.key.starts_with("dcgm.") && p.labels.contains_key("gpu"))
        .map(|p| ((p.key.clone(), p.labels["gpu"].clone()), p.clone()))
        .collect();
    let k = |key: &str, g: &str| (key.to_owned(), g.to_owned());
    let t = &by[&k("dcgm.gpu_temp_c", "0")];
    assert!(
        t.unit == "C"
            && t.value > 20.0
            && t.value < 100.0
            && t.labels["model"].starts_with("RTX-PRO-6000-Blackwell-Max-Q")
    );
    assert_eq!(
        by[&k("dcgm.gpu_temp_c", "1")].labels["model"],
        "RTX-PRO-6000-Blackwell-Workstation"
    );
    let raw: HashMap<String, f64> =
        parse_exposition(&fx("dcgm", "metrics.txt"))["DCGM_FI_DEV_FB_USED"]
            .iter()
            .map(|(l, v)| (l["gpu"].clone(), *v))
            .collect();
    assert_eq!(by[&k("dcgm.fb_used_b", "0")].value, raw["0"] * 1_048_576.0);
    assert_eq!(by[&k("dcgm.fb_used_b", "0")].unit, "B");
    let u = &by[&k("dcgm.gpu_util", "0")];
    assert!(u.unit == "ratio" && (0.0..=1.0).contains(&u.value));
    assert!(
        by.contains_key(&k("dcgm.pcie_replay_total", "0"))
            && by.contains_key(&k("dcgm.sm_clock_mhz", "1"))
    );
    assert!(by.contains_key(&k("dcgm.tensor_active", "0"))); // DCP family present in the scrape
    assert!(!by.keys().any(|(key, _)| key == "dcgm.xid_last")); // never fabricated
    assert!(!by.keys().any(|(key, _)| key == "dcgm.mem_temp_c"));
    for p in &snap.points {
        assert!(
            p.labels
                .keys()
                .all(|l| ["gpu", "model", "host", "job"].contains(&l.as_str()))
        );
        assert!(p.labels.values().all(|v| v.chars().count() <= 40));
    }
    let blob = serde_json::to_string(&snap).unwrap();
    assert!(!blob.contains("GPU-") && !blob.contains("UUID") && !blob.contains("10.100"));
}

#[tokio::test]
async fn test_metrics_fleet_rollups() {
    let env = Env::new().await;
    let snap = env.adapter().metrics().await.unwrap();
    let fleet: HashMap<(String, String), f64> = snap
        .points
        .iter()
        .filter(|p| p.key.starts_with("dcgm.fleet"))
        .map(|p| ((p.key.clone(), p.labels["host"].clone()), p.value))
        .collect();
    let k = |key: &str, h: &str| (key.to_owned(), h.to_owned());
    assert!(
        fleet[&k("dcgm.fleet_gpus", "local")] == 2.0
            && fleet[&k("dcgm.fleet_gpus", "node1")] == 1.0
    );
    assert_eq!(fleet[&k("dcgm.fleet_temp_max_c", "node5")], 28.0);
    let power: Value = serde_json::from_str(&fx("dcgm", "prom_power.json")).unwrap();
    let local = power["data"]["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["metric"].get("host").is_none())
        .unwrap();
    let want: f64 = local["value"][1].as_str().unwrap().parse().unwrap();
    assert!((fleet[&k("dcgm.fleet_power_w", "local")] - want).abs() < 1e-9);
    let ups: Vec<_> = snap
        .points
        .iter()
        .filter(|p| p.key == "dcgm.host_up")
        .collect();
    assert!(ups.len() == 6 && ups.iter().all(|p| p.value == 1.0));
}

#[tokio::test]
async fn test_metrics_unavailable_when_nothing_answers() {
    let env = Env::new().await;
    let snap = env
        .adapter_with(Arc::clone(&env.runner), CLOSED, CLOSED)
        .metrics()
        .await
        .unwrap();
    assert!(!snap.available && snap.points.is_empty() && snap.error.is_some());
}

#[tokio::test]
async fn test_metrics_garbage_prometheus_json_tolerated() {
    let env = Env::new().await;
    let garbage = r#"{"status":"success","data":{"result":[{"metric":{},"value":[1]}]}}"#;
    for needle in ["up{", "count by", "max by", "sum by"] {
        env.prom.set(needle, garbage);
    }
    let snap = env.adapter().metrics().await.unwrap();
    assert!(snap.available && snap.points.iter().any(|p| p.key == "dcgm.gpu_temp_c"));
}

#[tokio::test]
async fn test_xid_event_problem_and_dedupe() {
    let env = Env::new().await;
    let a = env.adapter();
    assert!(a.recent_events(100).await.unwrap().is_empty());
    env.metrics.set(200, &xid_metrics(79, ""));
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dcgm.xid.nonzero")
        .unwrap();
    assert!(
        p.severity == Severity::Warn
            && p.message.contains("GPU 1: 79")
            && st.health == Health::Degraded
    );
    let snap = a.metrics().await.unwrap(); // same value again: no second event
    assert!(
        snap.points
            .iter()
            .any(|x| x.key == "dcgm.xid_last" && x.value == 79.0)
    );
    let evs = a.recent_events(100).await.unwrap();
    assert_eq!(evs.len(), 1);
    assert!(evs[0].kind == "dcgm.xid" && evs[0].data["xid"] == 79 && evs[0].data["gpu"] == "1");
    env.metrics.set(200, &xid_metrics(48, ""));
    a.metrics().await.unwrap();
    let evs = a.recent_events(100).await.unwrap();
    let xids: Vec<&Value> = evs.iter().map(|e| &e.data["xid"]).collect();
    assert!(xids == [48, 79] || evs.len() == 2);
}

#[tokio::test]
async fn test_xid_total_counter_increase() {
    let env = Env::new().await;
    let line =
        |n: i64| format!("DCGM_EXP_XID_ERRORS_TOTAL{{gpu=\"0\",modelName=\"NVIDIA X\"}} {n}\n");
    let a = env.adapter();
    env.metrics
        .set(200, &(fx("dcgm", "metrics.txt") + &line(0)));
    a.metrics().await.unwrap();
    env.metrics
        .set(200, &(fx("dcgm", "metrics.txt") + &line(2)));
    a.metrics().await.unwrap();
    let evs = a.recent_events(100).await.unwrap();
    assert!(evs.len() == 1 && evs[0].data["new"] == 2 && evs[0].summary.contains("2 new XID"));
}

#[tokio::test]
async fn test_follow_events_yields_new_xid_and_cancels() {
    let env = Env::new().await;
    let a = env.adapter();
    let mut stream = Arc::clone(&a).follow_events(None);
    // first pass: baseline, nothing to yield
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err()
    );
    env.metrics.set(200, &xid_metrics(61, ""));
    let ev = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev.kind, "dcgm.xid");
    // a consumer that is dropped ends the tail promptly (Python: the task is cancelled)
    let forever = tokio::spawn(async move {
        let mut s = Arc::clone(&a).follow_events(None);
        while s.next().await.is_some() {}
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    forever.abort();
    assert!(forever.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn test_every_metric_point_forms_a_history_key_the_api_accepts() {
    // Found against the real console 2026-10-24: a model label with spaces made every per-GPU series un-chartable.
    let env = Env::new().await;
    let snap = env.adapter().metrics().await.unwrap();
    assert!(!snap.points.is_empty());
    let bad: Vec<String> = snap
        .points
        .iter()
        .map(nemo_console::services::metrics::series_name)
        .filter(|n| !nemo_console::api::observe::KEY_RE.is_match(n))
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
    let models: HashSet<&String> = snap
        .points
        .iter()
        .filter_map(|p| p.labels.get("model"))
        .collect();
    assert!(!models.is_empty() && models.iter().all(|m| !m.contains(' ')));
}

#[tokio::test]
async fn test_diag_image_is_checked_right_after_boot() {
    // CI 2026-10-24: a cache stamped 0.0 looked fresh shortly after boot. Rust: a never-filled cache is None (stale
    // at any clock value), so a fresh adapter always asks docker.
    let env = Env::new().await;
    env.runner
        .insert_first(is_image_inspect, fixed(rc(1, "", "Error: No such image")));
    let st = env.adapter().status().await.unwrap();
    assert!(
        st.problems
            .iter()
            .any(|p| p.code == "dcgm.diag.image_missing")
    );
}
