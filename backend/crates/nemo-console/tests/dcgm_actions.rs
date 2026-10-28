//! Port of backend/tests/dcgm/test_actions.py: golden docker argv (constant templates), diag parsing, run dirs, no
//! serials, failure as data.
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

use std::sync::Arc;
use std::time::Duration;

use env::{CONTAINER, Env, IMG, diag_doc, is_labels_inspect, is_restart, is_rm, is_run};
use futures::StreamExt;
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::dcgm::{DISCOVERY_CMD, DcgmAdapter, diag_cmd, summarize_diag};
use nemo_console::domain::models::{Danger, ParamType};
use serde_json::{Map, Value, json};
use support::{fixed, fx, mode, ok, rc, timed_out, with};

fn golden(name: &str, script: &str) -> Vec<String> {
    [
        "docker",
        "run",
        "--rm",
        "--pull",
        "never",
        "--name",
        name,
        "--gpus",
        "all",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "SYS_ADMIN",
        "--network",
        "none",
        "--security-opt",
        "no-new-privileges",
        "--read-only",
        "--tmpfs",
        "/tmp",
        "--pids-limit",
        "256",
        "--memory",
        "2g",
        "--entrypoint",
        "/bin/sh",
        IMG,
        "-c",
        script,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// learns the GPUs for the enum
async fn prime(a: &DcgmAdapter) {
    a.metrics().await.unwrap();
}

fn params(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn test_constant_templates() {
    assert_eq!(
        diag_cmd(1),
        "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi diag -r 1 -i {gpu} -j"
    );
    assert_eq!(
        diag_cmd(2),
        "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi diag -r 2 -i {gpu} -j"
    );
    assert_eq!(
        DISCOVERY_CMD,
        "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi discovery -l"
    );
}

#[tokio::test]
async fn test_action_specs() {
    let env = Env::new().await;
    let a = env.adapter();
    prime(&a).await;
    let specs = a.actions().unwrap();
    let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        ["diag", "diag_level2", "discovery", "restart_exporter"]
    );
    let by = |id: &str| specs.iter().find(|s| s.id == id).unwrap().clone();
    assert!(by("diag").danger == Danger::Disruptive && by("discovery").danger == Danger::Safe);
    assert!(
        by("diag_level2").danger == Danger::Disruptive
            && by("restart_exporter").danger == Danger::Disruptive
    );
    let g = &by("diag").params[0];
    assert!(
        g.key == "gpu"
            && g.type_ == ParamType::Enum
            && g.choices == Some(vec!["0".into(), "1".into()])
    );
    assert!(g.help.contains("Max-Q") && !g.help.contains("GPU-"));
    assert_eq!(
        by("diag_level2").params[0].choices,
        Some(vec!["0".into(), "1".into()])
    );
}

#[tokio::test]
async fn test_gpu_param_before_discovery_is_a_bounded_int() {
    let env = Env::new().await;
    let a = env.adapter();
    let g = a.actions().unwrap()[0].params[0].clone();
    assert!(
        g.type_ == ParamType::Int
            && g.min == Some(0.0)
            && g.max == Some(15.0)
            && g.choices.is_none()
    );
    assert_eq!(g.default, json!(0));
}

#[tokio::test]
async fn test_diag_level1_golden_argv_run_dir_and_event() {
    let env = Env::new().await;
    env.runner
        .on(is_run, fixed(ok(&fx("dcgm", "diag_level1_gpu0.json")))); // REAL level-1 output, serial redacted
    let a = env.adapter();
    prime(&a).await;
    let res = a
        .run_action("diag", &params(json!({"gpu": "0"})))
        .await
        .unwrap();
    assert!(res.ok, "{}", res.output);
    let runs = env.runner.called(&["docker", "run"]);
    assert_eq!(runs.len(), 1);
    let argv = &runs[0];
    assert_eq!(
        argv,
        &golden(
            &argv[6],
            "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi diag -r 1 -i 0 -j"
        )
    );
    assert_eq!(
        argv[..6],
        ["docker", "run", "--rm", "--pull", "never", "--name"]
    );
    let re = regex::Regex::new(r"\Anemo-console-dcgm-\d{8}t\d{6}-\d{3}-diag1-gpu0\z").unwrap();
    assert!(re.is_match(&argv[6]), "{}", argv[6]);
    assert!(
        res.output.contains("PASS")
            && res.output.contains("software")
            && res.output.contains("RTX PRO 6000 Blackwell Max-Q")
    );
    let runs_dir = env.state().join("dcgm").join("runs");
    let entries: Vec<_> = std::fs::read_dir(&runs_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let run = &entries[0];
    assert_eq!(mode(run), 0o700);
    assert_eq!(mode(&run.join("run.json")), 0o600);
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(run.join("run.json")).unwrap()).unwrap();
    assert!(meta["verdict"] == "pass" && meta["gpu"] == 0 && meta["level"] == 1);
    let evs = a.recent_events(100).await.unwrap();
    let d: Vec<_> = evs.iter().filter(|e| e.kind == "dcgm.diag").collect();
    assert!(
        d.len() == 1
            && d[0].data["verdict"] == "pass"
            && d[0].data["tests"] == json!({"software": "Pass"})
    );
}

#[tokio::test]
async fn test_serial_numbers_never_stored_or_shown() {
    let env = Env::new().await;
    let mut doc: Value = serde_json::from_str(&fx("dcgm", "diag_level1_gpu0.json")).unwrap();
    doc["entity_groups"][0]["entities"][0]["serial_num"] = json!("1794225037164");
    env.runner.on(is_run, fixed(ok(&doc.to_string())));
    let a = env.adapter();
    prime(&a).await;
    let res = a
        .run_action("diag", &params(json!({"gpu": 0})))
        .await
        .unwrap();
    let mut blob = res.output.clone();
    for e in walk(&env.state()) {
        if e.extension().is_some_and(|x| x == "json") {
            blob.push_str(&std::fs::read_to_string(&e).unwrap());
        }
    }
    for e in a.recent_events(100).await.unwrap() {
        blob.push_str(&serde_json::to_string(&e).unwrap());
    }
    assert!(!blob.contains("1794225037164") && !blob.to_lowercase().contains("serial"));
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
async fn test_level2_is_its_own_disruptive_action_with_longer_timeout() {
    let env = Env::new().await;
    let runner = support::FakeRunner::new();
    let doc = diag_doc(
        &[("memory", "Pass"), ("diagnostic", "Warn")],
        Some("ECC pending"),
    );
    runner.on(
        |_| true,
        with(move |argv| {
            if argv[..2] == ["docker", "run"] {
                ok(&doc)
            } else {
                ok("sha256:x\n")
            }
        }),
    );
    let a = env.adapter_with(Arc::clone(&runner), &env.url(), &env.url());
    prime(&a).await;
    let res = a
        .run_action("diag_level2", &params(json!({"gpu": "1"})))
        .await
        .unwrap();
    assert!(
        res.ok && res.output.contains("WARN") && res.output.contains("ECC pending"),
        "{}",
        res.output
    );
    assert!(runner.call_log().iter().any(|c| c.timeout == 600.0));
}

#[tokio::test]
async fn test_failed_test_is_not_ok() {
    let env = Env::new().await;
    env.runner.on(
        is_run,
        fixed(ok(&diag_doc(
            &[("memory", "Fail")],
            Some("uncorrectable ECC on GPU 1"),
        ))),
    );
    let a = env.adapter();
    prime(&a).await;
    let res = a
        .run_action("diag_level2", &params(json!({"gpu": "1"})))
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("FAIL"));
    let evs: Vec<_> = a
        .recent_events(100)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "dcgm.diag")
        .collect();
    assert!(evs.len() == 1 && evs[0].data["verdict"] == "fail");
}

#[test]
fn test_summary_level_warnings_are_reported() {
    // Real shape seen live under a read-only root: results Pass, test_summary Fail + warnings.
    let doc = json!({"DCGM Diagnostic": {"test_categories": [{"tests": [{"name": "software", "results": [{"status": "Pass"}],
        "test_summary": {"status": "Fail", "warnings": [{"warning": "No permission to create a file"}]}}]}]}});
    let (verdict, _, notes) = summarize_diag(&doc);
    assert!(verdict == "fail" && notes == ["No permission to create a file"]);
}

#[test]
fn test_summarize_diag_handles_odd_shapes() {
    assert_eq!(summarize_diag(&json!({})).0, "error");
    assert_eq!(
        summarize_diag(&json!({"DCGM Diagnostic": {"test_categories": [{"tests": [{"name": "x", "results": [{"status": "Skip"}]}]}]}})).0,
        "pass"
    );
    assert_eq!(
        summarize_diag(&json!({"DCGM Diagnostic": "nonsense"})).0,
        "error"
    );
}

#[tokio::test]
async fn test_gpu_param_is_validated_before_anything_runs() {
    for bad_gpu in [
        json!("2"),
        json!("-1"),
        json!("0; rm -rf /"),
        json!(""),
        Value::Null,
        json!(true),
        json!(1.5),
        json!("01x"),
        json!("99999"),
    ] {
        let env = Env::new().await;
        let a = env.adapter();
        prime(&a).await;
        let res = a
            .run_action("diag", &params(json!({"gpu": bad_gpu})))
            .await
            .unwrap();
        assert!(
            !res.ok && res.output.contains("gpu must be"),
            "{bad_gpu}: {}",
            res.output
        );
        assert!(env.runner.called(&["docker", "run"]).is_empty());
    }
}

#[tokio::test]
async fn test_timeout_removes_the_container() {
    let env = Env::new().await;
    env.runner.on(is_run, fixed(timed_out()));
    env.runner.on(is_rm, fixed(ok("")));
    let a = env.adapter();
    prime(&a).await;
    let res = a
        .run_action("diag", &params(json!({"gpu": "0"})))
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("timed out"));
    let rm = env.runner.called(&["docker", "rm", "-f"]);
    assert!(rm.len() == 1 && rm[0][3].starts_with("nemo-console-dcgm-"));
}

#[tokio::test]
async fn test_missing_image_is_an_actionable_problem() {
    let env = Env::new().await;
    env.runner.on(
        is_run,
        fixed(rc(125, "", "Unable to find image 'x' locally")),
    );
    let a = env.adapter();
    prime(&a).await;
    let res = a
        .run_action("diag", &params(json!({"gpu": "0"})))
        .await
        .unwrap();
    assert!(!res.ok);
    let p = res
        .problems
        .iter()
        .find(|p| p.code == "dcgm.diag.image_missing")
        .unwrap();
    assert_eq!(
        p.fix.as_deref(),
        Some(format!("docker pull {IMG}").as_str())
    );
}

#[tokio::test]
async fn test_no_docker_is_data() {
    let env = Env::new().await;
    let a = env.adapter();
    env.runner.clear();
    let res = a.run_action("discovery", &Map::new()).await.unwrap();
    assert!(!res.ok && res.output.contains("failed"));
}

#[tokio::test]
async fn test_only_one_diag_at_a_time() {
    let env = Env::new().await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let g = Arc::clone(&gate);
    let level1 = fx("dcgm", "diag_level1_gpu0.json");
    env.runner.on(
        is_run,
        Arc::new(move |_| {
            let g = Arc::clone(&g);
            let out = level1.clone();
            Box::pin(async move {
                let _permit = g.acquire().await;
                Ok(ok(&out))
            })
        }),
    );
    let a = env.adapter();
    prime(&a).await;
    let a1 = Arc::clone(&a);
    let first = tokio::spawn(async move {
        a1.run_action("diag", &params(json!({"gpu": "0"})))
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = a
        .run_action("diag", &params(json!({"gpu": "1"})))
        .await
        .unwrap();
    assert!(!second.ok && second.output.contains("another diagnostic"));
    gate.add_permits(10);
    assert!(first.await.unwrap().ok);
}

#[tokio::test]
async fn test_discovery_shortens_uuids() {
    let env = Env::new().await;
    env.runner
        .on(is_run, fixed(ok(&fx("dcgm", "discovery.txt"))));
    let a = env.adapter();
    let res = a.run_action("discovery", &Map::new()).await.unwrap();
    assert!(res.ok && res.output.contains("2 GPUs found") && res.output.contains("Max-Q"));
    assert!(
        !res.output
            .contains("GPU-00000000-0000-0000-0000-000000000001")
            && res.output.contains("GPU-00000000...")
    );
    let runs = env.runner.called(&["docker", "run"]);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].last().unwrap(),
        "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi discovery -l"
    );
    assert!(runs[0][6].starts_with("nemo-console-dcgm-disc-"));
}

#[tokio::test]
async fn test_restart_exporter() {
    let env = Env::new().await;
    let a = env.adapter();
    let res = a.run_action("restart_exporter", &Map::new()).await.unwrap();
    assert!(
        res.ok
            && env.runner.called(&["docker", "restart"])
                == vec![vec![
                    "docker".to_owned(),
                    "restart".into(),
                    CONTAINER.into()
                ]]
    );
    env.runner
        .insert_first(is_restart, fixed(rc(1, "", "Error: No such container")));
    assert!(
        !a.run_action("restart_exporter", &Map::new())
            .await
            .unwrap()
            .ok
    );
}

#[tokio::test]
async fn test_restart_refused_for_a_foreign_container() {
    let env = Env::new().await;
    env.runner
        .insert_first(is_labels_inspect, fixed(ok("someone-else|dcgm-exporter\n")));
    let a = env.adapter();
    let res = a.run_action("restart_exporter", &Map::new()).await.unwrap();
    assert!(
        !res.ok
            && res.output.contains("refusing to restart")
            && res.output.contains("someone-else")
    );
    let ap = a.post_apply("counters", true).await.unwrap();
    assert!(
        ap.post_check == nemo_console::domain::models::PostCheck::Unhealthy
            && !ap.restarted
            && ap.detail.contains("refusing")
    );
    env.runner.insert_first(
        is_labels_inspect,
        fixed(ok("ai-lab-observability|prometheus\n")),
    );
    assert!(
        !a.run_action("restart_exporter", &Map::new())
            .await
            .unwrap()
            .ok
    );
    assert!(env.runner.called(&["docker", "restart"]).is_empty());
}

#[tokio::test]
async fn test_restart_refused_when_labels_unreadable() {
    let env = Env::new().await;
    env.runner
        .insert_first(is_labels_inspect, fixed(rc(1, "", "Error: No such object")));
    let a = env.adapter();
    let res = a.run_action("restart_exporter", &Map::new()).await.unwrap();
    assert!(
        !res.ok
            && res.output.contains("cannot inspect")
            && env.runner.called(&["docker", "restart"]).is_empty()
    );
}

#[tokio::test]
async fn test_unknown_action_is_keyerror() {
    let env = Env::new().await;
    let err = env
        .adapter()
        .run_action("rm_rf", &Map::new())
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::UnknownId(ref k) if k == "rm_rf"));
}

#[tokio::test]
async fn test_runs_are_pruned_and_ignored_when_foreign() {
    let env = Env::new().await;
    let runs = env.state().join("dcgm").join("runs");
    std::fs::create_dir_all(runs.join("not-a-run")).unwrap();
    for i in 0..105 {
        let d = runs.join(format!("20260101T0000{}0-{:03}-diag1-gpu0", i / 100, i));
        std::fs::create_dir(&d).unwrap();
        let meta = json!({"run_id": d.file_name().unwrap().to_str().unwrap(), "ts": 1.0 + f64::from(i), "gpu": 0, "level": 1,
                          "verdict": "pass", "tests": []});
        std::fs::write(d.join("run.json"), meta.to_string()).unwrap();
    }
    env.runner
        .on(is_run, fixed(ok(&fx("dcgm", "diag_level1_gpu0.json"))));
    let a = env.adapter();
    prime(&a).await;
    a.run_action("diag", &params(json!({"gpu": 0})))
        .await
        .unwrap();
    let n = std::fs::read_dir(&runs)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name() != "not-a-run")
        .count();
    assert_eq!(n, 100);
}

#[tokio::test]
async fn test_follow_yields_a_diag_run_once() {
    let env = Env::new().await;
    env.runner
        .on(is_run, fixed(ok(&fx("dcgm", "diag_level1_gpu0.json"))));
    let a = env.adapter();
    prime(&a).await;
    let mut stream = Arc::clone(&a).follow_events(None);
    // the first pass is the baseline
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err()
    );
    a.run_action("diag", &params(json!({"gpu": 0})))
        .await
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev.kind, "dcgm.diag");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err()
    ); // once
}
