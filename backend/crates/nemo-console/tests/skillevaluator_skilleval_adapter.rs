//! Port of backend/tests/skillevaluator/test_skilleval_adapter.py: discovery and containment, status, evaluate,
//! stored runs (report, events, metrics, retention), report parsing, the runner's SKILL_EVAL_* rule, install fix
//! and the Tier 2 embeddings hint. Offline: fakes + captured fixtures only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "skillevaluator_helpers.rs"]
mod helpers;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use helpers::*;
use nemo_console::adapters::base::{AdapterError, CommandRunner, RunOpts, SystemAdapter};
use nemo_console::adapters::skillevaluator::{
    PINNED_SHA, SkillEvaluatorAdapter, failed_checks, summarize_tier1,
};
use nemo_console::domain::models::Health;
use nemo_console::services::environ::Environ;
use nemo_console::services::runner::SubprocessRunner;
use serde_json::{Value, json};

fn points(
    m: &nemo_console::domain::models::MetricsSnapshot,
) -> std::collections::HashMap<String, f64> {
    m.points.iter().map(|p| (p.key.clone(), p.value)).collect()
}

// ── discovery and containment ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_skills_are_real_dirs_with_skill_md_only() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    assert_eq!(a.skills(), vec!["robust-scripts", "serve-model"]);
}

#[tokio::test]
async fn test_skill_path_refuses_anything_outside_the_list() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    let long = "x".repeat(200);
    for name in [
        "../outside",
        "linked",
        "no-skill-md",
        "",
        "a/b",
        ".hidden",
        long.as_str(),
    ] {
        assert!(
            matches!(a.skill_path(name), Err(AdapterError::UnknownId(_))),
            "{name}"
        );
    }
}

#[tokio::test]
async fn test_evaluate_refuses_an_unknown_skill_without_running() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "linked", "tiers": "static"})),
        )
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("unknown skill"));
    assert!(!run.calls().iter().any(|c| c.contains(&"tier1".to_owned())));
}

// ── status ─────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_status_prefers_the_wrapper_and_is_up() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Up);
    assert_eq!(st.version.as_deref(), Some("0.5.0"));
    assert_eq!(st.components[0].name, "skillevaluator-local");
    assert!(
        st.components[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("own key file")
    );
    assert!(st.summary.contains("2 skills") && st.problems.is_empty());
    assert_eq!(run.calls()[0], vec!["skillevaluator-local", "--version"]);
}

#[tokio::test]
async fn test_version_is_cached_between_polls() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    a.status().await.unwrap();
    a.status().await.unwrap();
    assert_eq!(
        run.calls()
            .iter()
            .filter(|c| c.last().unwrap() == "--version")
            .count(),
        1
    );
}

#[tokio::test]
async fn test_plain_cli_without_wrapper_is_flagged_info() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator"));
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Up);
    assert!(codes(&st.problems).contains("skillevaluator.no_wrapper"));
}

#[tokio::test]
async fn test_not_installed_is_down_with_install_fix() {
    let f = Fx::new().await;
    let a = f.adapter(&FakeRunner::new());
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "skillevaluator.not_installed")
        .unwrap();
    assert!(p.fix.as_deref().unwrap().contains("uv tool install"));
}

#[tokio::test]
async fn test_router_down_is_degraded() {
    let f = Fx::new().await;
    let tmp = f.path();
    let a = SkillEvaluatorAdapter::new(make_ctx(
        &tmp,
        standard_runner("skillevaluator-local") as Arc<dyn CommandRunner>,
        CLOSED,
        true,
        None,
    ));
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    assert!(codes(&st.problems).contains("skillevaluator.router_down"));
}

#[tokio::test]
async fn test_no_skills_dir_is_a_warning_not_a_crash() {
    let f = Fx::new().await;
    let a = f.adapter_with(&standard_runner("skillevaluator-local"), false, None);
    let st = a.status().await.unwrap();
    assert!(codes(&st.problems).contains("skillevaluator.no_skills") && a.skills().is_empty());
    assert!(a.actions().unwrap()[0].params[0].choices.is_none());
}

// ── evaluate ───────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_static_tier_passes_no_llm_and_keeps_the_report() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "robust-scripts", "tiers": "static"})),
        )
        .await
        .unwrap();
    let calls = run.calls();
    let t1 = calls.iter().find(|c| c[1] == "tier1").unwrap();
    let skill = f.path().join("repo/.claude/skills/robust-scripts");
    assert_eq!(
        t1[..3],
        ["skillevaluator-local", "tier1", skill.to_str().unwrap()]
    );
    assert!(t1.contains(&"--no-llm".to_owned()));
    let r = t1.iter().position(|x| x == "-r").unwrap();
    assert_eq!(t1[r + 1], "json,markdown");
    let o = t1.iter().position(|x| x == "-o").unwrap();
    let run_dir = Path::new(&t1[o + 1]).to_path_buf();
    assert_eq!(
        run_dir.parent().unwrap(),
        f.path().join("state/skillevaluator/runs")
    );
    assert_eq!(
        run.cwds().last().unwrap().as_deref(),
        Some(run_dir.as_path())
    );
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(meta["verdict"], "incomplete");
    assert_eq!(meta["model_calls"], false);
    assert_eq!(meta["tier1"]["quality_score"], json!(83.0));
    assert_eq!(meta["tier1"]["grade"], "B");
    assert_eq!(meta["tier1"]["incomplete"], json!(["skillspector"]));
    assert_eq!(mode_of(&run_dir.join("run.json")), 0o600);
    assert_eq!(mode_of(&run_dir), 0o700);
    assert!(
        res.ok
            && res
                .output
                .contains("robust-scripts · static · quality 83.0 B")
    );
    assert!(res.output.contains("INCOMPLETE"));
}

#[tokio::test]
async fn test_tier1_runs_the_llm_judge() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "robust-scripts", "tiers": "tier1"})),
        )
        .await
        .unwrap();
    let calls = run.calls();
    let t1 = calls.iter().find(|c| c[1] == "tier1").unwrap();
    assert!(!t1.contains(&"--no-llm".to_owned()) && !calls.iter().any(|c| c[1] == "tier2"));
    assert!(res.output.contains("judge 83.6"), "{}", res.output);
}

#[tokio::test]
async fn test_tier1_plus_2_runs_dedup_and_records_it() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "serve-model", "tiers": "tier1+2"})),
        )
        .await
        .unwrap();
    let calls = run.calls();
    let t2 = calls.iter().find(|c| c[1] == "tier2").unwrap();
    assert!(t2[2].ends_with("/serve-model") && t2.contains(&"-o".to_owned()));
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["tier2_status"], "passed");
    assert_eq!(ev.data["verdict"], "incomplete");
    assert!(res.output.contains("Tier 2 (deduplication): passed"));
}

#[tokio::test]
async fn test_tier2_failure_downgrades_a_passing_verdict() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    run.on(
        &["skillevaluator-local", "tier1"],
        func(|argv| {
            let mut d: Value = serde_json::from_str(&fx("tier1_incomplete.json")).unwrap();
            d["overall_status"] = json!("passed");
            d["incomplete_scans"] = json!([]);
            let o = argv.iter().position(|x| x == "-o").unwrap();
            std::fs::write(
                Path::new(&argv[o + 1]).join("skillevaluator-tier1.json"),
                d.to_string(),
            )
            .unwrap();
            res("", 0, "")
        }),
    );
    run.on(
        &["skillevaluator-local", "tier2"],
        ok_rc("", 1, "Error: SKILL_EVAL_EMBEDDING_PROVIDER is required"),
    );
    let a = f.adapter(&run);
    a.run_action(
        "evaluate",
        &params(json!({"skill": "serve-model", "tiers": "tier1+2"})),
    )
    .await
    .unwrap();
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["tier2_status"], "error");
    assert_eq!(ev.data["verdict"], "error");
}

#[tokio::test]
async fn test_no_report_is_an_error_run_with_redacted_stderr() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    run.on(
        &["skillevaluator-local", "tier1"],
        ok_rc("", 2, "boom: Authorization: Bearer sk-abcdefghijklmnop"),
    );
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "robust-scripts", "tiers": "tier1"})),
        )
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("did not produce a report"));
    assert!(!res.output.contains("sk-abcdefghijklmnop"));
    let dir = std::fs::read_dir(a.runs_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        !std::fs::read_to_string(dir.join("run.json"))
            .unwrap()
            .contains("sk-abcdefghijklmnop")
    );
}

#[tokio::test]
async fn test_evaluate_all_static_runs_every_skill_without_llm() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let res = a
        .run_action("evaluate_all_static", &params(json!({})))
        .await
        .unwrap();
    let t1: Vec<_> = run
        .calls()
        .into_iter()
        .filter(|c| c[1] == "tier1")
        .collect();
    assert_eq!(t1.len(), 2);
    assert!(t1.iter().all(|c| c.contains(&"--no-llm".to_owned())));
    assert!(res.output.contains("robust-scripts") && res.output.contains("serve-model"));
    assert!(res.output.contains("83.0"));
}

#[tokio::test]
async fn test_unknown_action_raises_keyerror() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    assert!(matches!(
        a.run_action("tier3", &params(json!({}))).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_doctor_runs_the_launcher() {
    let f = Fx::new().await;
    let run = standard_runner("skillevaluator-local");
    let a = f.adapter(&run);
    let res = a.run_action("doctor", &params(json!({}))).await.unwrap();
    assert!(res.ok && res.output.contains("openai-compatible"));
    assert!(run.calls().contains(&vec![
        "skillevaluator-local".to_owned(),
        "doctor".to_owned()
    ]));
}

// ── stored runs: report, events, metrics, retention ────────────────────────────────────────────

#[tokio::test]
async fn test_report_action_newest_and_by_id() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    let empty = a.run_action("report", &params(json!({}))).await.unwrap();
    assert!(!empty.ok && empty.output.contains("no stored runs"));
    a.run_action(
        "evaluate",
        &params(json!({"skill": "robust-scripts", "tiers": "static"})),
    )
    .await
    .unwrap();
    let newest = a
        .run_action("report", &params(json!({"run_id": ""})))
        .await
        .unwrap();
    let rid = a.recent_events(100).await.unwrap()[0].data["run_id"].clone();
    let by_id = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(newest.ok && by_id.output == newest.output);
    let bad = a
        .run_action("report", &params(json!({"run_id": "../../etc/passwd"})))
        .await
        .unwrap();
    assert!(!bad.ok && bad.output == "unknown run id");
}

#[tokio::test]
async fn test_events_and_metrics_follow_the_stored_runs() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    let m0 = a.metrics().await.unwrap();
    assert_eq!(
        points(&m0),
        [("se.skills".to_owned(), 2.0), ("se.runs".to_owned(), 0.0)]
            .into_iter()
            .collect()
    );
    a.run_action(
        "evaluate",
        &params(json!({"skill": "robust-scripts", "tiers": "tier1"})),
    )
    .await
    .unwrap();
    let evs = a.recent_events(100).await.unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, "skilleval.run");
    assert_eq!(evs[0].system.as_str(), "skillevaluator");
    let pts = points(&a.metrics().await.unwrap());
    assert_eq!(pts["se.runs"], 1.0);
    assert_eq!(pts["se.last_quality_score"], 83.0);
    assert_eq!(pts["se.last_judge_score"], 83.6);
    assert_eq!(pts["se.runs_by_verdict{verdict=incomplete}"], 1.0);
    let st = a.status().await.unwrap();
    assert!(st.summary.contains("last run: robust-scripts · tier1"));
}

#[tokio::test]
async fn test_follow_events_yields_only_new_runs() {
    let f = Fx::new().await;
    let a = Arc::new(f.adapter(&standard_runner("skillevaluator-local")));
    a.run_action(
        "evaluate",
        &params(json!({"skill": "robust-scripts", "tiers": "static"})),
    )
    .await
    .unwrap();
    let mut stream = Arc::clone(&a).follow_events(None);
    let first = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
    assert!(first.is_err(), "the old run is not replayed");
    a.run_action(
        "evaluate",
        &params(json!({"skill": "serve-model", "tiers": "static"})),
    )
    .await
    .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev.data["skill"], "serve-model");
}

#[tokio::test]
async fn test_retention_keeps_the_newest_runs() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    a.set_keep_runs(2);
    for _ in 0..3 {
        a.run_action(
            "evaluate",
            &params(json!({"skill": "robust-scripts", "tiers": "static"})),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(std::fs::read_dir(a.runs_dir()).unwrap().count(), 2);
}

// ── report parsing ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_summarize_never_invents_missing_fields() {
    let s = summarize_tier1(json!({"overall_status": "passed"}).as_object().unwrap());
    assert_eq!(s["status"], "passed");
    assert!(s["quality_score"].is_null() && s["rubric_score"].is_null());
    assert_eq!(s["incomplete"], json!([]));
    let r = summarize_tier1(
        json!({"rubric_eval": {"execution_status": "failed", "overall_score": 50}})
            .as_object()
            .unwrap(),
    );
    assert!(r["rubric_score"].is_null());
    assert!(
        failed_checks(
            json!({"results": [{"validator": "X", "findings": [{"severity": "low", "message": "m"}]}]})
                .as_object()
                .unwrap(),
            12
        )
        .is_empty()
    );
    assert!(failed_checks(json!({"results": "garbage"}).as_object().unwrap(), 12).is_empty());
}

// ── runner: SKILL_EVAL_* reaches SkillEvaluator only ───────────────────────────────────────────

#[tokio::test]
async fn test_runner_passes_skill_eval_env_only_to_skillevaluator() {
    // the real runner against tiny fake executables that report which variables reached them
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = "#!/bin/sh\necho \"A=${SKILL_EVAL_LLM_API_KEY:+set} M=${SKILLEVAL_MODEL:-unset} O=${OPENAI_API_KEY:+set}\"\n";
    for name in [
        "skillevaluator",
        "skillevaluator-local",
        "docker",
        "systemctl",
        "nemo-relay",
    ] {
        let p = bin.join(name);
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let dir = bin.to_string_lossy().into_owned();
    let r = SubprocessRunner::with_environ(
        std::slice::from_ref(&dir),
        Environ::from_pairs(&[
            ("PATH", dir.as_str()),
            ("SKILL_EVAL_LLM_API_KEY", "secret-value"),
            ("SKILLEVAL_MODEL", "m"),
            ("OPENAI_API_KEY", "other-secret"),
        ]),
    );
    for b in ["skillevaluator", "skillevaluator-local"] {
        let out = r
            .run(&[b.to_owned(), "x".to_owned()], RunOpts::timeout(10.0))
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "A=set M=m O=", "{b}");
    }
    for b in ["docker", "systemctl", "nemo-relay"] {
        let out = r
            .run(&[b.to_owned(), "x".to_owned()], RunOpts::timeout(10.0))
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "A= M=unset O=", "{b}");
    }
    // the allowlist still gates by bare name, never a path
    let e = r.resolve("/usr/bin/skillevaluator").unwrap_err();
    assert!(e.to_string().contains("not allowed"));
}

// ── install fix and the Tier 2 embeddings hint ─────────────────────────────────────────────────

#[tokio::test]
async fn test_install_fix_is_pinned_or_the_lab_installer() {
    let f = Fx::new().await;
    let a = f.adapter(&FakeRunner::new());
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "skillevaluator.not_installed")
        .unwrap();
    let fix = p.fix.clone().unwrap();
    assert!(fix.contains(&format!("SkillEvaluator.git@{PINNED_SHA}")));
    assert!(!fix.contains("git+https://github.com/NVIDIA/SkillEvaluator.git\""));
    let res = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "robust-scripts", "tiers": "static"})),
        )
        .await
        .unwrap();
    assert_eq!(res.problems[0].fix.as_deref(), Some(fix.as_str()));
    let repo = f.path().join("lab");
    std::fs::create_dir_all(repo.join("clients/skillevaluator")).unwrap();
    std::fs::write(
        repo.join("clients/skillevaluator/install.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    let b = f.adapter_with(&FakeRunner::new(), true, Some(&repo));
    let st = b.status().await.unwrap();
    let q = st
        .problems
        .iter()
        .find(|p| p.code == "skillevaluator.not_installed")
        .unwrap();
    assert!(q.fix.as_deref().unwrap().starts_with(&format!(
        "{}/clients/skillevaluator/install.sh   # pinned SkillEvaluator 7304d76 (v0.5.0)",
        repo.display()
    )));
}

#[tokio::test]
async fn test_tier2_without_embeddings_says_so_with_the_ollama_fix() {
    let f = Fx::new().await;
    let mut t2: Value = serde_json::from_str(&fx("tier2_passed.json")).unwrap();
    t2["overall_status"] = json!("incomplete");
    t2["overall_passed"] = json!(false);
    t2["incomplete_scans"] = json!(["embedding-provider"]);
    let run = standard_runner("skillevaluator-local");
    let t2s = t2.to_string();
    run.on(
        &["skillevaluator-local", "tier2"],
        func(move |argv| {
            let o = argv.iter().position(|x| x == "-o").unwrap();
            std::fs::write(
                Path::new(&argv[o + 1]).join("skillevaluator-tier2.json"),
                &t2s,
            )
            .unwrap();
            res("", 1, "")
        }),
    );
    let repo = f.path().join("lab");
    std::fs::create_dir_all(repo.join("inference/ollama")).unwrap();
    let a = f.adapter_with(&run, true, Some(&repo));
    let res1 = a
        .run_action(
            "evaluate",
            &params(json!({"skill": "serve-model", "tiers": "tier1+2"})),
        )
        .await
        .unwrap();
    assert_eq!(res1.problems.len(), 1);
    let p = &res1.problems[0];
    assert_eq!(p.code, "skillevaluator.tier2.no_embeddings");
    assert!(p.message.contains("embeddings"));
    let want_fix = format!(
        "cd {}/inference/ollama && docker compose up -d",
        repo.display()
    );
    assert_eq!(p.fix.as_deref(), Some(want_fix.as_str()));
    assert!(
        res1.output.contains("Tier 2 could not get embeddings") && res1.output.contains(&want_fix)
    );
    // a run that wrote no report at all (embeddings not configured) gets the same hint
    run.on(
        &["skillevaluator-local", "tier2"],
        ok_rc("", 1, "Error: SKILL_EVAL_EMBEDDING_PROVIDER is required"),
    );
    let b = f.adapter(&run);
    let res2 = b
        .run_action(
            "evaluate",
            &params(json!({"skill": "serve-model", "tiers": "tier1+2"})),
        )
        .await
        .unwrap();
    assert_eq!(
        res2.problems
            .iter()
            .map(|p| p.code.as_str())
            .collect::<Vec<_>>(),
        ["skillevaluator.tier2.no_embeddings"]
    );
    assert!(
        res2.problems[0]
            .fix
            .as_deref()
            .unwrap()
            .contains("SKILL_EVAL_EMBEDDING_MODEL")
    );
    // a passing Tier 2 carries no such hint
    let c = f.adapter(&standard_runner("skillevaluator-local"));
    let res3 = c
        .run_action(
            "evaluate",
            &params(json!({"skill": "serve-model", "tiers": "tier1+2"})),
        )
        .await
        .unwrap();
    assert!(res3.problems.is_empty());
}
