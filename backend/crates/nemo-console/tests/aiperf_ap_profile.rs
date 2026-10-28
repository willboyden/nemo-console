//! Port of backend/tests/aiperf/test_ap_profile.py: the ONE background run, verdicts, run dirs, report, preflight
//! refusals, and the core's confirm + param checks. Offline: fakes + the captured live fixtures only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

mod common;
#[path = "fixtures/aiperf/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;
mod support_b;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use env::{ApEnv, LAUNCHER, Opts, gated, standard_runner, writes_profile, writes_records};
use nemo_console::adapters::aiperf::{AiPerfAdapter, KEEP_RUNS, request_errors, write_meta};
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::domain::models::{ActionResult, SystemId};
use serde_json::{Map, Value, json};
use support::{FakeRunner, fixed, fx, mode, ok, rc, timed_out};

fn smoke() -> Map<String, Value> {
    json!({"preset": "smoke-lightning"})
        .as_object()
        .unwrap()
        .clone()
}

fn codes(res: &ActionResult) -> HashSet<String> {
    res.problems.iter().map(|p| p.code.clone()).collect()
}

async fn finish(a: &AiPerfAdapter) {
    assert!(a.has_live());
    tokio::time::timeout(Duration::from_secs(5), a.join_live())
        .await
        .unwrap();
}

fn run_json(a: &AiPerfAdapter, run_id: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(a.runs_dir().join(run_id).join("run.json")).unwrap(),
    )
    .unwrap()
}

async fn run_to_end(a: &AiPerfAdapter, params: Option<Map<String, Value>>) -> (String, Value) {
    let res = a
        .run_action("profile", &params.unwrap_or_else(smoke))
        .await
        .unwrap();
    assert!(res.ok, "{}", res.output);
    let rid = a.live_run_id().unwrap();
    finish(a).await;
    let meta = run_json(a, &rid);
    (rid, meta)
}

fn profile_calls(run: &FakeRunner) -> Vec<Vec<String>> {
    run.argvs()
        .into_iter()
        .filter(|c| c.get(1).map(String::as_str) == Some("profile"))
        .collect()
}

// ── start in the background, golden launch args ───────────────────────────────────────────────
#[tokio::test]
async fn test_profile_starts_in_the_background_with_golden_args() {
    let env = ApEnv::new().await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let run = standard_runner();
    run.on_prefix(&[LAUNCHER, "profile"], gated(Arc::clone(&gate)));
    let (a, _) = env.make(Some(Arc::clone(&run)), &Opts::default());
    let res = tokio::time::timeout(Duration::from_secs(2), a.run_action("profile", &smoke()))
        .await
        .unwrap()
        .unwrap(); // does not wait
    assert!(
        res.ok
            && res.output.contains("Started load run")
            && res.output.contains("in the background")
    );
    let rid = a.live_run_id().unwrap();
    assert!(rid.ends_with("-smoke-lightning"));
    let run_dir = a.runs_dir().join(&rid);
    let meta = run_json(&a, &rid);
    assert!(
        meta["verdict"] == "running"
            && meta["preset"] == "smoke-lightning"
            && meta["concurrency"] == 2
    );
    assert!(mode(&run_dir) == 0o700 && mode(&run_dir.join("run.json")) == 0o600);
    tokio::time::sleep(Duration::from_millis(10)).await;
    let calls: Vec<_> = run
        .call_log()
        .into_iter()
        .filter(|c| c.argv.get(1).map(String::as_str) == Some("profile"))
        .collect();
    assert_eq!(calls.len(), 1);
    let want: Vec<String> = [
        "aiperf-local",
        "profile",
        "--model",
        "aitop/nemotron-lightning",
        "--endpoint-type",
        "chat",
        "--streaming",
        "--concurrency",
        "2",
        "--request-count",
        "8",
        "--prompt-input-tokens-mean",
        "128",
        "--prompt-output-tokens-mean",
        "64",
        "--warmup-request-count",
        "1",
        "--request-timeout-seconds",
        "120",
        "--random-seed",
        "42",
        "--export-level",
        "records",
        "--use-server-token-count",
        "--artifact-dir",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .chain(std::iter::once(
        run_dir.join("artifacts").display().to_string(),
    ))
    .collect();
    assert_eq!(calls[0].argv, want);
    assert!(calls[0].cwd.as_deref() == Some(run_dir.as_path()) && calls[0].timeout == 3600.0);
    let evs = a.recent_events(100).await.unwrap();
    assert!(evs.len() == 1 && evs[0].data["verdict"] == "running");
    gate.add_permits(10);
    finish(&a).await;
    assert_eq!(run_json(&a, &rid)["verdict"], "succeeded");
}

#[tokio::test]
async fn test_one_run_at_a_time() {
    let env = ApEnv::new().await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let run = standard_runner();
    run.on_prefix(&[LAUNCHER, "profile"], gated(Arc::clone(&gate)));
    let (a, _) = env.make(Some(Arc::clone(&run)), &Opts::default());
    assert!(a.run_action("profile", &smoke()).await.unwrap().ok);
    let second = a
        .run_action(
            "profile",
            json!({"preset": "latency-lightning"}).as_object().unwrap(),
        )
        .await
        .unwrap();
    assert!(!second.ok && codes(&second).contains("ap.profile.busy"));
    assert!(profile_calls(&run).len() <= 1);
    let st = a.status().await.unwrap();
    assert!(
        st.components
            .iter()
            .any(|c| c.name == "profile smoke-lightning" && c.state == "running")
    );
    let m = a.metrics().await.unwrap();
    assert!(m.points[1].key == "ap.live_running" && m.points[1].value == 1.0);
    gate.add_permits(10);
    finish(&a).await;
    assert!(a.run_action("profile", &smoke()).await.unwrap().ok); // free again
    finish(&a).await;
}

// ── verdicts ───────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_success_parses_the_captured_artifacts() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let (_, meta) = run_to_end(&a, None).await;
    assert!(
        meta["verdict"] == "succeeded"
            && meta["aiperf_version"] == "0.13.0"
            && meta["duration_ms"].as_i64().unwrap() >= 0
    );
    assert!((meta["metrics"]["ttft_p50_ms"].as_f64().unwrap() - 29.963_736_5).abs() < 1e-9);
    assert!(
        (meta["metrics"]["output_tps"].as_f64().unwrap() - 1_532.991_017_636_744_1).abs() < 1e-9
    );
    assert_eq!(meta["metrics"]["requests"].as_f64(), Some(8.0));
    let raw = serde_json::to_string(&meta).unwrap();
    assert!(meta.get("stdout").is_none() && !raw.contains("api_key"));
    let snap = a.metrics().await.unwrap();
    let get = |key: &str, label: Option<(&str, &str)>| {
        snap.points
            .iter()
            .find(|p| {
                p.key == key
                    && label.is_none_or(|(k, v)| p.labels.get(k).map(String::as_str) == Some(v))
                    && (label.is_some() || p.labels.is_empty())
            })
            .map(|p| p.value)
            .unwrap()
    };
    assert!((get("ap.ttft_ms", Some(("q", "p99"))) - 40.695_334_159_999_994).abs() < 1e-9);
    assert!(
        get("ap.runs", None) == 1.0
            && get("ap.runs_by_verdict", Some(("verdict", "succeeded"))) == 1.0
    );
    let evs = a.recent_events(100).await.unwrap();
    assert!(evs.len() == 1 && evs[0].data["verdict"] == "succeeded");
    assert!(
        (evs[0].data["ttft_p50_ms"].as_f64().unwrap() - 29.963_736_5).abs() < 1e-9
            && evs[0].summary.contains("TTFT p50 30 ms")
    );
}

#[tokio::test]
async fn test_failure_text_is_cleaned_of_boxes_and_ansi() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    run.on_prefix(
        &[LAUNCHER, "profile"],
        fixed(rc(
            1,
            "",
            &format!("\x1b[31m{}\x1b[0m", fx("aiperf", "all-failed.txt")),
        )),
    );
    let (a, _) = env.make(Some(run), &Opts::default());
    let (rid, meta) = run_to_end(&a, None).await;
    let err = meta["error"].as_str().unwrap();
    assert!(
        meta["verdict"] == "failed" && err.contains("All 1 inference request(s) failed"),
        "{err}"
    );
    assert!(!err.contains('│') && !err.contains('\x1b') && meta.get("metrics").is_none());
    assert_eq!(a.metrics().await.unwrap().points[0].value, 1.0);
    let (ok_, text) = a.report_text(&rid);
    assert!(
        ok_ && text.contains("AIPerf did not complete the run")
            && text.contains("All 1 inference request(s) failed")
    );
}

#[tokio::test]
async fn test_failure_reports_the_routers_own_reason_per_request() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    run.on_prefix(&[LAUNCHER, "profile"], writes_records());
    let (a, _) = env.make(Some(run), &Opts::default());
    let (rid, meta) = run_to_end(&a, None).await;
    let err = meta["error"].as_str().unwrap();
    assert!(meta["verdict"] == "failed" && meta["failed_requests"] == 2);
    assert!(err.contains("2 request(s) failed; the first: HTTP 400 Bad Request: /chat/completions: Invalid model name"), "{err}");
    assert!(err.contains("no-such-model-xyz") && !err.contains('{')); // the inner message, not the JSON blob
    assert!(a.report_text(&rid).1.contains("Invalid model name"));
}

#[tokio::test]
async fn test_report_mentions_partial_failures() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let d = a.runs_dir().join("20261024T100000-001-smoke-lightning");
    std::fs::create_dir_all(&d).unwrap();
    let meta = json!({"run_id": "20261024T100000-001-smoke-lightning", "preset": "smoke-lightning", "verdict": "succeeded",
                      "failed_requests": 3, "first_error": "HTTP 429 Too Many Requests: slow down", "metrics": {"ttft_p50_ms": 1.0}});
    std::fs::write(d.join("run.json"), meta.to_string()).unwrap();
    assert!(
        a.report_text("20261024T100000-001-smoke-lightning")
            .1
            .contains("3 request(s) failed; the first: HTTP 429 Too Many Requests: slow down")
    );
}

#[tokio::test]
async fn test_request_errors_reader_is_defensive() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        request_errors(&tmp.path().join("absent.jsonl")),
        (0, String::new())
    );
    let p = tmp.path().join("r.jsonl");
    std::fs::write(
        &p,
        "not json\n[1]\n{\"metadata\": {}, \"metrics\": {}}\n{\"error\": {\"code\": 401, \"type\": \"Unauthorized\", \"message\": \"Bearer sk-abcdefghijklmnopqrstuv0123456789 rejected\"}}\n{\"error\": \"str\"}\n",
    )
    .unwrap();
    let (n, first) = request_errors(&p);
    assert!(
        n == 1
            && first.starts_with("HTTP 401 Unauthorized:")
            && !first.contains("sk-abcdefghijklmnop"),
        "{first}"
    );
    let link = tmp.path().join("link.jsonl");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert_eq!(request_errors(&link), (0, String::new()));
}

#[tokio::test]
async fn test_timeout_is_a_failed_run() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    run.on_prefix(&[LAUNCHER, "profile"], fixed(timed_out()));
    let (a, _) = env.make(Some(run), &Opts::default());
    let (_, meta) = run_to_end(&a, None).await;
    assert!(meta["verdict"] == "failed" && meta["error"].as_str().unwrap().contains("timed out"));
}

#[tokio::test]
async fn test_exit_zero_without_a_summary_is_an_error() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    run.on_prefix(&[LAUNCHER, "profile"], fixed(ok("done\n")));
    let (a, _) = env.make(Some(run), &Opts::default());
    let (_, meta) = run_to_end(&a, None).await;
    assert!(
        meta["verdict"] == "error"
            && meta["error"]
                .as_str()
                .unwrap()
                .contains("wrote no readable profile_export_aiperf.json")
    );
}

#[tokio::test]
async fn test_summary_flags_decide_the_verdict() {
    for (patch, verdict) in [
        (json!({"was_cancelled": true}), "interrupted"),
        (json!({"is_complete": false}), "failed"),
        (
            json!({"request_count": {"unit": "requests", "avg": 0.0}}),
            "error",
        ),
    ] {
        let env = ApEnv::new().await;
        let mut doc: Value =
            serde_json::from_str(&fx("aiperf", "profile_export_aiperf.json")).unwrap();
        for (k, v) in patch.as_object().unwrap() {
            doc[k] = v.clone();
        }
        let run = standard_runner();
        run.on_prefix(
            &[LAUNCHER, "profile"],
            writes_profile(Some(doc.to_string())),
        );
        let (a, _) = env.make(Some(run), &Opts::default());
        let (_, meta) = run_to_end(&a, None).await;
        assert_eq!(meta["verdict"], verdict, "{patch}");
    }
}

#[tokio::test]
async fn test_a_garbage_summary_is_an_error_not_a_crash() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    run.on_prefix(
        &[LAUNCHER, "profile"],
        writes_profile(Some("{not json".into())),
    );
    let (a, _) = env.make(Some(run), &Opts::default());
    let (_, meta) = run_to_end(&a, None).await;
    assert_eq!(meta["verdict"], "error");
}

#[tokio::test]
async fn test_secrets_in_runner_output_never_reach_run_json_or_the_report() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    run.on_prefix(
        &[LAUNCHER, "profile"],
        fixed(rc(
            1,
            "",
            "Error: 401 Authorization: Bearer sk-abcdefghijklmnopqrstuv0123456789",
        )),
    );
    let (a, _) = env.make(Some(run), &Opts::default());
    let (rid, _) = run_to_end(&a, None).await;
    let raw = std::fs::read_to_string(a.runs_dir().join(&rid).join("run.json")).unwrap();
    assert!(
        !raw.contains("sk-abcdefghijklmnop")
            && !a.report_text(&rid).1.contains("sk-abcdefghijklmnop")
    );
}

#[tokio::test]
async fn test_console_stop_marks_the_run_interrupted() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    let started = Arc::new(tokio::sync::Notify::new());
    let s2 = Arc::clone(&started);
    run.on_prefix(
        &[LAUNCHER, "profile"],
        Arc::new(move |_| {
            let s = Arc::clone(&s2);
            Box::pin(async move {
                s.notify_one();
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(ok(""))
            })
        }),
    );
    let (a, _) = env.make(Some(run), &Opts::default());
    let res = a.run_action("profile", &smoke()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    assert!(res.ok);
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert!(a.has_live());
    a.abort_live();
    assert!(!a.join_live().await); // cancelled
    let meta = run_json(&a, &rid);
    assert!(meta["verdict"] == "interrupted" && meta["interrupted_by"] == "console stop");
    assert!(a.live_run_id().is_none());
}

// ── preflight refusals (nothing is launched) ──────────────────────────────────────────────────
#[tokio::test]
async fn test_preflight_refusals_carry_a_fix() {
    for (opts, code) in [
        (
            Opts {
                key_file: false,
                ..Opts::default()
            },
            "ap.no_key",
        ),
        (
            Opts {
                tokenizer: false,
                ..Opts::default()
            },
            "ap.no_tokenizer",
        ),
    ] {
        let env = ApEnv::new().await;
        let (a, run) = env.make(None, &opts);
        let res = a.run_action("profile", &smoke()).await.unwrap();
        assert!(
            !res.ok
                && codes(&res) == HashSet::from([code.to_owned()])
                && res.problems[0].fix.is_some()
        );
        assert!(profile_calls(&run).is_empty() && !a.has_live());
        assert!(
            !a.runs_dir().exists() || std::fs::read_dir(a.runs_dir()).unwrap().next().is_none()
        );
    }
}

#[tokio::test]
async fn test_preflight_router_down_and_launcher_absent() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(
        None,
        &Opts {
            router_down: true,
            ..Opts::default()
        },
    );
    let res = a.run_action("profile", &smoke()).await.unwrap();
    assert!(!res.ok && codes(&res) == HashSet::from(["ap.router_down".to_owned()]));
    let (b, _) = env.make(Some(FakeRunner::new()), &Opts::default());
    let res2 = b.run_action("profile", &smoke()).await.unwrap();
    assert!(!res2.ok && codes(&res2) == HashSet::from(["ap.not_installed".to_owned()]));
}

#[tokio::test]
async fn test_unknown_presets_are_refused() {
    for preset in [
        "",
        "nope",
        "../etc",
        "SMOKE-LIGHTNING",
        "smoke-lightning; rm -rf /",
    ] {
        let env = ApEnv::new().await;
        let (a, run) = env.make(None, &Opts::default());
        let res = a
            .run_action("profile", json!({"preset": preset}).as_object().unwrap())
            .await
            .unwrap();
        assert!(
            !res.ok && res.output.contains("unknown or invalid preset"),
            "{preset}"
        );
        assert!(profile_calls(&run).is_empty());
    }
}

#[tokio::test]
async fn test_a_preset_with_a_validation_error_cannot_run() {
    let env = ApEnv::new().await;
    let (a, run) = env.make(None, &Opts::default());
    std::fs::write(
        env.aiperf_dir().join("profiles.yaml"),
        fx("aiperf", "profiles.yaml").replace("concurrency: 2", "concurrency: 999"),
    )
    .unwrap();
    let res = a.run_action("profile", &smoke()).await.unwrap();
    assert!(
        !res.ok
            && res.output.contains("unknown or invalid preset")
            && res.output.contains("'concurrency' must be an integer")
    );
    assert!(profile_calls(&run).is_empty());
}

#[tokio::test]
async fn test_unknown_action_is_a_key_error() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    assert!(matches!(
        a.run_action("nope", &Map::new()).await,
        Err(AdapterError::UnknownId(_))
    ));
}

// ── report ─────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_report_of_a_successful_run() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let (rid, _) = run_to_end(&a, None).await;
    let res = a
        .run_action("report", json!({"run_id": rid}).as_object().unwrap())
        .await
        .unwrap();
    assert!(res.ok);
    let out = &res.output;
    assert!(
        out.contains("smoke-lightning")
            && out.contains("concurrency 2")
            && out.contains("8 requests (+1 warmup)")
    );
    assert!(
        out.contains("| Time to first output (ms) | 30.0 | 40.7 |")
            && out.contains("| Inter-token latency (ms) | 0.79 | 0.97 |"),
        "{out}"
    );
    assert!(
        out.contains("Output throughput: 1533.0 tokens/s")
            && out.contains("error rate: 0.0 %")
            && out.contains("router hop")
    );
    assert_eq!(
        &a.run_action("report", &Map::new()).await.unwrap().output,
        out
    ); // empty run id = the newest
}

#[tokio::test]
async fn test_report_edge_cases() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    assert!(!a.run_action("report", &Map::new()).await.unwrap().ok);
    let r = |id: &str| json!({"run_id": id}).as_object().unwrap().clone();
    assert_eq!(
        a.run_action("report", &r("../../etc/passwd"))
            .await
            .unwrap()
            .output,
        "unknown run id"
    );
    assert_eq!(
        a.run_action("report", &r("20261024T100000-001-nope"))
            .await
            .unwrap()
            .output,
        "unknown run id"
    );
}

// ── housekeeping ───────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_only_the_newest_hundred_runs_are_kept() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    for i in 0..(KEEP_RUNS + 5) {
        let d = a
            .runs_dir()
            .join(format!("20260101T0000{:02}-{:03}-old", i / 100, i % 100));
        std::fs::create_dir_all(&d).unwrap();
        #[allow(clippy::cast_precision_loss)]
        let meta = json!({"run_id": d.file_name().unwrap().to_str().unwrap(), "verdict": "succeeded", "ts": 1.0 + i as f64});
        std::fs::write(d.join("run.json"), meta.to_string()).unwrap();
    }
    run_to_end(&a, None).await;
    let mut names: Vec<String> = std::fs::read_dir(a.runs_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert!(names.len() == KEEP_RUNS && names.last().unwrap().ends_with("smoke-lightning"));
    assert!(!a.runs_dir().join("20260101T000000-000-old").exists());
}

#[tokio::test]
async fn test_stray_entries_in_the_runs_dir_are_ignored() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let rd = a.runs_dir();
    std::fs::create_dir_all(rd.join("not-a-run")).unwrap();
    std::fs::create_dir_all(rd.join("20261024T100000-001-x")).unwrap(); // a run dir without run.json
    std::fs::create_dir_all(rd.join("20261024T100000-002-y")).unwrap();
    std::fs::write(rd.join("20261024T100000-002-y").join("run.json"), "[1, 2]").unwrap();
    assert!(
        a.recent_events(100).await.unwrap().is_empty()
            && a.metrics().await.unwrap().points[0].value == 0.0
    );
}

// ── the core: confirm required, params validated, admin only ──────────────────────────────────
fn core_state(
    cenv: &common::Env,
    a: &Arc<AiPerfAdapter>,
    run: &Arc<FakeRunner>,
) -> Arc<nemo_console::container::AppState> {
    let mut adapters = support_b::fakes(cenv);
    adapters.insert(SystemId::Aiperf, Arc::clone(a) as Arc<dyn SystemAdapter>);
    support_b::state_with(
        cenv,
        &cenv.settings,
        adapters,
        Arc::clone(run) as _,
        &cenv.environ,
    )
}

#[tokio::test]
async fn test_core_requires_confirm_and_validates_the_preset() {
    let env = ApEnv::new().await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let run = standard_runner();
    run.on_prefix(&[LAUNCHER, "profile"], gated(Arc::clone(&gate)));
    let (a, _) = env.make(Some(Arc::clone(&run)), &Opts::default());
    let cenv = common::Env::new(None);
    let state = core_state(&cenv, &a, &run);
    let req = |m: &str, p: &str| common::TestReq::new(Arc::clone(&state), m, p);
    let url = "/api/v1/systems/aiperf/actions/profile";
    let specs = req("GET", "/api/v1/systems/aiperf/actions")
        .send()
        .await
        .json();
    let spec = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "profile")
        .unwrap()
        .clone();
    assert!(spec["danger"] == "disruptive" && spec["role"] == "admin");
    assert_eq!(
        spec["params"][0]["choices"],
        json!([
            "latency-lightning",
            "smoke-lightning",
            "throughput-lightning"
        ])
    );
    let r = req("POST", url)
        .json(json!({"params": smoke()}))
        .send()
        .await;
    assert!(r.status == 400 && r.json()["error"]["code"] == "confirm_required");
    for bad in [
        json!({"preset": "nope"}),
        json!({"preset": 3}),
        json!({"unknown": 1}),
    ] {
        let mut p = smoke();
        for (k, v) in bad.as_object().unwrap() {
            p.insert(k.clone(), v.clone());
        }
        let r = req("POST", url)
            .json(json!({"params": p, "confirm": true}))
            .send()
            .await;
        assert!(
            r.status == 422 && r.json()["error"]["code"] == "invalid_param",
            "{bad}: {}",
            r.text()
        );
    }
    assert!(profile_calls(&run).is_empty());
    let r = req("POST", url)
        .json(json!({"params": smoke(), "confirm": true}))
        .send()
        .await;
    assert!(r.status == 200 && r.json()["ok"] == true, "{}", r.text());
    let r2 = req("POST", "/api/v1/systems/aiperf/actions/report")
        .json(json!({"params": {}}))
        .send()
        .await;
    assert_eq!(r2.status, 200); // safe: no confirm needed
    let st = req("GET", "/api/v1/systems/aiperf/status")
        .send()
        .await
        .json();
    assert!(st["system"] == "aiperf" && st["health"] == "up", "{st}");
    gate.add_permits(10);
    finish(&a).await;
}

#[tokio::test]
async fn test_core_config_roundtrip_uses_our_validator() {
    let env = ApEnv::new().await;
    let run = standard_runner();
    let (a, _) = env.make(Some(Arc::clone(&run)), &Opts::default());
    let cenv = common::Env::new(None);
    let state = core_state(&cenv, &a, &run);
    let req = |m: &str, p: &str| common::TestReq::new(Arc::clone(&state), m, p);
    let files = req("GET", "/api/v1/systems/aiperf/config")
        .send()
        .await
        .json();
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].clone())
            .collect::<Vec<_>>()
            == [json!("profiles")]
            && files[0]["editable"] == true
    );
    let cf = req("GET", "/api/v1/systems/aiperf/config/profiles")
        .send()
        .await
        .json();
    assert_eq!(cf["content"], fx("aiperf", "profiles.yaml"));
    let content = cf["content"].as_str().unwrap();
    let bad = content.replace("requests: 8", "requests: 0");
    let v = req("POST", "/api/v1/systems/aiperf/config/profiles/validate")
        .json(json!({"content": bad}))
        .send()
        .await
        .json();
    assert!(v["ok"] == false && v["authoritative"] == true);
    assert!(
        v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("'requests' must be an integer")
    );
    let r = req("POST", "/api/v1/systems/aiperf/config/profiles/apply")
        .json(
            json!({"content": bad, "base_sha256": cf["sha256"], "restart": false, "confirm": true}),
        )
        .send()
        .await;
    assert!(r.status >= 400 || r.json()["ok"] == false); // an invalid file is never written
    let prof = env.aiperf_dir().join("profiles.yaml");
    assert_eq!(
        std::fs::read_to_string(&prof).unwrap(),
        fx("aiperf", "profiles.yaml")
    );
    let good = content.replace("requests: 8", "requests: 6");
    let r = req("POST", "/api/v1/systems/aiperf/config/profiles/apply")
        .json(json!({"content": good, "base_sha256": cf["sha256"], "restart": false, "confirm": true}))
        .send()
        .await;
    assert!(
        r.status == 200 && r.json()["ok"] == true && r.json()["post_check"] == "healthy",
        "{}",
        r.text()
    );
    assert_eq!(std::fs::read_to_string(&prof).unwrap(), good);
    assert!(a.run_action("profile", &smoke()).await.unwrap().ok); // the next run uses the new file
    finish(&a).await;
    let last = run.argvs().last().unwrap().clone();
    let i = last.iter().position(|x| x == "--request-count").unwrap();
    assert_eq!(last[i + 1], "6");
}

#[tokio::test]
async fn test_run_json_redacts_errors_even_if_a_caller_forgot() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("rd");
    std::fs::create_dir(&d).unwrap();
    let meta =
        json!({"run_id": "x", "error": "failed with Bearer sk-abcdefghijklmnopqrstuv0123456789"});
    write_meta(&d, meta.as_object().unwrap()).unwrap();
    assert!(
        !std::fs::read_to_string(d.join("run.json"))
            .unwrap()
            .contains("sk-abcdefghijklmnop")
    );
}
