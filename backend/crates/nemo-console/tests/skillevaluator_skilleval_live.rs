//! Port of backend/tests/skillevaluator/test_skilleval_live.py: Tier 3 (live agent evaluation) — background start,
//! one run at a time, completion from the CAPTURED result.json (fixtures/tier3_result.json), interrupted runs, the
//! wrapper-only rule, and the core's confirm + param checks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
#[path = "skillevaluator_helpers.rs"]
mod helpers;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use helpers::*;
use nemo_console::adapters::base::{CommandRunner, SystemAdapter};
use nemo_console::adapters::skillevaluator::{
    MAX_FEEDBACK, SkillEvaluatorAdapter, last_json_object, summarize_tier3, tier3_verdict,
};
use nemo_console::config::settings_for_test;
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::SystemId;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Value, json};

fn live() -> serde_json::Map<String, Value> {
    params(json!({"skill": "robust-scripts", "attempts": 2}))
}

async fn finish(a: &SkillEvaluatorAdapter) {
    assert!(a.live().wait(5.0).await, "live run did not finish");
}

fn pts(m: &nemo_console::domain::models::MetricsSnapshot) -> HashMap<String, f64> {
    m.points.iter().map(|p| (p.key.clone(), p.value)).collect()
}

fn gated_runner(gate: &Gate) -> Arc<FakeRunner> {
    let run = standard_runner("skillevaluator-local");
    run.on(
        &["skillevaluator-local", "tier3"],
        gated(writes_tier3(Some("tier3_result.json"), 0, ""), gate),
    );
    run
}

// ── start in the background ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_live_run_starts_in_the_background_and_returns_at_once() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let run = gated_runner(&gate);
    let a = f.adapter(&run);
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        a.run_action("evaluate_live", &live()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        res.ok
            && res.output.contains("Started live evaluation")
            && res.output.contains("in the background")
    );
    let rid = a.live_run_id().unwrap();
    assert!(rid.ends_with("-robust-scripts") && res.output.contains(&rid));
    let run_dir = a.runs_dir().join(&rid);
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "running");
    assert_eq!(meta["tiers"], "tier3");
    assert_eq!(meta["model_calls"], true);
    assert_eq!(
        meta["live"],
        json!({"agent": "opencode", "agent_model": "nemotron-lightning",
               "judge_model": "nemotron-lightning-judge", "attempts": 2, "dataset": "auto",
               "skip_baseline": false})
    );
    assert_eq!(mode_of(&run_dir), 0o700);
    assert_eq!(mode_of(&run_dir.join("run.json")), 0o600);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let t3: Vec<_> = run
        .calls()
        .into_iter()
        .filter(|c| c[1] == "tier3")
        .collect();
    let skill = f.path().join("repo/.claude/skills/robust-scripts");
    assert_eq!(
        t3,
        vec![vec![
            "skillevaluator-local".to_owned(),
            "tier3".into(),
            skill.to_string_lossy().into_owned(),
            "--n-attempts".into(),
            "2".into(),
            "--agent-model".into(),
            "opencode=nemotron-lightning".into(),
            "--judge-model".into(),
            "nemotron-lightning-judge".into(),
            "--dataset".into(),
            "auto".into(),
            "--results-dir".into(),
            run_dir.to_string_lossy().into_owned(),
            "--quiet".into(),
        ]]
    );
    assert_eq!(
        run.cwds().last().unwrap().as_deref(),
        Some(run_dir.as_path())
    );
    // while it runs: status, events and the report say so
    let st = a.status().await.unwrap();
    assert!(st.summary.contains("live: robust-scripts running"));
    assert!(
        st.components
            .iter()
            .any(|c| c.name == "tier3 robust-scripts" && c.state == "running")
    );
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["verdict"], "running");
    assert!(ev.summary.contains("running"));
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(
        rep.ok
            && rep.output.contains("Status: running")
            && rep.output.contains("fills in when the run ends")
    );
    assert_eq!(pts(&a.metrics().await.unwrap())["se.live_running"], 1.0);
    gate.set();
    finish(&a).await;
    assert_eq!(run_json(&a, &rid)["verdict"], "succeeded");
    assert!(a.live_run_id().is_none());
}

#[tokio::test]
async fn test_one_live_run_at_a_time() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let run = gated_runner(&gate);
    let a = f.adapter(&run);
    let first = a.run_action("evaluate_live", &live()).await.unwrap();
    let second = a
        .run_action("evaluate_live", &params(json!({"skill": "serve-model"})))
        .await
        .unwrap();
    assert!(first.ok && !second.ok);
    assert_eq!(
        codes(&second.problems).into_iter().collect::<Vec<_>>(),
        ["skillevaluator.live.busy"]
    );
    assert!(
        second.output.contains("already running")
            && second.output.contains(&a.live_run_id().unwrap())
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(run.calls().iter().filter(|c| c[1] == "tier3").count(), 1);
    assert_eq!(std::fs::read_dir(a.runs_dir()).unwrap().count(), 1);
    gate.set();
    finish(&a).await;
    let third = a
        .run_action("evaluate_live", &params(json!({"skill": "serve-model"})))
        .await
        .unwrap();
    assert!(third.ok);
    finish(&a).await;
}

#[tokio::test]
async fn test_static_runs_keep_working_while_a_live_run_is_in_progress() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let a = f.adapter(&gated_runner(&gate));
    a.run_action("evaluate_live", &live()).await.unwrap();
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "serve-model", "tiers": "static"})),
        )
        .await
        .unwrap();
    assert!(res.ok && res.output.contains("serve-model · static"));
    gate.set();
    finish(&a).await;
}

// ── completion ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_completion_records_verdict_lift_attempts_and_feedback() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    let mut p = live();
    p.insert("agent_model".into(), json!("my-agent"));
    p.insert("judge_model".into(), json!("my-judge"));
    p.insert("dataset".into(), json!("generate"));
    p.insert("skip_baseline".into(), json!(true));
    let res = a.run_action("evaluate_live", &p).await.unwrap();
    assert!(res.ok);
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "succeeded");
    assert_eq!(meta["container_exit"], 0);
    assert_eq!(meta["job"], "20261017-051633-16fc9065");
    assert_eq!(meta["result"], format!("{T3_SKILL}/{T3_RUN}/result.json"));
    let t3 = &meta["tier3"];
    assert_eq!(t3["scored_attempts"], json!(4.0));
    assert_eq!(t3["expected_attempts"], json!(4.0));
    assert_eq!(t3["duration_seconds"], json!(16.571));
    let agent = &t3["agents"]["opencode"];
    assert_eq!(agent["model"], "openai/nemotron-lightning");
    assert_eq!(agent["skill_lift"], json!(0.1597));
    assert_eq!(agent["with_skill"], json!(0.7153));
    assert_eq!(agent["without_skill"], json!(0.5555));
    assert_eq!(
        agent["metrics"]["skill_execution"],
        json!({"with_skill": 0.625, "without_skill": 0.0, "delta": 0.625})
    );
    let mut keys: Vec<&String> = agent["metrics"].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "accuracy",
            "behavior_check",
            "goal_accuracy",
            "security",
            "skill_efficiency",
            "skill_execution"
        ]
    );
    let titles: Vec<&str> = meta["conclusions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles[..2], ["Best performing agent", "Weakest dimension"]);
    let recs = meta["recommendations"].as_array().unwrap();
    assert_eq!(recs.len(), 3);
    assert!(
        recs.iter()
            .all(|r| r["message"].as_str().unwrap().chars().count() <= 400)
    );
    assert_eq!(meta["live"]["dataset"], "generate");
    assert_eq!(meta["live"]["skip_baseline"], true);
    // the event is metadata only: lift numbers yes, feedback text no
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(
        ev.data["tier3"]["agents"]["opencode"]["skill_lift"],
        json!(0.1597)
    );
    assert!(
        ev.summary.contains("lift +0.160 (opencode)")
            && ev.summary.contains("succeeded (4/4 attempts scored)")
    );
    assert!(
        !serde_json::to_string(&ev.data)
            .unwrap()
            .contains("Best performing agent")
    );
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    for needle in [
        "Status: succeeded",
        "Skill Lift",
        "+0.160",
        "skill_execution",
        "Conclusions:",
        "Recommendations:",
        "report.html",
        "judge my-judge",
        "on my-agent",
    ] {
        assert!(rep.output.contains(needle), "{needle}: {}", rep.output);
    }
    assert!(rep.ok);
    let p = pts(&a.metrics().await.unwrap());
    assert_eq!(p["se.last_skill_lift"], 0.1597);
    assert_eq!(p["se.runs_by_verdict{verdict=succeeded}"], 1.0);
    assert_eq!(p["se.live_running"], 0.0);
}

#[tokio::test]
async fn test_failed_execution_status_is_the_verdict() {
    let f = Fx::new().await;
    let mut d: Value = serde_json::from_str(&fx("tier3_result.json")).unwrap();
    d["execution_status"] = json!("failed");
    let ds = d.to_string();
    let run = standard_runner("skillevaluator-local");
    run.on(
        &["skillevaluator-local", "tier3"],
        func(move |argv| {
            let i = argv.iter().position(|x| x == "--results-dir").unwrap();
            let out = Path::new(&argv[i + 1]).join(T3_SKILL).join(T3_RUN);
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join("result.json"), &ds).unwrap();
            res(&tier3_stdout(Some("failed"), None, 1), 1, "")
        }),
    );
    let a = f.adapter(&run);
    a.run_action("evaluate_live", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    assert_eq!(run_json(&a, &rid)["verdict"], "failed");
}

#[tokio::test]
async fn test_no_result_is_an_error_run_with_the_launcher_reason_redacted() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    run.on(
        &["skillevaluator-local", "tier3"],
        ok_rc(
            "",
            1,
            "launch: NOT READY: broker down; Bearer sk-abcdefghijklmnop",
        ),
    );
    let a = f.adapter(&run);
    a.run_action("evaluate_live", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "error");
    assert!(meta["error"].as_str().unwrap().contains("NOT READY"));
    assert!(
        !std::fs::read_to_string(a.runs_dir().join(&rid).join("run.json"))
            .unwrap()
            .contains("sk-abcdefghijklmnop")
    );
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(
        rep.output.contains("No Tier 3 result was written")
            && !rep.output.contains("sk-abcdefghijklmnop")
    );
    // the launcher's own tier3 error (from its summary) wins over stderr noise
    run.on(
        &["skillevaluator-local", "tier3"],
        ok_rc(
            &tier3_stdout(
                None,
                Some("dataset=existing but the skill has no evals/evals.json"),
                1,
            ),
            1,
            "launch: job x",
        ),
    );
    let mut p = live();
    p.insert("dataset".into(), json!("existing"));
    a.run_action("evaluate_live", &p).await.unwrap();
    let rid2 = a.live_run_id().unwrap();
    finish(&a).await;
    assert_eq!(
        run_json(&a, &rid2)["error"],
        "dataset=existing but the skill has no evals/evals.json"
    );
}

#[tokio::test]
async fn test_a_symlinked_result_is_never_read() {
    let f = Fx::new().await;
    let secret = f.path().join("private.json");
    std::fs::write(&secret, fx("tier3_result.json")).unwrap();
    let run = standard_runner("skillevaluator-local");
    run.on(
        &["skillevaluator-local", "tier3"],
        func(move |argv| {
            let i = argv.iter().position(|x| x == "--results-dir").unwrap();
            let out = Path::new(&argv[i + 1]).join(T3_SKILL).join(T3_RUN);
            std::fs::create_dir_all(&out).unwrap();
            std::os::unix::fs::symlink(&secret, out.join("result.json")).unwrap();
            res(&tier3_stdout(Some("succeeded"), None, 0), 0, "")
        }),
    );
    let a = f.adapter(&run);
    a.run_action("evaluate_live", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    assert_eq!(run_json(&a, &rid)["verdict"], "error");
}

// ── interrupted ────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_a_running_run_left_by_a_previous_process_reads_as_interrupted() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let old = f.adapter(&gated_runner(&gate));
    old.run_action("evaluate_live", &live()).await.unwrap();
    let rid = old.live_run_id().unwrap();
    // a new console process over the same state dir: run.json still says running, but no task owns it
    let new = f.adapter(&standard_runner("skillevaluator-local"));
    assert_eq!(run_json(&new, &rid)["verdict"], "running");
    let ev = &new.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["verdict"], "interrupted");
    assert!(ev.summary.contains("interrupted"));
    let rep = new
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(rep.output.contains("Interrupted: the console restarted"));
    assert!(new.run_action("evaluate_live", &live()).await.unwrap().ok);
    finish(&new).await;
    assert_eq!(
        pts(&new.metrics().await.unwrap())["se.runs_by_verdict{verdict=interrupted}"],
        1.0
    );
    // the old process's launcher finished on its own after all: the report shows the result that landed
    gate.set();
    finish(&old).await;
    old.live().clear();
    let late = f.adapter(&standard_runner("skillevaluator-local"));
    let mut m = run_json(&late, &rid);
    m["verdict"] = json!("running");
    std::fs::write(late.runs_dir().join(&rid).join("run.json"), m.to_string()).unwrap();
    let rep = late
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(
        rep.output.contains("Interrupted") && rep.output.contains("result.json landed after that")
    );
    assert!(rep.output.contains("+0.160"), "{}", rep.output);
}

#[tokio::test]
async fn test_console_stop_cancels_the_run_and_marks_it_interrupted() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let a = f.adapter(&gated_runner(&gate));
    a.run_action("evaluate_live", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    a.live().cancel().await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "interrupted");
    assert_eq!(meta["interrupted_by"], "console stop");
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(rep.output.contains("the console stopped while it ran"));
}

// ── refusals and validation ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_plain_skillevaluator_is_refused_for_tier3() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator");
    let a = f.adapter(&run);
    let res = a.run_action("evaluate_live", &live()).await.unwrap();
    assert!(!res.ok);
    assert_eq!(
        codes(&res.problems).into_iter().collect::<Vec<_>>(),
        ["skillevaluator.live.needs_wrapper"]
    );
    assert!(
        res.problems[0]
            .fix
            .as_deref()
            .unwrap()
            .starts_with("install the lab wrapper: clients/skillevaluator/install.sh")
    );
    assert!(res.output.contains("sandboxed runner"));
    assert!(!run.calls().iter().any(|c| c.contains(&"tier3".to_owned())) && !a.runs_dir().exists());
    let repo = f.path().join("lab");
    std::fs::create_dir_all(repo.join("clients/skillevaluator")).unwrap();
    std::fs::write(
        repo.join("clients/skillevaluator/install.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    let b = f.adapter_with(&standard_runner("skillevaluator"), true, Some(&repo));
    let q = b
        .run_action("evaluate_live", &live())
        .await
        .unwrap()
        .problems;
    assert_eq!(
        q[0].fix.as_deref().unwrap(),
        format!(
            "install the lab wrapper: {}/clients/skillevaluator/install.sh   # links skillevaluator-local into ~/.local/bin",
            repo.display()
        )
    );
}

#[tokio::test]
async fn test_not_installed_and_bad_values_are_refused_without_running() {
    let f = Fx::new().await;
    let a = f.adapter(&FakeRunner::new());
    let res = a.run_action("evaluate_live", &live()).await.unwrap();
    assert!(!res.ok);
    assert_eq!(
        codes(&res.problems).into_iter().collect::<Vec<_>>(),
        ["skillevaluator.not_installed"]
    );
    let run2 = standard_runner("skillevaluator-local");
    let b = f.adapter(&run2);
    let with = |k: &str, v: Value| {
        let mut p = live();
        p.insert(k.into(), v);
        p
    };
    for (p, why) in [
        (params(json!({"skill": "linked"})), "unknown skill"),
        (with("attempts", json!(0)), "attempts"),
        (with("attempts", json!(true)), "attempts"),
        (with("agent_model", json!("a b")), "agent_model"),
        (with("judge_model", json!("--version")), "judge_model"),
        (with("dataset", json!("all")), "dataset"),
        (with("skip_baseline", json!("yes")), "skip_baseline"),
    ] {
        let r = b.run_action("evaluate_live", &p).await.unwrap();
        assert!(!r.ok && r.output.contains(why), "{p:?}");
    }
    assert!(!run2.calls().iter().any(|c| c.contains(&"tier3".to_owned())));
    assert!(b.live_run_id().is_none());
}

#[test]
fn test_summarize_tier3_never_invents_and_caps() {
    let s = summarize_tier3(
        json!({"execution_status": "succeeded"})
            .as_object()
            .unwrap(),
    );
    assert_eq!(s["agents"], json!({}));
    assert!(s["scored_attempts"].is_null());
    assert_eq!(s["conclusions"], json!([]));
    let conclusion = json!({"severity": "pass", "title": "t", "message": "x".repeat(5000)});
    let hostile = json!({
        "execution_status": "<script>", "run_id": "../../x",
        "agents": {"bad name;": {}, "opencode": {"model": "m; rm -rf /", "lift": {"overall": {"delta": "1"},
                                                                                "Bad-Key": {"delta": 1}}}},
        "tier3_feedback": {"conclusions": vec![conclusion; 20], "recommendations": ["plain string", 7]},
    });
    let h = summarize_tier3(hostile.as_object().unwrap());
    assert!(h["execution_status"].is_null() && h["run_id"].is_null());
    assert_eq!(
        h["agents"].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["opencode"]
    );
    assert!(
        h["agents"]["opencode"]["model"].is_null()
            && h["agents"]["opencode"]["skill_lift"].is_null()
    );
    assert_eq!(h["agents"]["opencode"]["metrics"], json!({}));
    assert_eq!(h["conclusions"].as_array().unwrap().len(), MAX_FEEDBACK);
    assert_eq!(
        h["conclusions"][0]["message"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        400
    );
    assert_eq!(
        h["recommendations"],
        json!([{"severity": "info", "title": "", "message": "plain string"}])
    );
    assert_eq!(tier3_verdict(&h), "error");
    assert_eq!(
        Value::Object(
            last_json_object(
                "progress\nline\n{\n  \"job\": \"x\",\n  \"summary\": {\n    \"a\": 1\n  }\n}"
            )
            .unwrap()
        ),
        json!({"job": "x", "summary": {"a": 1}})
    );
    assert!(last_json_object("").is_none() && last_json_object("no json here").is_none());
    assert_eq!(
        last_json_object(&tier3_stdout(Some("succeeded"), None, 0)).unwrap()["container_exit"],
        0
    );
}

// ── through the core: confirm required, params validated against the ActionSpec ────────────────

#[tokio::test]
async fn test_core_requires_confirm_and_validates_live_params() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = Arc::new(f.adapter(&run));
    let tmp = f.path();
    std::fs::create_dir_all(tmp.join("home")).unwrap();
    let cfg = settings_for_test(json!({
        "state_dir": tmp.join("state").to_string_lossy(),
        "frontend_dir": tmp.join("front").to_string_lossy(),
        "repo_root": tmp.join("repo").to_string_lossy(),
        "background_tasks": false,
        "trust_loopback": true,
        "urls": {"switchyard": CLOSED, "relay": CLOSED, "dynamo": CLOSED, "dynamo_worker_metrics": CLOSED,
                 "litellm": f.router.uri(), "dcgm": CLOSED, "prometheus": CLOSED, "guardrails": CLOSED},
    }));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = HashMap::new();
    adapters.insert(
        SystemId::Skillevaluator,
        Arc::clone(&a) as Arc<dyn SystemAdapter>,
    );
    let state = build_container(
        cfg,
        Overrides {
            adapters,
            runner: Some(Arc::clone(&run) as Arc<dyn CommandRunner>),
            http: Some(Http::new().unwrap()),
            registry: None,
            environ: Some(Environ::fixed(HashMap::new())),
        },
    )
    .unwrap();
    let url = "/api/v1/systems/skillevaluator/actions/evaluate_live";
    let specs = common::TestReq::new(
        Arc::clone(&state),
        "GET",
        "/api/v1/systems/skillevaluator/actions",
    )
    .send()
    .await
    .json();
    let spec = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "evaluate_live")
        .unwrap()
        .clone();
    assert_eq!(spec["danger"], "disruptive");
    assert_eq!(spec["role"], "admin");
    let defaults: serde_json::Map<String, Value> = spec["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["key"].as_str().unwrap().to_owned(), p["default"].clone()))
        .collect();
    assert_eq!(
        Value::Object(defaults),
        json!({"skill": "robust-scripts", "attempts": 1, "agent_model": "nemotron-lightning",
               "judge_model": "nemotron-lightning-judge", "dataset": "auto", "skip_baseline": false})
    );
    let r = common::TestReq::new(Arc::clone(&state), "POST", url)
        .json(json!({"params": live()}))
        .send()
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.code(), "confirm_required");
    for bad in [
        json!({"attempts": 6}),
        json!({"attempts": "2"}),
        json!({"dataset": "all"}),
        json!({"skip_baseline": "true"}),
        json!({"skill": "linked"}),
        json!({"agent_model": "x\ny"}),
        json!({"n_concurrent": 4}),
    ] {
        let mut p = live();
        for (k, v) in bad.as_object().unwrap() {
            p.insert(k.clone(), v.clone());
        }
        let r = common::TestReq::new(Arc::clone(&state), "POST", url)
            .json(json!({"params": p, "confirm": true}))
            .send()
            .await;
        assert_eq!(r.status, 422, "{bad}");
        assert_eq!(r.code(), "invalid_param", "{bad}");
    }
    assert!(!run.calls().iter().any(|c| c.contains(&"tier3".to_owned())));
    let r = common::TestReq::new(Arc::clone(&state), "POST", url)
        .json(json!({"params": live(), "confirm": true}))
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], true);
    finish(&a).await;
}
