//! The NeMo Gym adapter: status from `gym-local status` (wrapper, broker, network containment, image, key), the
//! gym.* metrics, the smoke run as a background job (run.json, verdicts, interrupted), the actions and the
//! output parsers. Offline: a fake wrapper, no Docker, no router.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/gym/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::HashSet;
use std::time::Duration;

use env::{GymEnv, LAUNCHER, STATUS_BROKER_UP, STATUS_FRESH, STATUS_OPEN, STATUS_UP};
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::gym::{Net, parse_smoke, parse_status, render_argv};
use nemo_console::domain::models::{Danger, Health, SystemId, SystemStatus};
use nemo_console::services::runner::{GYM_BINARIES, GYM_SMOKE, MAX_TIMEOUT, max_timeout};
use serde_json::{Map, Value, json};

fn codes(st: &SystemStatus) -> HashSet<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

fn comp<'a>(st: &'a SystemStatus, name: &str) -> &'a nemo_console::domain::models::Component {
    st.components.iter().find(|c| c.name == name).unwrap()
}

fn params(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[test]
fn parse_status_reads_every_fact_the_wrapper_prints() {
    let st = parse_status(STATUS_UP);
    assert_eq!(st.broker.as_deref(), Some("Exited (0) 2 hours ago"));
    assert_eq!(st.network, Net::Internal);
    assert_eq!(st.image_built, Some(true));
    assert_eq!(st.key_set, Some(true));
    let fresh = parse_status(STATUS_FRESH);
    assert_eq!(fresh.broker, None);
    assert_eq!(fresh.network, Net::Missing);
    assert_eq!(fresh.image_built, Some(false));
    assert_eq!(fresh.key_set, Some(false));
    assert_eq!(parse_status(STATUS_OPEN).network, Net::Open("false".into()));
    assert_eq!(parse_status("").network, Net::Unknown);
}

#[test]
fn parse_smoke_keeps_only_the_numbers() {
    let (m, rewards) = parse_smoke(env::SMOKE_PASS);
    assert_eq!(m.get("rollouts"), Some(&json!(5)));
    assert_eq!(m.get("rewarded"), Some(&json!(5)));
    assert_eq!(m.get("mean_reward"), Some(&json!(0.8)));
    // 'pass@1': 80.0 in the aggregate is kept as a fraction
    assert_eq!(m.get("pass_at_1"), Some(&json!(0.8)));
    assert_eq!(rewards, vec![1.0, 1.0, 1.0, 0.0, 1.0]);
    // a fraction stays a fraction
    let (m2, _) = parse_smoke(
        "rollouts: 2   with a reward: 2   rewards: [1, 0]   mean: 0.5\naggregate: {'pass@1': 0.5}\n",
    );
    assert_eq!(m2.get("pass_at_1"), Some(&json!(0.5)));
    // 'mean: None' (no rewards) is absent, not zero
    let (m3, r3) = parse_smoke(env::SMOKE_FAIL);
    assert_eq!(m3.get("rollouts"), Some(&json!(0)));
    assert!(m3.get("mean_reward").is_none());
    assert!(r3.is_empty());
}

#[test]
fn argv_and_runner_caps() {
    assert_eq!(
        render_argv("glm-5.3-flash-fp4-local", 5),
        vec![
            "gym-local",
            "smoke",
            "--model",
            "glm-5.3-flash-fp4-local",
            "--limit",
            "5"
        ]
    );
    assert_eq!(GYM_BINARIES, ["gym-local"]);
    let s: Vec<String> = ["gym-local", "smoke"]
        .iter()
        .map(|x| (*x).to_owned())
        .collect();
    assert!((max_timeout(&s) - GYM_SMOKE).abs() < f64::EPSILON);
    let v: Vec<String> = ["gym-local", "verify"]
        .iter()
        .map(|x| (*x).to_owned())
        .collect();
    assert!((max_timeout(&v) - MAX_TIMEOUT).abs() < f64::EPSILON);
}

#[tokio::test]
async fn status_up_when_everything_is_in_place() {
    let e = GymEnv::new();
    let a = e.make(env::runner_with(STATUS_UP), true);
    let st = a.status().await.unwrap();
    assert_eq!(st.system, SystemId::Gym);
    assert_eq!(st.health, Health::Up);
    assert_eq!(st.version.as_deref(), Some("0.7.0"));
    assert!(codes(&st).is_empty(), "{:?}", codes(&st));
    assert_eq!(comp(&st, LAUNCHER).state, "running");
    // a stopped broker is its normal resting state, not a problem
    assert_eq!(comp(&st, "nemo-gym-router-broker").state, "stopped");
    assert_eq!(
        comp(&st, LAUNCHER).image.as_deref(),
        Some("lab/nemo-gym:0.7.0")
    );
    assert!(
        st.summary.contains("key [set]") && st.summary.contains("network internal"),
        "{}",
        st.summary
    );
    // only the contract's component kinds and states (container|systemd|process|http, running|stopped|failed|absent|unknown)
    for c in &st.components {
        assert!(
            ["container", "systemd", "process", "http"].contains(&c.kind.as_str()),
            "{}",
            c.kind
        );
        assert!(
            ["running", "stopped", "failed", "absent", "unknown"].contains(&c.state.as_str()),
            "{}",
            c.state
        );
    }
    // the key file's path and value never reach the status
    let all = serde_json::to_string(&st).unwrap();
    assert!(
        !all.contains("litellm.key"),
        "the key path stays out of the status"
    );
    let st2 = e
        .make(env::runner_with(STATUS_BROKER_UP), true)
        .status()
        .await
        .unwrap();
    assert_eq!(comp(&st2, "nemo-gym-router-broker").state, "running");
}

#[tokio::test]
async fn status_down_without_the_wrapper_names_the_fix() {
    let e = GymEnv::new();
    let a = e.make(env::runner_without_the_launcher(), true);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    assert!(codes(&st).contains("gym.not_installed"));
    let fix = st.problems[0].fix.clone().unwrap();
    assert!(
        fix.contains("ln -s") && fix.contains("clients/nemo-gym/gym-local"),
        "{fix}"
    );
    // no lab files (a clean clone / CI): still a clean down state with a generic fix
    let e2 = GymEnv::new();
    let st2 = e2
        .make(env::runner_without_the_launcher(), false)
        .status()
        .await
        .unwrap();
    assert_eq!(st2.health, Health::Down);
    assert!(
        st2.problems[0]
            .fix
            .clone()
            .unwrap()
            .contains("clients/nemo-gym")
    );
    assert!(st2.version.is_none());
}

#[tokio::test]
async fn status_down_when_the_network_is_not_internal() {
    let e = GymEnv::new();
    let st = e
        .make(env::runner_with(STATUS_OPEN), true)
        .status()
        .await
        .unwrap();
    assert_eq!(st.health, Health::Down);
    assert!(codes(&st).contains("gym.not_contained"));
    assert!(
        st.summary.contains("network NOT internal (Internal=false)"),
        "{}",
        st.summary
    );
}

#[tokio::test]
async fn status_degraded_on_a_fresh_install() {
    let e = GymEnv::new();
    let st = e
        .make(env::runner_with(STATUS_FRESH), true)
        .status()
        .await
        .unwrap();
    assert_eq!(st.health, Health::Degraded);
    let c = codes(&st);
    assert!(
        c.contains("gym.no_image") && c.contains("gym.no_key"),
        "{c:?}"
    );
    // a network that does not exist yet is not a containment problem
    assert!(!c.contains("gym.not_contained"));
    assert!(st.version.is_none());
}

#[tokio::test]
async fn actions_and_their_danger() {
    let e = GymEnv::new();
    let a = e.make(env::runner_with(STATUS_UP), true);
    let acts = a.actions().unwrap();
    let by: std::collections::HashMap<String, Danger> =
        acts.iter().map(|x| (x.id.clone(), x.danger)).collect();
    assert_eq!(
        by.get("smoke"),
        Some(&Danger::Disruptive),
        "a smoke run spends router tokens"
    );
    assert_eq!(by.get("verify"), Some(&Danger::Safe));
    assert_eq!(by.get("report"), Some(&Danger::Safe));
    let v = a.run_action("verify", &Map::new()).await.unwrap();
    assert!(v.ok && v.output.contains("is internal"));
    assert!(a.run_action("nope", &Map::new()).await.is_err());
}

#[tokio::test]
async fn smoke_runs_in_the_background_and_records_the_numbers() {
    let e = GymEnv::new();
    let a = e.make(env::runner_with(STATUS_UP), true);
    let r = a
        .run_action(
            "smoke",
            &params(&[
                ("model", json!("glm-5.3-flash-fp4-local")),
                ("limit", json!(5)),
            ]),
        )
        .await
        .unwrap();
    assert!(r.ok, "{}", r.output);
    assert!(r.output.contains("Started smoke run"));
    tokio::time::timeout(Duration::from_secs(5), a.join_live())
        .await
        .unwrap();
    let meta = a.run_meta("").unwrap();
    assert_eq!(meta.get("verdict"), Some(&json!("succeeded")));
    assert_eq!(meta.get("limit"), Some(&json!(5)));
    assert_eq!(meta["metrics"]["mean_reward"], json!(0.8));
    assert_eq!(meta["rewards"], json!([1.0, 1.0, 1.0, 0.0, 1.0]));
    // the run dir is private
    let dir = e.runs_dir().join(meta["run_id"].as_str().unwrap());
    assert_eq!(support::mode(&dir.join("run.json")) & 0o777, 0o600);
    let m = a.metrics().await.unwrap();
    let keys: HashSet<String> = m.points.iter().map(|p| p.key.clone()).collect();
    for k in [
        "gym.runs",
        "gym.live_running",
        "gym.runs_by_verdict",
        "gym.mean_reward",
        "gym.rollouts",
        "gym.pass_at_1",
        "gym.duration_s",
    ] {
        assert!(keys.contains(k), "missing {k}");
    }
    let ev = a.recent_events(10).await.unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].kind, "gym.smoke");
    let (ok, text) = nemo_console::adapters::gym::report_text(Some(&meta), "");
    assert!(ok && text.contains("Rewards: [1, 1, 1, 0, 1]"), "{text}");
}

#[tokio::test]
async fn a_failed_smoke_keeps_the_wrappers_result_line() {
    let e = GymEnv::new();
    let a = e.make(env::failing_smoke_runner(), true);
    let r = a.run_action("smoke", &Map::new()).await.unwrap();
    assert!(r.ok);
    tokio::time::timeout(Duration::from_secs(5), a.join_live())
        .await
        .unwrap();
    let meta = a.run_meta("").unwrap();
    assert_eq!(meta.get("verdict"), Some(&json!("failed")));
    assert!(
        meta["error"]
            .as_str()
            .unwrap()
            .starts_with("RESULT: FAIL gym eval run exited 1")
    );
    let st = a.status().await.unwrap();
    assert!(codes(&st).contains("gym.last_smoke_failed"));
}

#[tokio::test]
async fn smoke_refuses_bad_input_and_a_missing_wrapper() {
    let e = GymEnv::new();
    let a = e.make(env::runner_with(STATUS_UP), true);
    for bad in [json!("glm 5; rm -rf /"), json!("x".repeat(200))] {
        let r = a
            .run_action("smoke", &params(&[("model", bad)]))
            .await
            .unwrap();
        assert!(!r.ok && r.output.contains("bad model alias"));
    }
    for bad in [json!(0), json!(21), json!(2.5)] {
        let r = a
            .run_action("smoke", &params(&[("limit", bad)]))
            .await
            .unwrap();
        assert!(!r.ok && r.output.contains("1 to 20"));
    }
    assert!(a.live_run_id().is_none(), "nothing started");
    let none = e.make(env::runner_without_the_launcher(), true);
    let r = none.run_action("smoke", &Map::new()).await.unwrap();
    assert!(!r.ok);
    assert_eq!(r.problems[0].code, "gym.not_installed");
    let open = GymEnv::new();
    let r = open
        .make(env::runner_with(STATUS_OPEN), true)
        .run_action("smoke", &Map::new())
        .await
        .unwrap();
    assert!(!r.ok && r.problems[0].code == "gym.not_contained");
}

#[tokio::test]
async fn a_run_left_running_by_a_restart_reads_as_interrupted() {
    let e = GymEnv::new();
    e.seed_run(
        "20261102T230000-123-smoke",
        r#"{"run_id": "20261102T230000-123-smoke", "model": "m", "limit": 5, "ts": 1.0, "verdict": "running"}"#,
    );
    let a = e.make(env::runner_with(STATUS_UP), true);
    let meta = a.run_meta("20261102T230000-123-smoke").unwrap();
    assert_eq!(meta.get("verdict"), Some(&json!("interrupted")));
    // an id that is not a run id is never a path
    assert!(a.run_meta("../../etc").is_none());
    let (ok, _) = nemo_console::adapters::gym::report_text(None, "x");
    assert!(!ok);
}
