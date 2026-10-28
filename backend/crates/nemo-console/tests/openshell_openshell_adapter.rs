//! Port of backend/tests/openshell/test_openshell_adapter.py: status, metrics, events, read-only trust files,
//! actions. Offline: fakes answering with the captured fixtures; nothing starts a CLI, docker or a gateway.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/openshell/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use env::{GW, OsEnv, World, live_sandbox_list, make_runner};
use futures::StreamExt;
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::openshell::{
    MAX_SANDBOXES, OpenShellAdapter, PHASES, RING, phase_bucket, strip_ansi,
};
use nemo_console::domain::models::{Danger, Health, MetricsSnapshot, Role, Severity, SystemStatus};
use nemo_console::services::environ::Environ;
use nemo_console::services::runner::{ALLOWED_BINARIES, SubprocessRunner};
use serde_json::{Map, Value, json};
use support::{FakeRunner, fixed, fx, ok, raising, rc, with};

fn codes(st: &SystemStatus) -> HashSet<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

fn point(snap: &MetricsSnapshot, key: &str, labels: &[(&str, &str)]) -> Option<f64> {
    snap.points
        .iter()
        .find(|p| {
            p.key == key
                && p.labels.len() == labels.len()
                && labels
                    .iter()
                    .all(|(k, v)| p.labels.get(*k).map(String::as_str) == Some(*v))
        })
        .map(|p| p.value)
}

fn gw(rest: &[&str]) -> Vec<String> {
    GW.iter()
        .chain(rest.iter())
        .map(|s| (*s).to_owned())
        .collect()
}

fn params(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn world(f: impl FnOnce(&mut World)) -> World {
    let mut w = World::default();
    f(&mut w);
    w
}

// ── runner policy / pure helpers ────────────────────────────────────────────────────────────────
#[test]
fn test_binaries_are_allowlisted() {
    assert!(ALLOWED_BINARIES.contains("openshell") && ALLOWED_BINARIES.contains("docker"));
    let r = SubprocessRunner::with_environ(&[], Environ::from_pairs(&[("PATH", "/nonexistent")]));
    let err = r.resolve("openshell-gateway").unwrap_err(); // only the bare CLI is allowlisted
    assert!(err.to_string().contains("not allowed"));
    if let Err(e) = r.resolve("openshell") {
        assert!(!e.to_string().contains("not allowed")); // allowlisted but not installed here is fine
    }
}

#[test]
fn test_strip_ansi_handles_the_real_status_output() {
    let raw = fx("openshell", "status-connected.raw.txt");
    assert!(raw.contains('\x1b'));
    let clean = strip_ansi(&raw);
    assert!(
        !clean.contains('\x1b')
            && clean.contains("Status: Connected")
            && clean.contains("Version: 0.0.77")
    );
}

#[test]
fn test_phase_buckets_are_bounded() {
    assert!(phase_bucket("Ready") == "Ready" && phase_bucket("provisioning") == "Provisioning");
    assert!(phase_bucket("SomethingNew") == "Other" && phase_bucket("") == "Other");
}

// ── status ──────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_status_up_matches_the_live_shape() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    let st = a.status().await.unwrap();
    assert!(
        st.system.as_str() == "openshell"
            && st.health == Health::Up
            && st.version.as_deref() == Some("0.0.77")
    );
    assert!(
        st.summary.contains("connected")
            && st.summary.contains("0 sandbox(es)")
            && st.summary.contains("CLI 0.0.71"),
        "{}",
        st.summary
    );
    assert_eq!(
        st.components
            .iter()
            .take(3)
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["openshell CLI", "ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"]
    );
    assert!(st.components.iter().all(|c| c.state == "running"));
    assert_eq!(
        st.components[1].image.as_deref(),
        Some("ghcr.io/nvidia/openshell/gateway@sha256:ba6f6537544d")
    );
    assert_eq!(st.endpoints[0].url, "https://127.0.0.1:17670");
    assert_eq!(
        codes(&st),
        HashSet::from(["openshell.version.skew".to_owned()])
    ); // CLI 0.0.71 vs gateway 0.0.77
    assert_eq!(
        st.problems.iter().map(|p| p.severity).collect::<Vec<_>>(),
        [Severity::Info]
    );
    // golden argv: pinned to the gateway registration, no shell, docker via the allowlisted binary
    let calls = run.argvs();
    assert!(calls.contains(&vec!["openshell".to_owned(), "--version".into()]));
    assert!(calls.contains(&gw(&["status"])));
    assert!(
        calls.contains(
            &[
                "docker",
                "network",
                "inspect",
                "openshell-docker",
                "--format",
                "{{.Internal}}"
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
        )
    );
    assert!(calls.iter().any(|c| c.len() >= 5
        && c[..3] == ["docker", "inspect", "--format"]
        && c[c.len() - 2..] == ["ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"]));
    assert!(calls.contains(&gw(&["sandbox", "list", "--limit", "200", "-o", "json"])));
}

#[tokio::test]
async fn test_status_text_never_carries_ansi() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let st = a.status().await.unwrap();
    assert!(!serde_json::to_string(&st).unwrap().contains("\\u001b"));
}

#[tokio::test]
async fn test_stopped_on_purpose_is_off_not_down() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(
        Some(world(|w| {
            w.inspect_out = fx("openshell", "docker-inspect-stopped.HAND-WRITTEN.txt");
        })),
        true,
    );
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Off);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "openshell.gateway.off")
        .unwrap();
    let fix = p.fix.clone().unwrap();
    assert!(
        p.severity == Severity::Info && fix.contains("docker compose -f") && fix.contains("up -d")
    );
    assert!(fix.contains("data-designer-local") && p.message.contains("data-designer-local"));
    assert_eq!(run.count(&[GW[0], GW[1], GW[2], "status"]), 0); // no CLI connect attempt to a stopped gateway
    assert_eq!(run.count(&[GW[0], GW[1], GW[2], "sandbox"]), 0);
    assert_eq!(
        st.components
            .iter()
            .filter(|c| c.kind == "container")
            .map(|c| c.state.as_str())
            .collect::<Vec<_>>(),
        ["stopped", "stopped"]
    );
}

#[tokio::test]
async fn test_container_never_created_is_off() {
    let env = OsEnv::new();
    let w = world(|w| {
        w.inspect_out = String::new();
        w.inspect_rc = 1;
        w.inspect_err = "error: no such object: ai-lab-dd-gateway".into();
    });
    let (a, _, _) = env.make(Some(w), true);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Off);
    assert_eq!(
        st.components
            .iter()
            .filter(|c| c.kind == "container")
            .map(|c| c.state.as_str())
            .collect::<Vec<_>>(),
        ["absent", "absent"]
    );
}

#[tokio::test]
async fn test_crashed_gateway_is_down() {
    let env = OsEnv::new();
    let out = fx("openshell", "docker-inspect-stopped.HAND-WRITTEN.txt").replacen(
        "|exited|137|",
        "|exited|1|",
        1,
    );
    let (a, _, _) = env.make(Some(world(|w| w.inspect_out = out)), true);
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Down && codes(&st).contains("openshell.gateway.failed"));
    assert!(
        st.problems
            .iter()
            .find(|p| p.code == "openshell.gateway.failed")
            .unwrap()
            .fix
            .clone()
            .unwrap()
            .starts_with("docker logs")
    );
}

#[tokio::test]
async fn test_docker_down_degrades_to_the_cli_view() {
    let env = OsEnv::new();
    let w = world(|w| {
        w.inspect_out = String::new();
        w.inspect_rc = 1;
        w.inspect_err = "Cannot connect to the Docker daemon".into();
    });
    let (a, _, _) = env.make(Some(w), true);
    let st = a.status().await.unwrap();
    assert!(codes(&st).contains("openshell.docker.unavailable"));
    assert!(matches!(
        st.health,
        Health::Up | Health::Degraded | Health::Unknown
    )); // never "stopped on purpose"
    assert!(
        st.components
            .iter()
            .filter(|c| c.kind == "container")
            .all(|c| c.state == "unknown")
    );
}

#[tokio::test]
async fn test_cli_absent_is_down_with_a_fix() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.cli_rc = 127;
            w.cli_version = String::new();
        })),
        true,
    );
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "openshell.cli.missing")
        .unwrap();
    assert!(p.severity == Severity::Error && p.fix.clone().unwrap().contains("0.0.71"));
    assert_eq!(st.components[0].state, "absent");
}

#[tokio::test]
async fn test_non_internal_sandbox_network_is_an_error() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(Some(world(|w| w.internal = "false".into())), true);
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "openshell.network.not_internal")
        .unwrap();
    let fix = p.fix.clone().unwrap();
    assert!(
        p.severity == Severity::Error && fix.contains("--internal") && !fix.contains("down -v")
    );
    assert!(fix.contains("172.16.0.0/24"));
    assert_eq!(st.health, Health::Degraded);
}

#[tokio::test]
async fn test_non_internal_network_is_flagged_even_when_gateway_is_off() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.internal = "false".into();
            w.inspect_out = fx("openshell", "docker-inspect-stopped.HAND-WRITTEN.txt");
        })),
        true,
    );
    let st = a.status().await.unwrap();
    assert!(
        st.health == Health::Off
            && codes(&st).contains("openshell.network.not_internal")
            && codes(&st).contains("openshell.gateway.off")
    );
}

#[tokio::test]
async fn test_missing_network_is_a_warning() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.internal = "missing".into();
            w.inspect_out = fx("openshell", "docker-inspect-stopped.HAND-WRITTEN.txt");
        })),
        true,
    );
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "openshell.network.missing")
        .unwrap();
    assert!(
        p.severity == Severity::Warn
            && p.fix.as_deref()
                == Some("docker network create --internal --subnet 172.16.0.0/24 openshell-docker")
    );
}

#[tokio::test]
async fn test_published_port_beyond_loopback_is_an_error() {
    let env = OsEnv::new();
    let out = fx("openshell", "docker-inspect-running.txt")
        .replace(r#""HostIp":"127.0.0.1""#, r#""HostIp":"0.0.0.0""#);
    let (a, _, _) = env.make(Some(world(|w| w.inspect_out = out)), true);
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "openshell.port.exposed")
        .unwrap();
    assert!(
        p.severity == Severity::Error
            && p.message.contains("0.0.0.0")
            && p.fix.clone().unwrap().contains("127.0.0.1")
    );
    assert_eq!(st.health, Health::Degraded);
}

#[tokio::test]
async fn test_forwarder_down_degrades() {
    let env = OsEnv::new();
    let mut lines: Vec<String> = fx("openshell", "docker-inspect-running.txt")
        .lines()
        .map(str::to_owned)
        .collect();
    lines[1] = lines[1].replace("|running|0|", "|exited|0|");
    let out = lines.join("\n") + "\n";
    let (a, _, _) = env.make(Some(world(|w| w.inspect_out = out)), true);
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Degraded && codes(&st).contains("openshell.forwarder.stopped"));
}

#[tokio::test]
async fn test_unregistered_gateway() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.status_out = fx("openshell", "status-unregistered.raw.txt");
        })),
        true,
    );
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded); // `status` exits 0 here: only the line decides
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "openshell.gateway.unregistered")
        .unwrap();
    assert_eq!(
        p.fix.as_deref(),
        Some("openshell gateway add https://127.0.0.1:17670 --local --name openshell")
    );
}

#[tokio::test]
async fn test_disconnected_gateway() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.status_out = fx("openshell", "status-disconnected.HAND-WRITTEN.raw.txt");
        })),
        true,
    );
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Degraded && codes(&st).contains("openshell.gateway.disconnected"));
    assert!(!st.summary.contains("0 sandbox"));
}

#[tokio::test]
async fn test_no_version_skew_when_pair_matches() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| w.cli_version = "openshell 0.0.77\n".into())),
        true,
    );
    assert!(!codes(&a.status().await.unwrap()).contains("openshell.version.skew"));
}

#[tokio::test]
async fn test_version_probe_is_cached() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    a.status().await.unwrap();
    a.status().await.unwrap();
    assert_eq!(run.count(&["openshell", "--version"]), 1);
}

#[tokio::test]
async fn test_status_flags_unpinned_gateway_config() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    assert!(
        !a.status()
            .await
            .unwrap()
            .problems
            .iter()
            .any(|p| p.code.starts_with("openshell.config"))
    );
    let cfg = env.rd().join("config").join("gateway.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        text.replace(
            "image_pull_policy = \"Never\"",
            "image_pull_policy = \"Always\"",
        ),
    )
    .unwrap();
    assert!(codes(&a.status().await.unwrap()).contains("openshell.config.pull_policy"));
    std::fs::write(&cfg, "[openshell\n").unwrap();
    assert!(codes(&a.status().await.unwrap()).contains("openshell.config.unreadable"));
}

#[tokio::test]
async fn test_status_survives_every_probe_failing() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    run.clear(); // all rc 127
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Down && codes(&st).contains("openshell.cli.missing"));
}

// ── metrics ─────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_metrics_empty_gateway() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let m = a.metrics().await.unwrap();
    assert!(m.available);
    assert!(
        point(&m, "os.gateway_up", &[]) == Some(1.0) && point(&m, "os.sandboxes", &[]) == Some(0.0)
    );
    assert!(
        point(&m, "os.network_internal", &[]) == Some(1.0)
            && point(&m, "os.providers", &[]) == Some(1.0)
    );
    assert!(
        point(&m, "os.gateway_restarts", &[]) == Some(0.0)
            && point(&m, "os.gateway_uptime_s", &[]).unwrap_or(0.0) > 0.0
    );
    let phases: HashSet<&str> = m
        .points
        .iter()
        .filter(|p| p.key == "os.sandboxes_by_phase")
        .map(|p| p.labels["phase"].as_str())
        .collect();
    assert_eq!(phases, PHASES.iter().copied().collect());
    assert!(point(&m, "os.sandbox_oldest_age_s", &[]).is_none()); // nothing to age: omitted, not 0
}

#[tokio::test]
async fn test_metrics_counts_phases_and_buckets_unknown() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.sandboxes = fx("openshell", "sandbox-list-phases.HAND-WRITTEN.json");
        })),
        true,
    );
    let m = a.metrics().await.unwrap();
    assert_eq!(point(&m, "os.sandboxes", &[]), Some(4.0));
    for (ph, n) in [
        ("Ready", 1.0),
        ("Provisioning", 1.0),
        ("Error", 1.0),
        ("Deleting", 0.0),
        ("Other", 1.0),
    ] {
        assert_eq!(
            point(&m, "os.sandboxes_by_phase", &[("phase", ph)]),
            Some(n),
            "{ph}"
        );
    }
    assert!(point(&m, "os.sandbox_oldest_age_s", &[]).unwrap_or(0.0) > 0.0);
}

#[tokio::test]
async fn test_metrics_gateway_off_reads_zero_and_omits_the_rest() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(
        Some(world(|w| {
            w.inspect_out = fx("openshell", "docker-inspect-stopped.HAND-WRITTEN.txt");
        })),
        true,
    );
    let m = a.metrics().await.unwrap();
    assert!(m.available && point(&m, "os.gateway_up", &[]) == Some(0.0));
    assert!(
        point(&m, "os.sandboxes", &[]).is_none()
            && point(&m, "os.providers", &[]).is_none()
            && point(&m, "os.gateway_uptime_s", &[]).is_none()
    );
    assert_eq!(run.count(&[GW[0], GW[1], GW[2], "status"]), 0);
}

#[tokio::test]
async fn test_metrics_unavailable_without_the_cli() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.cli_rc = 127;
            w.cli_version = String::new();
        })),
        true,
    );
    let m = a.metrics().await.unwrap();
    assert!(!m.available && m.points.is_empty() && m.error.is_some());
}

#[tokio::test]
async fn test_metrics_tolerates_a_failed_sandbox_list() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(Some(world(|w| w.sandbox_rc = 1)), true);
    let m = a.metrics().await.unwrap();
    assert!(
        point(&m, "os.gateway_up", &[]) == Some(1.0) && point(&m, "os.sandboxes", &[]).is_none()
    );
}

// ── events ──────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_events_create_phase_delete_and_failed_poll_is_not_a_delete() {
    let env = OsEnv::new();
    let (a, _, w) = env.make(None, true);
    assert!(a.recent_events(100).await.unwrap().is_empty()); // baseline of an empty gateway
    let live: Value = serde_json::from_str(&live_sandbox_list()).unwrap();
    let mut prov = live[0].clone();
    prov["phase"] = json!("Provisioning");
    w.lock().unwrap().sandboxes = json!([prov]).to_string();
    a.invalidate_snapshot().await;
    a.recent_events(100).await.unwrap();
    w.lock().unwrap().sandboxes = live_sandbox_list(); // Provisioning -> Ready
    a.invalidate_snapshot().await;
    a.recent_events(100).await.unwrap();
    w.lock().unwrap().sandbox_rc = 1; // gateway hiccup: NOT "deleted"
    a.invalidate_snapshot().await;
    a.recent_events(100).await.unwrap();
    {
        let mut g = w.lock().unwrap();
        g.sandbox_rc = 0;
        g.sandboxes = "[]".into();
    }
    a.invalidate_snapshot().await;
    let evs = a.recent_events(100).await.unwrap();
    let actions: Vec<&str> = evs
        .iter()
        .rev()
        .map(|e| e.data["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["created", "phase", "deleted"]);
    assert!(
        evs.iter()
            .all(|e| e.kind == "openshell.sandbox" && e.system.as_str() == "openshell")
    );
    assert!(evs[0].data["name"] == "nc-adapter-probe" && evs[0].summary.contains("deleted"));
    let keys: HashSet<&str> = evs[0].data.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        HashSet::from(["name", "id", "phase", "action", "previous_phase"])
    ); // metadata only
    assert!(evs[1].data["previous_phase"] == "Provisioning" && evs[1].data["phase"] == "Ready");
    let ts: Vec<f64> = evs.iter().map(|e| e.ts).collect();
    let mut sorted = ts.clone();
    sorted.sort_by(|a, b| b.total_cmp(a));
    assert_eq!(ts, sorted); // newest first
}

#[tokio::test]
async fn test_first_poll_reports_existing_sandboxes_as_present() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(Some(world(|w| w.sandboxes = live_sandbox_list())), true);
    let evs = a.recent_events(100).await.unwrap();
    assert!(evs.len() == 1 && evs[0].data["action"] == "present");
    assert!((evs[0].ts - 1_792_841_080.0).abs() <= 2.0); // created_at is UTC
}

#[tokio::test]
async fn test_event_memory_is_bounded() {
    let env = OsEnv::new();
    let (a, _, w) = env.make(None, true);
    for i in 0..(RING + 50) {
        w.lock().unwrap().sandboxes =
            json!([{"name": format!("sb-{i}"), "id": format!("id-{i}"), "phase": "Ready", "created_at": "2026-10-24 11:00:00"}]).to_string();
        a.invalidate_snapshot().await;
        a.recent_events(100).await.unwrap();
        a.invalidate_gw_cache();
    }
    assert!(a.ring_len() == RING && a.known_len() <= MAX_SANDBOXES);
    assert_eq!(a.recent_events(7).await.unwrap().len(), 7);
}

#[tokio::test]
async fn test_follow_events_delivers_and_cancels_promptly() {
    let env = OsEnv::new();
    let (a, _, w) = env.make(None, true);
    let got = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let g = Arc::clone(&got);
    let a2 = Arc::clone(&a);
    let task = tokio::spawn(async move {
        let mut s = a2.follow_events(None);
        while let Some(Ok(ev)) = s.next().await {
            g.lock()
                .unwrap()
                .push(ev.data["action"].as_str().unwrap().to_owned());
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    w.lock().unwrap().sandboxes = live_sandbox_list();
    a.invalidate_snapshot().await;
    a.invalidate_gw_cache();
    for _ in 0..100 {
        if !got.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        a.invalidate_snapshot().await;
    }
    assert_eq!(*got.lock().unwrap(), ["created"]);
    task.abort();
    let r = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap();
    assert!(r.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn test_follow_events_replays_since_ts() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(Some(world(|w| w.sandboxes = live_sandbox_list())), true);
    let mut s = Arc::clone(&a).follow_events(Some(0.0));
    let ev = tokio::time::timeout(Duration::from_secs(1), s.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev.data["name"], "nc-adapter-probe");
}

// ── config files: read-only (ADR 0007) ──────────────────────────────────────────────────────────
#[tokio::test]
async fn test_config_files_are_all_read_only_with_a_reason() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let specs = a.config_files().unwrap();
    assert_eq!(
        specs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["gateway", "policy", "provider-profile", "compose"]
    );
    assert_eq!(
        specs.iter().map(|s| s.format.as_str()).collect::<Vec<_>>(),
        ["toml", "yaml", "yaml", "yaml"]
    );
    for s in &specs {
        assert!(
            !s.editable && s.why_readonly.clone().unwrap().contains("ADR 0007") && !s.authoritative
        );
    }
}

#[tokio::test]
async fn test_no_config_files_without_paths() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, false);
    assert!(a.config_files().unwrap().is_empty());
    assert!(matches!(
        a.config_path("gateway"),
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_read_config_returns_the_file_and_its_raw_hash() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let cf = a.read_config("policy").await.unwrap();
    let raw = std::fs::read(a.config_path("policy").unwrap()).unwrap();
    assert!(
        cf.sha256 == nemo_console::services::common::sha256_hex(&raw)
            && cf.content.contains("network_policies")
    );
    assert!(
        a.read_config("gateway")
            .await
            .unwrap()
            .content
            .contains("host_gateway_ip")
    );
}

#[tokio::test]
async fn test_read_config_scrubs_secret_looking_text() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let p = a.config_path("provider-profile").unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(
        &p,
        text + "\n# api_key: sk-abcdefghijklmnopqrstuvwxyz0123456789\n",
    )
    .unwrap();
    assert!(
        !a.read_config("provider-profile")
            .await
            .unwrap()
            .content
            .contains("sk-abcdefghijklmnopqrstuvwxyz0123456789")
    );
}

#[tokio::test]
async fn test_unknown_config_id_is_a_key_error() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    for bad in ["nope", "../policy", ""] {
        assert!(matches!(
            a.config_path(bad),
            Err(AdapterError::UnknownId(_))
        ));
        assert!(matches!(
            a.read_config(bad).await,
            Err(AdapterError::UnknownId(_))
        ));
        assert!(matches!(
            a.validate_config(bad, "x").await,
            Err(AdapterError::UnknownId(_))
        ));
        assert!(matches!(
            a.post_apply(bad, false).await,
            Err(AdapterError::UnknownId(_))
        ));
    }
}

#[tokio::test]
async fn test_symlink_out_of_the_runner_dir_is_refused() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let outside = env.tmp.path().join("outside.yaml");
    std::fs::write(&outside, "secret: 1\n").unwrap();
    let pol = env.rd().join("policy.yaml");
    std::fs::remove_file(&pol).unwrap();
    std::os::unix::fs::symlink(&outside, &pol).unwrap();
    assert!(matches!(
        a.config_path("policy"),
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_missing_file_reads_as_file_not_found() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    std::fs::remove_file(a.config_path("compose").unwrap()).unwrap();
    assert!(matches!(
        a.read_config("compose").await,
        Err(AdapterError::FileNotFound(_))
    ));
}

#[tokio::test]
async fn test_validate_is_a_parse_only_proxy_and_apply_refuses() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let good = a
        .validate_config("gateway", "[openshell]\nversion = 1\n")
        .await
        .unwrap();
    assert!(good.ok && !good.authoritative && good.note.clone().unwrap().contains("ADR 0007"));
    let bad = a.validate_config("gateway", "[openshell\n").await.unwrap();
    assert!(!bad.ok && bad.diagnostics[0].severity == Severity::Error);
    let badyaml = a.validate_config("policy", "a: [1, 2\n").await.unwrap();
    assert!(!badyaml.ok);
    let res = a.post_apply("policy", true).await.unwrap();
    assert!(!res.ok && !res.restarted);
}

// ── actions ─────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_action_catalog() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    let acts = a.actions().unwrap();
    assert_eq!(
        acts.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        [
            "doctor",
            "list_sandboxes",
            "sandbox_get",
            "sandbox_logs",
            "gateway_logs",
            "prove_policy",
            "gateway_up",
            "gateway_stop",
            "delete_sandbox"
        ]
    );
    let safe: HashSet<&str> = acts
        .iter()
        .filter(|s| s.danger == Danger::Safe)
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(
        safe,
        HashSet::from([
            "doctor",
            "list_sandboxes",
            "sandbox_get",
            "sandbox_logs",
            "gateway_logs",
            "prove_policy"
        ])
    );
    let by: HashMap<&str, Danger> = acts.iter().map(|s| (s.id.as_str(), s.danger)).collect();
    assert!(by["gateway_up"] == Danger::Disruptive && by["gateway_stop"] == Danger::Disruptive);
    assert_eq!(by["delete_sandbox"], Danger::Destructive);
    assert!(acts.iter().all(|s| s.role == Role::Admin));
    assert!(a.playground().unwrap().is_none());
}

#[tokio::test]
async fn test_unknown_action_is_a_key_error() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    assert!(matches!(
        a.run_action("sandbox_create", &Map::new()).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_doctor_and_list() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(Some(world(|w| w.sandboxes = live_sandbox_list())), true);
    let r = a.run_action("doctor", &Map::new()).await.unwrap();
    assert!(
        r.ok && r.output.contains("All checks passed.")
            && run.argvs().contains(&vec![
                "openshell".to_owned(),
                "doctor".into(),
                "check".into()
            ])
    );
    let r = a.run_action("list_sandboxes", &Map::new()).await.unwrap();
    assert!(
        r.ok && r.output.contains("nc-adapter-probe")
            && r.output.contains("Ready")
            && !r.output.contains('\x1b')
    );
    w.lock().unwrap().sandboxes = "[]".into();
    assert_eq!(
        a.run_action("list_sandboxes", &Map::new())
            .await
            .unwrap()
            .output,
        "No sandboxes found."
    );
    w.lock().unwrap().sandbox_rc = 1;
    let r = a.run_action("list_sandboxes", &Map::new()).await.unwrap();
    assert!(
        !r.ok
            && r.problems[0].code == "openshell.gateway.unreachable"
            && r.problems[0].fix.clone().unwrap().contains("up -d")
    );
}

#[tokio::test]
async fn test_actions_report_a_missing_cli() {
    let env = OsEnv::new();
    let (a, _, _) = env.make(
        Some(world(|w| {
            w.cli_rc = 127;
            w.cli_version = String::new();
        })),
        true,
    );
    let r = a.run_action("doctor", &Map::new()).await.unwrap();
    assert!(!r.ok && r.problems[0].code == "openshell.cli.missing");
}

#[tokio::test]
async fn test_delete_validates_the_name_against_the_live_list() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(Some(world(|w| w.sandboxes = live_sandbox_list())), true);
    run.on_prefix(
        &[GW[0], GW[1], GW[2], "sandbox", "delete"],
        fixed(ok("Deleted sandbox nc-adapter-probe\n")),
    );
    for bad in [
        json!("--all"),
        json!("-x"),
        json!("no-such"),
        json!(""),
        json!("a b"),
        json!("../x"),
        json!("x;rm"),
        Value::Null,
        json!(7),
        json!("nc-adapter-probe\n--all"),
    ] {
        let r = a
            .run_action("delete_sandbox", &params(json!({"name": bad})))
            .await
            .unwrap();
        assert!(!r.ok, "{bad}");
    }
    assert_eq!(run.count(&[GW[0], GW[1], GW[2], "sandbox", "delete"]), 0); // nothing reached the CLI
    let r = a
        .run_action(
            "delete_sandbox",
            &params(json!({"name": "nc-adapter-probe"})),
        )
        .await
        .unwrap();
    assert!(
        r.ok && run
            .argvs()
            .contains(&gw(&["sandbox", "delete", "nc-adapter-probe"]))
    );
    assert_eq!(run.count(&[GW[0], GW[1], GW[2], "sandbox", "delete"]), 1);
    w.lock().unwrap().sandbox_rc = 1; // cannot list -> cannot validate -> no delete
    let r = a
        .run_action(
            "delete_sandbox",
            &params(json!({"name": "nc-adapter-probe"})),
        )
        .await
        .unwrap();
    assert!(!r.ok && run.count(&[GW[0], GW[1], GW[2], "sandbox", "delete"]) == 1);
}

#[tokio::test]
async fn test_sandbox_get_and_logs() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(Some(world(|w| w.sandboxes = live_sandbox_list())), true);
    run.on_prefix(
        &[GW[0], GW[1], GW[2], "sandbox", "get"],
        fixed(ok(&fx("openshell", "sandbox-get.raw.txt"))),
    );
    run.on_prefix(
        &[GW[0], GW[1], GW[2], "logs"],
        fixed(ok(&fx("openshell", "logs.txt"))),
    );
    let r = a
        .run_action("sandbox_get", &params(json!({"name": "nc-adapter-probe"})))
        .await
        .unwrap();
    assert!(r.ok && r.output.contains("Phase: Ready") && !r.output.contains('\x1b'));
    let r = a
        .run_action(
            "sandbox_logs",
            &params(json!({"name": "nc-adapter-probe", "lines": 5})),
        )
        .await
        .unwrap();
    assert!(
        r.ok && run
            .argvs()
            .contains(&gw(&["logs", "nc-adapter-probe", "-n", "5"]))
            && r.output.contains("Landlock")
    );
    for bad in [json!(0), json!(501), json!(true), json!("5"), Value::Null] {
        assert!(
            !a.run_action(
                "sandbox_logs",
                &params(json!({"name": "nc-adapter-probe", "lines": bad}))
            )
            .await
            .unwrap()
            .ok
        );
    }
    assert!(
        !a.run_action(
            "sandbox_logs",
            &params(json!({"name": "other", "lines": 5}))
        )
        .await
        .unwrap()
        .ok
    );
}

#[tokio::test]
async fn test_gateway_logs_argv_and_redaction() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    run.on_prefix(
        &["docker", "logs"],
        fixed(ok(
            "line\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz012345\n",
        )),
    );
    let r = a
        .run_action("gateway_logs", &params(json!({"lines": 20})))
        .await
        .unwrap();
    assert!(
        r.ok && run.argvs().contains(
            &["docker", "logs", "--tail", "20", "ai-lab-dd-gateway"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        )
    );
    assert!(!r.output.contains("abcdefghijklmnopqrstuvwxyz012345"));
    assert!(
        !a.run_action("gateway_logs", &params(json!({"lines": 10_000})))
            .await
            .unwrap()
            .ok
    );
}

// prove_policy ---------------------------------------------------------------------------------
type Seen = Arc<std::sync::Mutex<HashMap<&'static str, String>>>;

fn prove_runner(run: &FakeRunner, out: String, code: i32, seen: Seen) {
    run.on_prefix(
        &["openshell", "policy", "prove"],
        with(move |argv| {
            let i = argv.iter().position(|a| a == "--credentials").unwrap();
            let cred = std::path::PathBuf::from(&argv[i + 1]);
            let mut s = seen.lock().unwrap();
            s.insert("descriptor", std::fs::read_to_string(&cred).unwrap());
            s.insert("mode", format!("{:o}", support::mode(&cred)));
            s.insert("path", cred.display().to_string());
            s.insert("argv", serde_json::to_string(argv).unwrap());
            rc(code, &out, "")
        }),
    );
}

#[tokio::test]
async fn test_prove_derives_a_descriptor_without_any_key() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    let seen: Seen = Arc::default();
    prove_runner(
        &run,
        fx("openshell", "prove-clean.raw.txt"),
        0,
        Arc::clone(&seen),
    );
    let r = a
        .run_action("prove_policy", &params(json!({"scopes": "read, chat"})))
        .await
        .unwrap();
    assert!(
        r.ok && r.problems.is_empty()
            && r.output.contains("no findings")
            && !r.output.contains('\x1b'),
        "{}",
        r.output
    );
    let s = seen.lock().unwrap();
    let desc = nemo_console::services::pyyaml::safe_load(&s["descriptor"]).unwrap();
    let want = nemo_console::services::pyyaml::safe_load(
        "credentials:\n- name: litellm_dd_key\n  type: bearer\n  scopes: [read, chat]\n  injected_via: LITELLM_DD_KEY\n  target_hosts: [172.16.0.5]\n",
    )
    .unwrap();
    assert_eq!(desc, want);
    assert!(s["mode"] == "600" && !std::path::Path::new(&s["path"]).exists()); // 0600, removed afterwards
    let argv: Vec<String> = serde_json::from_str(&s["argv"]).unwrap();
    let policy = env.rd().join("policy.yaml").display().to_string();
    assert_eq!(
        argv[..5],
        ["openshell", "policy", "prove", "--policy", &policy]
    );
    assert!(argv.last().unwrap() == "--compact" && !argv.contains(&"-g".to_owned()));
}

#[tokio::test]
async fn test_prove_findings_are_a_result_not_a_failure() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    prove_runner(
        &run,
        fx("openshell", "prove-compact-findings.raw.txt"),
        1,
        Arc::default(),
    );
    let r = a.run_action("prove_policy", &Map::new()).await.unwrap();
    assert!(
        r.ok && r
            .problems
            .iter()
            .map(|p| p.code.as_str())
            .collect::<Vec<_>>()
            == ["openshell.policy.findings"]
    );
    assert!(
        r.problems[0].message.contains("8 finding")
            && r.output.contains("credential_reach_expansion"),
        "{}",
        r.problems[0].message
    );
}

#[tokio::test]
async fn test_prove_error_output_is_a_failure() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    prove_runner(
        &run,
        fx("openshell", "prove-bad.raw.txt"),
        1,
        Arc::default(),
    );
    let r = a
        .run_action("prove_policy", &params(json!({"compact": false})))
        .await
        .unwrap();
    assert!(!r.ok && r.output.contains("parsing credentials YAML"));
    assert!(
        !run.argvs()
            .last()
            .unwrap()
            .contains(&"--compact".to_owned())
    );
}

#[tokio::test]
async fn test_prove_rejects_bad_scopes_and_missing_inputs() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    let many = (0..20)
        .map(|i| format!("s{i}"))
        .collect::<Vec<_>>()
        .join(",");
    for bad in [
        "READ".to_owned(),
        "a;b".into(),
        "x".repeat(40),
        many,
        "-x".into(),
    ] {
        assert!(
            !a.run_action("prove_policy", &params(json!({"scopes": bad})))
                .await
                .unwrap()
                .ok
        );
    }
    assert_eq!(run.count(&["openshell", "policy", "prove"]), 0);
    std::fs::write(
        env.rd().join("profile-lab-router.yaml"),
        "credentials: []\n",
    )
    .unwrap();
    let r = a.run_action("prove_policy", &Map::new()).await.unwrap();
    assert!(!r.ok && r.output.contains("declares no credential"));
}

// gateway lifecycle -----------------------------------------------------------------------------
fn lifecycle(run: &FakeRunner, w: &env::SharedWorld) {
    run.on_prefix(
        &["docker", "compose"],
        fixed(ok("Container ai-lab-dd-gateway  Running\n")),
    );
    run.on_prefix(
        &["docker", "inspect", "ai-lab-litellm-litellm-1"],
        fixed(ok("ai-lab=172.21.0.17 openshell-docker=172.16.0.5 ")),
    );
    run.on_prefix(&["docker", "network"], fixed(ok("")));
    let w = Arc::clone(w);
    run.on_prefix(
        &["docker", "network", "inspect"],
        with(move |_| ok(&format!("{}\n", w.lock().unwrap().internal))),
    );
}

#[tokio::test]
async fn test_gateway_up_mirrors_ensure_gateway() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(None, true);
    lifecycle(&run, &w);
    let r = a.run_action("gateway_up", &Map::new()).await.unwrap();
    let compose = env.rd().join("docker-compose.yml").display().to_string();
    assert!(
        r.ok && r.output.contains("Connected") && r.output.contains("already pinned at 172.16.0.5"),
        "{}",
        r.output
    );
    let calls: Vec<Vec<String>> = run
        .argvs()
        .into_iter()
        .filter(|c| {
            (c[0] == "docker" && (c[1] == "compose" || c[1] == "network"))
                || (c[..2] == ["docker", "inspect"] && c[2] == "ai-lab-litellm-litellm-1")
        })
        .collect();
    let v = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        calls[0],
        v(&[
            "docker",
            "network",
            "inspect",
            "openshell-docker",
            "--format",
            "{{.Internal}}"
        ])
    );
    assert_eq!(
        calls[1],
        v(&["docker", "compose", "-f", &compose, "up", "-d"])
    ); // the exact data-designer-local argv
    assert!(
        !run.argvs()
            .iter()
            .any(|c| c.contains(&"down".to_owned()) || c.contains(&"-v".to_owned()))
    );
}

#[tokio::test]
async fn test_gateway_up_repins_the_router_when_missing() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(None, true);
    lifecycle(&run, &w);
    run.on_prefix(
        &["docker", "inspect", "ai-lab-litellm-litellm-1"],
        fixed(ok("ai-lab=172.21.0.17 ")),
    );
    let r = a.run_action("gateway_up", &Map::new()).await.unwrap();
    assert!(r.ok && r.output.contains("router pinned at 172.16.0.5"));
    let v = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert!(run.argvs().contains(&v(&[
        "docker",
        "network",
        "disconnect",
        "openshell-docker",
        "ai-lab-litellm-litellm-1"
    ])));
    assert!(run.argvs().contains(&v(&[
        "docker",
        "network",
        "connect",
        "--ip",
        "172.16.0.5",
        "--alias",
        "litellm",
        "openshell-docker",
        "ai-lab-litellm-litellm-1"
    ])));
}

#[tokio::test]
async fn test_gateway_up_refuses_without_an_internal_network() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(Some(world(|w| w.internal = "false".into())), true);
    lifecycle(&run, &w);
    let r = a.run_action("gateway_up", &Map::new()).await.unwrap();
    assert!(!r.ok && r.problems[0].code == "openshell.network.not_internal");
    assert_eq!(run.count(&["docker", "compose"]), 0);
}

#[tokio::test]
async fn test_gateway_up_reports_compose_failure_and_readiness_timeout() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(None, true);
    lifecycle(&run, &w);
    run.on_prefix(&["docker", "compose"], fixed(rc(1, "", "boom")));
    let r = a.run_action("gateway_up", &Map::new()).await.unwrap();
    assert!(!r.ok && r.output.contains("gateway compose failed"));
    run.on_prefix(&["docker", "compose"], fixed(ok("")));
    w.lock().unwrap().status_out = fx("openshell", "status-disconnected.HAND-WRITTEN.raw.txt");
    a.set_timing(nemo_console::adapters::openshell::OpenShellTiming {
        follow_interval: 0.01,
        poll_interval: 0.01,
        ready_timeout: 0.05,
    });
    let r = a.run_action("gateway_up", &Map::new()).await.unwrap();
    assert!(
        !r.ok
            && r.output.contains("did not become reachable")
            && r.problems[0].code == "openshell.gateway.disconnected"
    );
}

#[tokio::test]
async fn test_gateway_stop_refuses_with_sandboxes_unless_forced() {
    let env = OsEnv::new();
    let (a, run, w) = env.make(Some(world(|w| w.sandboxes = live_sandbox_list())), true);
    lifecycle(&run, &w);
    let r = a.run_action("gateway_stop", &Map::new()).await.unwrap();
    assert!(
        !r.ok
            && r.problems[0].code == "openshell.stop.sandboxes_exist"
            && r.output.contains("nc-adapter-probe")
    );
    assert_eq!(run.count(&["docker", "compose"]), 0);
    let r = a
        .run_action("gateway_stop", &params(json!({"force": true})))
        .await
        .unwrap();
    let compose = env.rd().join("docker-compose.yml").display().to_string();
    assert!(
        r.ok && run.argvs().contains(
            &["docker", "compose", "-f", &compose, "stop"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        )
    );
    w.lock().unwrap().sandboxes = "[]".into();
    a.invalidate_snapshot().await;
    assert!(a.run_action("gateway_stop", &Map::new()).await.unwrap().ok);
    assert!(!run.argvs().iter().any(|c| c[..2] == ["docker", "compose"]
        && (c.contains(&"down".to_owned()) || c.contains(&"-v".to_owned()))));
}

#[tokio::test]
async fn test_runner_exceptions_are_data() {
    let env = OsEnv::new();
    let boom = FakeRunner::new();
    boom.on(
        |_| true,
        raising("PermissionError", "binary 'openshell' is not allowed"),
    );
    let a = env.adapter(boom, true);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    assert!(!a.metrics().await.unwrap().available);
    assert!(!a.run_action("doctor", &Map::new()).await.unwrap().ok);
}

#[tokio::test]
async fn test_configured_container_names() {
    let env = OsEnv::new();
    let (a, run, _) = env.make(None, true);
    a.set_containers(&["gw-x".to_owned(), "fwd-x".to_owned()]);
    a.status().await.unwrap();
    assert!(
        run.called(&["docker", "inspect"])
            .iter()
            .any(|c| c[c.len() - 2..] == ["gw-x", "fwd-x"])
    );
    let fresh = OpenShellAdapter::new(support::ctx(FakeRunner::new(), &[], &[], env.tmp.path()));
    assert_eq!(
        fresh.containers(),
        ["ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"]
    ); // Settings default
}

#[test]
fn test_runner_factory_roundtrip() {
    let (r, _) = make_runner(None);
    let _: Arc<FakeRunner> = r;
}

#[tokio::test]
async fn test_gateway_state_is_read_right_after_boot() {
    // CI 2026-10-24: a cache stamped 0.0 looked fresh shortly after boot. Rust: a never-filled cache is None (stale
    // at any clock value), so the first events poll asks docker.
    let env = OsEnv::new();
    let (a, _, _) = env.make(None, true);
    assert!(a.gateway_running().await);
}
