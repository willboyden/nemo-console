//! Port of backend/tests/evaluator/test_ev_adapter.py: observe + configure (status, metrics, events, suites.yaml
//! validation). Offline; the fakes answer like the real wrapper.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "evaluator_helpers.rs"]
mod helpers;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use helpers::*;
use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::evaluator::validate_suites;
use nemo_console::domain::models::{Health, PostCheck, Severity};
use nemo_console::services::pyyaml::safe_load;
use serde_json::json;

const RUN: &str = "20261024T072853-123-smoke-lightning";

fn codes_of(st: &nemo_console::domain::models::SystemStatus) -> Vec<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

#[tokio::test]
async fn test_status_up() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Up);
    assert_eq!(st.version.as_deref(), Some("0.3.0"));
    assert!(st.problems.is_empty(), "{:?}", st.problems);
    assert!(st.summary.contains("3 suites") && st.summary.contains("6/6 datasets cached"));
    assert_eq!(st.components[0].name, LAUNCHER);
    assert_eq!(st.endpoints[0].auth, "bearer");
    assert!(!serde_json::to_string(&st).unwrap().contains("sk-"));
}

#[tokio::test]
async fn test_status_not_installed_is_down_with_fix() {
    let f = Fx::new().await;
    let repo = f.path().join("labrepo");
    std::fs::create_dir_all(repo.join("clients/nemo-evaluator")).unwrap();
    std::fs::write(
        repo.join("clients/nemo-evaluator/install.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    let a = f.adapter_with(&FakeRunner::new(), None, true, Some(&repo));
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    assert!(st.version.is_none());
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ev.not_installed")
        .unwrap();
    assert_eq!(p.severity, Severity::Error);
    assert!(
        p.fix
            .as_deref()
            .unwrap()
            .contains("clients/nemo-evaluator/install.sh")
    );
    assert!(!codes_of(&st).contains(&"ev.dataset_missing".to_owned()));
}

#[tokio::test]
async fn test_status_no_key_warns_with_mint_fix() {
    let f = Fx::new().await;
    let a = f.adapter_with(&standard_runner(), None, false, None);
    let st = a.status().await.unwrap();
    let p = st.problems.iter().find(|p| p.code == "ev.no_key").unwrap();
    assert_eq!(p.severity, Severity::Warn);
    assert!(p.fix.as_deref().unwrap().contains("mint-key.sh"));
    assert_eq!(st.health, Health::Up);
}

#[tokio::test]
async fn test_status_router_down_degraded() {
    let f = Fx::with_router(503).await;
    let a = f.adapter(&standard_runner());
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    assert!(codes_of(&st).contains(&"ev.router_down".to_owned()));
}

#[tokio::test]
async fn test_status_missing_datasets_suggest_prefetch() {
    let f = Fx::new().await;
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "cached"],
        ok(&fx("cached_partial.json").replace(
            "\"gsm8k\", \"cached\": true",
            "\"gsm8k\", \"cached\": false",
        )),
    );
    let a = f.adapter(&r);
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ev.dataset_missing")
        .unwrap();
    let fix = p.fix.as_deref().unwrap();
    assert!(fix.starts_with("evaluator-local prefetch gsm8k"));
    assert!(
        !fix.contains("mmlu")
            && p.message.contains("smoke-lightning")
            && p.severity == Severity::Warn
    );
}

#[tokio::test]
async fn test_status_cached_unreadable_is_not_a_false_alarm() {
    let f = Fx::new().await;
    let r = standard_runner();
    r.on(&[LAUNCHER, "cached"], ok("not json"));
    let a = f.adapter(&r);
    let st = a.status().await.unwrap();
    assert!(!codes_of(&st).contains(&"ev.dataset_missing".to_owned()));
    assert_eq!(st.health, Health::Up);
}

#[tokio::test]
async fn test_status_invalid_suites_is_degraded() {
    let f = Fx::new().await;
    let a = f.adapter_with(
        &standard_runner(),
        Some(
            "version: 1\nsuites:\n  x:\n    model: m\n    benchmarks:\n      - bench: humaneval\n",
        ),
        true,
        None,
    );
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ev.suites_invalid")
        .unwrap();
    assert!(p.message.contains("humaneval") && st.health == Health::Degraded);
    assert!(a.actions().unwrap()[0].params[0].choices.is_none());
}

#[tokio::test]
async fn test_metrics_scores_runs_and_verdicts() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    put_run(
        &a,
        "20261024T080000-001-smoke-lightning",
        json!({"ts": 1_792_844_000.0,
        "results": [{"bench": "gsm8k", "status": "ok", "score": 0.9, "samples": 10}]}),
    );
    put_run(
        &a,
        "20261024T070000-001-smoke-lightning",
        json!({"ts": 1_792_840_000.0,
        "results": [{"bench": "gsm8k", "status": "ok", "score": 0.5, "samples": 10}]}),
    );
    put_run(
        &a,
        "20261024T060000-001-knowledge-lightning",
        json!({"verdict": "failed",
        "results": [{"bench": "mmlu", "status": "failed"}]}),
    );
    put_run(
        &a,
        "20261024T050000-001-knowledge-lightning",
        json!({"model": "other/model",
        "results": [{"bench": "mmlu", "status": "ok", "score": 0.48, "samples": 50}]}),
    );
    let broken = a.runs_dir().join("20261024T040000-001-broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("run.json"), "{not json").unwrap();
    let m = a.metrics().await.unwrap();
    assert!(m.available);
    let scores: HashMap<(String, String), f64> = m
        .points
        .iter()
        .filter(|p| p.key == "ev.score")
        .map(|p| {
            (
                (p.labels["bench"].clone(), p.labels["model"].clone()),
                p.value,
            )
        })
        .collect();
    assert_eq!(
        scores,
        [
            (
                ("gsm8k".to_owned(), "aitop/nemotron-lightning".to_owned()),
                0.9
            ),
            (("mmlu".to_owned(), "other/model".to_owned()), 0.48)
        ]
        .into_iter()
        .collect()
    );
    let by: HashMap<String, f64> = m
        .points
        .iter()
        .filter(|p| p.key == "ev.runs_by_verdict")
        .map(|p| (p.labels["verdict"].clone(), p.value))
        .collect();
    assert_eq!(
        by,
        [
            ("succeeded", 3.0),
            ("failed", 1.0),
            ("error", 0.0),
            ("interrupted", 0.0)
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    );
    assert_eq!(
        m.points.iter().find(|p| p.key == "ev.runs").unwrap().value,
        4.0
    );
    assert_eq!(
        m.points
            .iter()
            .find(|p| p.key == "ev.suites")
            .unwrap()
            .value,
        3.0
    );
}

#[tokio::test]
async fn test_metrics_empty_has_no_fabricated_keys() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let m = a.metrics().await.unwrap();
    let keys: std::collections::BTreeSet<&str> = m.points.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["ev.runs", "ev.suites"].into_iter().collect());
    assert_eq!(
        m.points.iter().find(|p| p.key == "ev.runs").unwrap().value,
        0.0
    );
}

#[tokio::test]
async fn test_metrics_score_pairs_are_bounded() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    for i in 0..60 {
        put_run(
            &a,
            &format!("20261024T0{}{}000-001-smoke-lightning", i / 10, i % 10),
            json!({"model": format!("m{i}"), "results": [{"bench": "gsm8k", "score": 0.1, "status": "ok"}]}),
        );
    }
    let m = a.metrics().await.unwrap();
    assert_eq!(m.points.iter().filter(|p| p.key == "ev.score").count(), 50);
}

#[tokio::test]
async fn test_events_metadata_only_and_restart_reads_as_interrupted() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    put_run(
        &a,
        RUN,
        json!({"results": [{"bench": "gsm8k", "status": "ok", "score": 1.0, "samples": 10,
                                          "metric": "pass@1"}], "duration_ms": 2100}),
    );
    put_run(
        &a,
        "20261024T090000-001-knowledge-lightning",
        json!({"verdict": "running"}),
    );
    let evs = a.recent_events(100).await.unwrap();
    assert_eq!(
        evs.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["evaluator.run"; 2]
    );
    let by: HashMap<String, &nemo_console::domain::models::SystemEvent> = evs
        .iter()
        .map(|e| (e.data["run_id"].as_str().unwrap().to_owned(), e))
        .collect();
    assert_eq!(by[RUN].data["verdict"], "succeeded");
    assert_eq!(by[RUN].data["benchmarks"][0]["score"], json!(1.0));
    assert!(by[RUN].summary.contains("gsm8k 1.000"));
    assert_eq!(
        by["20261024T090000-001-knowledge-lightning"].data["verdict"],
        "interrupted"
    );
    assert!(!serde_json::to_string(&evs).unwrap().contains("prompt"));
}

#[tokio::test]
async fn test_follow_events_announces_new_runs() {
    let f = Fx::new().await;
    let a = Arc::new(f.adapter(&standard_runner()));
    let mut stream = Arc::clone(&a).follow_events(None);
    let first = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
    assert!(first.is_err());
    put_run(&a, RUN, json!({"results": []}));
    let ev = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev.data["run_id"], RUN);
}

// ── configure ───────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_config_files_and_read() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let specs = a.config_files().unwrap();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert!(
        spec.id == "suites.yaml" && spec.editable && spec.format == "yaml" && spec.authoritative
    );
    let cf = a.read_config("suites.yaml").await.unwrap();
    assert_eq!(cf.content, fx("suites_good.yaml"));
    assert_eq!(cf.sha256.len(), 64);
    assert!(matches!(
        a.config_path("../../etc/passwd"),
        Err(AdapterError::UnknownId(_))
    ));
    assert!(matches!(
        a.config_path("other.yaml"),
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_config_path_refuses_symlink() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let p = a.suites_path().unwrap();
    let real = f.path().join("elsewhere.yaml");
    std::fs::write(&real, "x: 1\n").unwrap();
    std::fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink(&real, &p).unwrap();
    assert!(matches!(
        a.config_path("suites.yaml"),
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_validate_good_candidate() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    let v = a
        .validate_config("suites.yaml", &fx("suites_good.yaml"))
        .await
        .unwrap();
    assert!(v.ok && v.authoritative);
    assert!(!v.diagnostics.iter().any(|d| d.severity == Severity::Error));
    assert!(
        run.calls()
            .iter()
            .any(|c| c.len() >= 3 && c[..3] == [LAUNCHER, "suites", "--file"])
    );
    assert!(v.note.as_deref().unwrap().contains("agrees"));
    assert_eq!(
        std::fs::read_dir(f.path().join("state/evaluator/tmp"))
            .unwrap()
            .count(),
        0
    );
}

// several mock routers in one test: wiremock's drop blocks on the runtime, so not a current-thread one
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_validate_broken_candidates() {
    let cases = [
        ("- not a mapping\n", "top level"),
        (
            "version: 2\nsuites: {a: {model: m, benchmarks: [{bench: gsm8k, max_problems: 1}]}}\n",
            "`version` must be 1",
        ),
        ("version: 1\nsuites: {}\n", "non-empty mapping"),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: humaneval, max_problems: 5}\n",
            "Docker sandbox",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: simpleqa, max_problems: 5}\n",
            "LLM judge",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: nope, max_problems: 5}\n",
            "unknown benchmark",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k}\n",
            "`max_problems` is required",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 99999}\n",
            "`max_problems`",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: true}\n",
            "`max_problems`",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5, url: x}\n",
            "unknown key 'url'",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    extra: 1\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
            "unknown key 'extra'",
        ),
        (
            "version: 1\nsuites:\n  A_B:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
            "name must match",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: 'bad model;rm'\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
            "`model`",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n      - {bench: gsm8k, max_problems: 5}\n",
            "listed twice",
        ),
        (
            "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5000, repeats: 8}\n",
            "sums to",
        ),
        ("version: 1\nsuites: [unclosed\n", "YAML error"),
    ];
    for (bad, needle) in cases {
        let f = Fx::new().await;
        let run = standard_runner();
        let a = f.adapter(&run);
        let v = a.validate_config("suites.yaml", bad).await.unwrap();
        assert!(!v.ok && v.authoritative, "{bad}");
        assert!(
            v.diagnostics.iter().any(|d| d.message.contains(needle)),
            "{needle}: {:?}",
            v.diagnostics
        );
        assert!(
            !run.calls().iter().any(|c| c[..2] == [LAUNCHER, "suites"]),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn test_validate_reports_line_numbers() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let bad = "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - bench: humaneval\n        max_problems: 5\n";
    let v = a.validate_config("suites.yaml", bad).await.unwrap();
    assert_eq!(
        v.diagnostics.iter().map(|d| d.line).collect::<Vec<_>>(),
        [Some(6)]
    );
}

#[tokio::test]
async fn test_validate_warns_about_unprefetched_datasets_but_is_ok() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    a.status().await.unwrap();
    let cand = "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n";
    let mut view = IndexMap::new();
    view.insert("gsm8k".to_owned(), false);
    a.set_cached_view(Some(view));
    let v = a.validate_config("suites.yaml", cand).await.unwrap();
    assert!(v.ok);
    assert!(
        v.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Warn && d.message.contains("prefetch gsm8k"))
    );
}

#[tokio::test]
async fn test_validate_wrapper_disagreement_is_an_error() {
    let f = Fx::new().await;
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "suites"],
        ok_rc("", 1, "evaluator-local: invalid suites file:\n  boom"),
    );
    let a = f.adapter(&r);
    let v = a
        .validate_config("suites.yaml", &fx("suites_good.yaml"))
        .await
        .unwrap();
    assert!(!v.ok && v.diagnostics.iter().any(|d| d.message.contains("boom")));
}

#[tokio::test]
async fn test_validate_without_tool_uses_schema_only() {
    let f = Fx::new().await;
    let a = f.adapter(&FakeRunner::new());
    let v = a
        .validate_config("suites.yaml", &fx("suites_good.yaml"))
        .await
        .unwrap();
    assert!(v.ok && !v.note.as_deref().unwrap().contains("agrees"));
}

#[tokio::test]
async fn test_post_apply_is_healthy_and_never_restarts() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    let r = a.post_apply("suites.yaml", true).await.unwrap();
    assert!(r.ok && r.post_check == PostCheck::Healthy && !r.restarted);
    assert!(run.calls().is_empty());
}

#[test]
fn test_validate_suites_accepts_the_shipped_file() {
    let doc = safe_load(&fx("suites_good.yaml")).unwrap();
    assert!(validate_suites(&doc).unwrap().is_empty());
}

#[test]
fn test_standard_runner_fixture_is_consistent() {
    let r = standard_runner();
    assert!(r.calls().is_empty());
}
