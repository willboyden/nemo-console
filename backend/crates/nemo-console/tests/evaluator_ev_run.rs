//! Port of backend/tests/evaluator/test_ev_run.py: the `run` and `report` actions end to end with golden argv
//! (offline fakes).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "evaluator_helpers.rs"]
mod helpers;

use std::sync::Arc;
use std::time::Duration;

use helpers::*;
use nemo_console::adapters::base::{AdapterError, CommandResult, SystemAdapter};
use nemo_console::adapters::evaluator::EvaluatorAdapter;
use nemo_console::domain::models::ParamType;
use serde_json::json;

async fn finish(a: &EvaluatorAdapter) {
    assert!(a.live().wait(5.0).await, "the run did not finish");
}

fn suite(name: &str) -> serde_json::Map<String, serde_json::Value> {
    params(json!({"suite": name}))
}

#[tokio::test]
async fn test_action_specs() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let acts = a.actions().unwrap();
    assert_eq!(
        acts.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
        ["run", "report"]
    );
    assert_eq!(acts[0].danger.as_str(), "disruptive");
    assert_eq!(acts[1].danger.as_str(), "safe");
    let p = &acts[0].params[0];
    assert_eq!(p.type_, ParamType::Enum);
    assert_eq!(
        p.choices.clone().unwrap(),
        [
            "smoke-lightning",
            "knowledge-lightning",
            "baseline-lightning"
        ]
    );
    assert_eq!(p.default, json!("smoke-lightning"));
}

#[tokio::test]
async fn test_run_end_to_end_golden_argv() {
    let f = Fx::new().await;
    let runner = standard_runner();
    let a = f.adapter(&runner);
    let res = a
        .run_action("run", &suite("smoke-lightning"))
        .await
        .unwrap();
    assert!(res.ok && res.output.contains("Started evaluation"));
    let run_dir = only_run_dir(&a);
    assert_eq!(read_json(&run_dir.join("run.json"))["verdict"], "running");
    finish(&a).await;
    let calls = runner.calls();
    let run_calls: Vec<_> = calls
        .iter()
        .filter(|c| c[..2] == [LAUNCHER, "run"])
        .collect();
    assert_eq!(
        run_calls,
        vec![&vec![
            LAUNCHER.to_owned(),
            "run".into(),
            "--suite".into(),
            "smoke-lightning".into(),
            "--output-dir".into(),
            run_dir.join("out").to_string_lossy().into_owned(),
        ]]
    );
    assert!(calls.iter().all(|c| {
        let j = c.join(" ");
        !j.contains("sk-") && !j.to_lowercase().contains("key")
    }));
    let idx = calls.iter().position(|c| c == run_calls[0]).unwrap();
    assert_eq!(runner.timeouts()[idx], 4800.0);
    let meta = read_json(&run_dir.join("run.json"));
    assert_eq!(meta["verdict"], "succeeded");
    assert_eq!(meta["model"], "aitop/nemotron-lightning");
    assert_eq!(meta["results"][0]["bench"], "gsm8k");
    assert_eq!(meta["results"][0]["score"], json!(1.0));
    assert_eq!(meta["results"][0]["samples"], 10);
    assert!(!meta.to_string().contains("SECRET"));
    assert_eq!(mode_of(&run_dir.join("run.json")), 0o600);
    let m = a.metrics().await.unwrap();
    let sc = m.points.iter().find(|p| p.key == "ev.score").unwrap();
    assert_eq!(sc.labels["bench"], "gsm8k");
    assert_eq!(sc.labels["model"], "aitop/nemotron-lightning");
    assert_eq!(sc.labels.len(), 2);
    assert_eq!(sc.value, 1.0);
    let evs = a.recent_events(100).await.unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].data["verdict"], "succeeded");
    assert!(!serde_json::to_string(&evs[0]).unwrap().contains("SECRET"));
}

#[tokio::test]
async fn test_second_run_is_refused_while_one_is_live() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "run"],
        gated(writes_summary("summary_smoke.json", 0, None), &gate),
    );
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    let second = a
        .run_action("run", &suite("knowledge-lightning"))
        .await
        .unwrap();
    assert!(!second.ok);
    assert_eq!(second.problems[0].code, "ev.run.busy");
    let st = a.status().await.unwrap();
    assert!(
        st.components
            .iter()
            .any(|c| c.name == "run smoke-lightning")
    );
    gate.set();
    finish(&a).await;
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
}

#[tokio::test]
async fn test_run_refuses_unprefetched_datasets_with_the_prefetch_command() {
    let f = Fx::new().await;
    let cached = json!({"cache_dir": "/x", "benchmarks": [{"bench": "gsm8k", "cached": false}, {"bench": "mmlu", "cached": true}]});
    let runner = standard_runner();
    runner.on(&[LAUNCHER, "cached"], ok(&cached.to_string()));
    let a = f.adapter(&runner);
    let res = a
        .run_action("run", &suite("baseline-lightning"))
        .await
        .unwrap();
    assert!(!res.ok);
    assert_eq!(res.problems[0].code, "ev.dataset_missing");
    assert_eq!(
        res.problems[0].fix.as_deref(),
        Some("evaluator-local prefetch gsm8k")
    );
    assert!(!runner.calls().iter().any(|c| c[..2] == [LAUNCHER, "run"]));
    assert!(!a.runs_dir().exists());
}

#[tokio::test]
async fn test_run_refuses_unknown_suite_missing_key_and_missing_tool() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    assert!(
        a.run_action("run", &suite("nope"))
            .await
            .unwrap()
            .output
            .contains("unknown suite")
    );
    assert!(
        a.run_action("run", &params(json!({})))
            .await
            .unwrap()
            .output
            .contains("unknown suite")
    );
    let b = f.adapter_with(&standard_runner(), None, false, None);
    let r = b
        .run_action("run", &suite("smoke-lightning"))
        .await
        .unwrap();
    assert!(!r.ok && r.problems[0].code == "ev.no_key" && r.problems[0].fix.is_some());
    let c = f.adapter(&FakeRunner::new());
    let r = c
        .run_action("run", &suite("smoke-lightning"))
        .await
        .unwrap();
    assert!(!r.ok && r.problems[0].code == "ev.not_installed");
}

#[tokio::test]
async fn test_failed_run_keeps_the_wrappers_error() {
    let f = Fx::new().await;
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "run"],
        writes_summary("summary_failed.json", 1, None),
    );
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
    let meta = read_json(&only_run_dir(&a).join("run.json"));
    assert_eq!(meta["verdict"], "failed");
    assert!(
        meta["error"]
            .as_str()
            .unwrap()
            .contains("OfflineModeIsEnabled")
    );
    let st = a.status().await.unwrap();
    assert!(st.problems.iter().any(|p| p.code == "ev.last_run_failed"));
    let m = a.metrics().await.unwrap();
    assert!(!m.points.iter().any(|p| p.key == "ev.score"));
}

#[tokio::test]
async fn test_wrapper_exit_zero_without_summary_is_error_not_success() {
    let f = Fx::new().await;
    let r = standard_runner();
    r.on(&[LAUNCHER, "run"], ok("done"));
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
    assert_eq!(
        read_json(&only_run_dir(&a).join("run.json"))["verdict"],
        "error"
    );
}

// several mock routers in one test: wiremock's drop blocks on the runtime, so not a current-thread one
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_timeout_and_runner_exception_are_data() {
    let f = Fx::new().await;
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "run"],
        func(|_| CommandResult {
            returncode: -9,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            duration_ms: 0,
        }),
    );
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
    let meta = read_json(&only_run_dir(&a).join("run.json"));
    assert_eq!(meta["verdict"], "failed");
    assert_eq!(meta["error"], "timed out");
    let f2 = Fx::new().await;
    let r2 = standard_runner();
    r2.on(&[LAUNCHER, "run"], raises("RuntimeError", "spawn failed"));
    let b = f2.adapter(&r2);
    assert!(
        b.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&b).await;
    assert_eq!(
        read_json(&only_run_dir(&b).join("run.json"))["verdict"],
        "failed"
    );
}

#[tokio::test]
async fn test_cancel_marks_the_run_interrupted() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let started = Gate::new();
    let s2 = started.clone();
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "run"],
        Arc::new(move |_| {
            let s2 = s2.clone();
            let gate = gate.clone();
            Box::pin(async move {
                s2.set();
                gate.wait().await;
                Ok(CommandResult::new(0, "", ""))
            })
        }),
    );
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    tokio::time::timeout(Duration::from_secs(2), started.wait())
        .await
        .unwrap();
    a.live().cancel().await;
    let meta = read_json(&only_run_dir(&a).join("run.json"));
    assert_eq!(meta["verdict"], "interrupted");
    assert_eq!(meta["interrupted_by"], "console stop");
}

#[tokio::test]
async fn test_interrupted_summary_from_the_wrapper() {
    let f = Fx::new().await;
    let inner = writes_summary("summary_smoke.json", 0, None);
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "run"],
        Arc::new(move |argv: Vec<String>| {
            let inner = Arc::clone(&inner);
            Box::pin(async move {
                let res = inner(argv.clone()).await?;
                let i = argv.iter().position(|a| a == "--output-dir").unwrap();
                let out = std::path::Path::new(&argv[i + 1]).join("summary.json");
                let mut d = read_json(&out);
                d["state"] = json!("interrupted");
                std::fs::write(&out, d.to_string()).unwrap();
                Ok(CommandResult::new(130, &res.stdout, ""))
            })
        }),
    );
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
    assert_eq!(
        read_json(&only_run_dir(&a).join("run.json"))["verdict"],
        "interrupted"
    );
}

#[tokio::test]
async fn test_report_action() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    assert!(!a.run_action("report", &params(json!({}))).await.unwrap().ok);
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
    let rid = only_run_dir(&a)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for p in [json!({}), json!({"run_id": rid})] {
        let r = a.run_action("report", &params(p)).await.unwrap();
        assert!(r.ok, "{}", r.output);
        assert!(r.output.contains("verdict: succeeded"));
        assert!(
            r.output.contains("| gsm8k | ok | pass@1 | 1.0000 |"),
            "{}",
            r.output
        );
        assert!(!r.output.contains("SECRET") && !r.output.contains("sk-"));
    }
    let bad = a
        .run_action("report", &params(json!({"run_id": "../../etc"})))
        .await
        .unwrap();
    assert!(!bad.ok && bad.output.contains("run id"));
    assert!(
        !a.run_action(
            "report",
            &params(json!({"run_id": "20200101T000000-000-ghost"}))
        )
        .await
        .unwrap()
        .ok
    );
}

#[tokio::test]
async fn test_unknown_action_is_keyerror() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    assert!(matches!(
        a.run_action("prefetch", &params(json!({}))).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_prune_keeps_the_newest_hundred() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    for i in 0..105 {
        let d = a.runs_dir().join(format!(
            "2026010{}T0{}{}000-001-smoke-lightning",
            i / 100,
            (i % 100) / 10,
            i % 10
        ));
        std::fs::create_dir_all(&d).unwrap();
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::write(
            d.join("run.json"),
            json!({"verdict": "succeeded", "run_id": name}).to_string(),
        )
        .unwrap();
    }
    assert!(
        a.run_action("run", &suite("smoke-lightning"))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
    assert_eq!(std::fs::read_dir(a.runs_dir()).unwrap().count(), 100);
}
