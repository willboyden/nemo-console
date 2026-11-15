//! Addendum 9.15 (charts, wave 3): the per-run routes `GET /systems/{id}/runs/{run_id}/summary` (AIPerf,
//! SkillEvaluator), `.../requests` (AIPerf) and `.../profile` (Data Designer), Rust-only (PORTING.md D-40), and the
//! wider `evaluator.run` / `gym.smoke` event data (D-41). Hermetic: SYNTHETIC run directories written into a temp
//! state dir (no real host, id or captured text; the one exception reads the AIPerf summary fixture already in the
//! repo to prove the real shape), the real adapters for these five systems, fakes for the rest.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::aiperf::AiPerfAdapter;
use nemo_console::adapters::base::{AdapterContext, CommandRunner, SystemAdapter};
use nemo_console::adapters::datadesigner::DataDesignerAdapter;
use nemo_console::adapters::evaluator::EvaluatorAdapter;
use nemo_console::adapters::gym::GymAdapter;
use nemo_console::adapters::runviz;
use nemo_console::adapters::skillevaluator::SkillEvaluatorAdapter;
use nemo_console::config::settings_for_test;
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::{Role, SystemId};
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Value, json};

/// Planted in every text field the routes must never copy (finding messages, criterion text, notes, conclusions,
/// recommendations, error texts, request / conversation / worker ids, record values for a viewer).
const MARK: &str = "ZZMARKERZZ";

const AP_RUN: &str = "20261101T120000-000-smoke";
const SE_RUN: &str = "20261101T120000-000-demo-skill";
const SE_RUN3: &str = "20261101T130000-000-demo-skill";
const DD_RUN: &str = "20261101T120000-000-demo.py";

struct Lab {
    _tmp: tempfile::TempDir,
    state: PathBuf,
    st: Arc<AppState>,
    viewer: String,
    admin: String,
    adapters: HashMap<SystemId, Arc<dyn SystemAdapter>>,
}

fn ctx(state: &Path, runner: Arc<common::FakeRunner>) -> AdapterContext {
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls: IndexMap::new(),
        paths: IndexMap::new(),
        secret_env: IndexMap::new(),
        state_dir: state.to_path_buf(),
        environ: Environ::from_pairs(&[]),
    }
}

fn lab() -> Lab {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("adapter-state");
    std::fs::create_dir_all(&state).unwrap();
    let runner = Arc::new(common::FakeRunner::default());
    let c = ctx(&state, Arc::clone(&runner));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = HashMap::new();
    adapters.insert(SystemId::Aiperf, Arc::new(AiPerfAdapter::new(c.clone())));
    adapters.insert(
        SystemId::Skillevaluator,
        Arc::new(SkillEvaluatorAdapter::new(c.clone())),
    );
    adapters.insert(
        SystemId::Datadesigner,
        Arc::new(DataDesignerAdapter::new(c.clone())),
    );
    adapters.insert(
        SystemId::Evaluator,
        Arc::new(EvaluatorAdapter::new(c.clone())),
    );
    adapters.insert(SystemId::Gym, Arc::new(GymAdapter::new(c)));
    for sid in SystemId::ALL {
        adapters
            .entry(sid)
            .or_insert_with(|| common::FakeAdapter::new(sid, None) as Arc<dyn SystemAdapter>);
    }
    let settings = settings_for_test(json!({
        "state_dir": tmp.path().join("app-state").to_string_lossy(),
        "frontend_dir": tmp.path().join("front").to_string_lossy(),
        "repo_root": tmp.path().join("repo").to_string_lossy(),
        "background_tasks": false, "trust_loopback": true,
    }));
    let st = build_container(
        settings,
        Overrides {
            adapters: adapters.clone(),
            runner: Some(runner as Arc<dyn CommandRunner>),
            http: Some(Http::new().unwrap()),
            registry: None,
            environ: Some(Environ::from_pairs(&[])),
        },
    )
    .unwrap();
    let (_, viewer) = st.auth.mint(Role::Viewer, "").unwrap();
    let (_, admin) = st.auth.mint(Role::Admin, "").unwrap();
    Lab {
        _tmp: tmp,
        state,
        st,
        viewer,
        admin,
        adapters,
    }
}

impl Lab {
    fn run_dir(&self, system: &str, run_id: &str) -> PathBuf {
        let d = self.state.join(system).join("runs").join(run_id);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn get(&self, path: &str, key: Option<&str>) -> common::TestResp {
        let mut r = common::TestReq::new(Arc::clone(&self.st), "GET", path).client("192.0.2.10");
        if let Some(k) = key {
            r = r.token(k);
        }
        r.send().await
    }

    async fn viewer_json(&self, path: &str) -> Value {
        let r = self.get(path, Some(&self.viewer)).await;
        assert_eq!(r.status, 200, "{path}: {}", r.text());
        assert!(!r.text().contains(MARK), "{path} leaked: {}", r.text());
        r.json()
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn stats(base: f64) -> Value {
    json!({"unit": "ms", "avg": base, "min": base - 1.0, "max": base + 1.0, "std": 0.5, "p1": base - 0.9,
        "p5": base - 0.8, "p10": base - 0.7, "p25": base - 0.3, "p50": base, "p75": base + 0.3, "p90": base + 0.7,
        "p95": base + 0.8, "p99": base + 0.9, "count": 4, "sum": base * 4.0})
}

/// A synthetic AIPerf run: run.json, the summary JSON and four per-request records (two workers, one failure with a
/// string code, one without a code).
fn seed_aiperf(lab: &Lab) -> PathBuf {
    let d = lab.run_dir("aiperf", AP_RUN);
    write(
        &d.join("run.json"),
        &json!({"run_id": AP_RUN, "preset": "smoke", "model": "lab-model", "verdict": "succeeded",
            "concurrency": 2, "requests": 4, "isl": 100, "osl": 50, "ts": 1.0, "error": MARK,
            "first_error": MARK, "metrics": {"duration_s": 9.5}})
        .to_string(),
    );
    let profile = json!({
        "schema_version": "1.4",
        "benchmark_id": MARK,
        "request_latency": stats(80.0),
        "time_to_first_token": stats(30.0),
        "inter_token_latency": stats(1.0),
        "request_throughput": {"unit": "requests/sec", "avg": 12.5},
        "output_token_throughput_per_user": {"unit": "tokens/sec/user", "avg": 100.0, "p50": 99.0},
        "benchmark_duration": {"unit": "sec", "avg": 0.5},
        "http_req_dns_lookup": {"unit": "ms", "avg": 0.0},
        "http_req_connecting": {"unit": "ms", "avg": 0.25},
        "http_req_sending": {"unit": "ms", "avg": 0.5},
        "http_req_waiting": {"unit": "ms", "avg": 9.0},
        "http_req_receiving": {"unit": "ms", "avg": 70.0},
        "odd_unit_metric": {"unit": MARK.repeat(10), "avg": 3.0},
        "no_numbers": {"unit": "ms", "avg": "fast"},
        "Bad Name": {"unit": "ms", "avg": 1.0},
        "input_config": {"endpoint": {"urls": [MARK]}},
        "run_info": {"cli_command": MARK},
        "error_summary": [{"message": MARK}],
        "start_time": MARK,
    });
    write(
        &d.join("artifacts").join("profile_export_aiperf.json"),
        &profile.to_string(),
    );
    let rec = |i: i64, worker: &str, start: i64, error: Option<Value>| {
        let mut r = json!({
            "metadata": {"session_num": i, "x_request_id": format!("{MARK}-req-{i}"),
                "x_correlation_id": format!("{MARK}-corr"), "conversation_id": format!("{MARK}-conv"),
                "request_start_ns": start, "request_end_ns": start + 80_000_000, "worker_id": worker,
                "record_processor_id": MARK},
            "metrics": {"request_latency": {"value": 80.0, "unit": "ms"},
                "time_to_first_token": {"value": 30_000_000.0, "unit": "ns"},
                "inter_token_latency": {"value": 0.001, "unit": "s"},
                "input_sequence_length": {"value": 100, "unit": "tokens"},
                "output_sequence_length": {"value": 50, "unit": "tokens"}},
        });
        if let Some(e) = error {
            r["error"] = e;
        }
        r.to_string()
    };
    let w1 = format!("{MARK}-worker-a");
    let w2 = format!("{MARK}-worker-b");
    let lines = [
        rec(0, &w1, 1_000_000_000_000_000_000, None),
        rec(1, &w2, 1_000_000_000_500_000_000, None),
        rec(
            2,
            &w1,
            1_000_000_001_000_000_000,
            Some(json!({"code": "429", "type": MARK, "message": MARK})),
        ),
        rec(
            3,
            &w2,
            1_000_000_002_000_000_000,
            Some(
                json!({"type": MARK, "message": format!("{{\"error\": {{\"message\": \"{MARK}\"}}}}")}),
            ),
        ),
        rec(
            4,
            &w1,
            1_000_000_003_000_000_000,
            Some(json!({"code": 400, "message": MARK})),
        ),
    ];
    write(
        &d.join("artifacts").join("profile_export.jsonl"),
        &(lines.join("\n") + "\n"),
    );
    d
}

// ── AIPerf summary ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn aiperf_summary_numbers_units_and_phases() {
    let lab = lab();
    seed_aiperf(&lab);
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"))
        .await;
    assert_eq!(doc["run_id"], AP_RUN);
    assert_eq!(doc["preset"], "smoke");
    assert_eq!(doc["model"], "lab-model");
    assert_eq!(doc["verdict"], "succeeded");
    assert_eq!(
        (
            &doc["concurrency"],
            &doc["requests"],
            &doc["isl"],
            &doc["osl"]
        ),
        (&json!(2), &json!(4), &json!(100), &json!(50))
    );
    // the profile's benchmark_duration (sec) wins over run.json's copy
    assert_eq!(doc["duration_s"], 0.5);
    let m = doc["metrics"].as_object().unwrap();
    // every object with a finite statistic and a metric-shaped name; nothing else
    let mut names: Vec<&str> = m.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "benchmark_duration",
            "http_req_connecting",
            "http_req_dns_lookup",
            "http_req_receiving",
            "http_req_sending",
            "http_req_waiting",
            "inter_token_latency",
            "odd_unit_metric",
            "output_token_throughput_per_user",
            "request_latency",
            "request_throughput",
            "time_to_first_token",
        ]
    );
    let lat = &m["request_latency"];
    assert_eq!(lat["unit"], "ms");
    for s in runviz::AIPERF_STATS {
        assert!(lat[s].is_number(), "{s}");
    }
    assert!(lat.get("count").is_none() && lat.get("sum").is_none());
    // absent statistics are null; a unit that is not unit-shaped is null
    assert_eq!(m["request_throughput"]["p99"], Value::Null);
    assert_eq!(m["request_throughput"]["avg"], 12.5);
    assert_eq!(
        m["output_token_throughput_per_user"]["unit"],
        "tokens/sec/user"
    );
    assert_eq!(m["odd_unit_metric"]["unit"], Value::Null);
    assert_eq!(
        doc["http_phases_ms"],
        json!({"dns": 0.0, "connecting": 0.25, "sending": 0.5, "waiting": 9.0, "receiving": 70.0})
    );
}

#[tokio::test]
async fn aiperf_summary_reads_the_real_summary_shape() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/aiperf/profile_export_aiperf.json");
    std::fs::copy(&fixture, d.join("artifacts/profile_export_aiperf.json")).unwrap();
    let r = lab
        .get(
            &format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"),
            Some(&lab.viewer),
        )
        .await;
    assert_eq!(r.status, 200);
    let doc = r.json();
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&fixture).unwrap()).unwrap();
    let expected = raw
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, v)| {
            v.as_object().is_some_and(|o| {
                runviz::AIPERF_STATS
                    .iter()
                    .any(|s| o.get(*s).is_some_and(Value::is_number))
            })
        })
        .count();
    assert_eq!(doc["metrics"].as_object().unwrap().len(), expected);
    assert!(doc["metrics"].get("warmup_metrics").is_none());
    assert_eq!(doc["metrics"]["time_to_first_token"]["unit"], "ms");
    assert!(doc["http_phases_ms"]["waiting"].is_number());
    assert!(doc["duration_s"].as_f64().unwrap() > 0.0);
    // no URL, command line, path or id from input_config / run_info reaches the answer
    let text = r.text();
    for s in [
        "127.0.0.1",
        "cli_command",
        "/LAB",
        "benchmark_id",
        "redacted",
    ] {
        assert!(!text.contains(s), "{s}");
    }
}

#[tokio::test]
async fn aiperf_summary_without_a_profile_and_with_odd_meta() {
    let lab = lab();
    let d = lab.run_dir("aiperf", AP_RUN);
    write(
        &d.join("run.json"),
        &json!({"run_id": AP_RUN, "preset": "Bad Preset", "model": format!("x;{MARK}"),
            "verdict": MARK, "concurrency": "2", "requests": true})
        .to_string(),
    );
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"))
        .await;
    assert_eq!(doc["metrics"], json!({}));
    assert_eq!(doc["http_phases_ms"], Value::Null);
    for k in [
        "preset",
        "model",
        "verdict",
        "concurrency",
        "requests",
        "isl",
        "osl",
        "duration_s",
    ] {
        assert_eq!(doc[k], Value::Null, "{k}");
    }
    // a run left at running by a process that no longer runs it reads as interrupted
    write(
        &d.join("run.json"),
        &json!({"run_id": AP_RUN, "verdict": "running"}).to_string(),
    );
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"))
        .await;
    assert_eq!(doc["verdict"], "interrupted");
}

#[tokio::test]
async fn aiperf_summary_caps_the_metric_count() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    let mut doc = serde_json::Map::new();
    for i in 0..300 {
        doc.insert(format!("metric_{i:03}"), json!({"unit": "ms", "avg": i}));
    }
    write(
        &d.join("artifacts/profile_export_aiperf.json"),
        &Value::Object(doc).to_string(),
    );
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"))
        .await;
    let m = doc["metrics"].as_object().unwrap();
    assert_eq!(m.len(), runviz::AIPERF_MAX_METRICS);
    assert!(m.contains_key("metric_000") && !m.contains_key("metric_100"));
}

// ── AIPerf requests ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn aiperf_requests_numbers_only() {
    let lab = lab();
    seed_aiperf(&lab);
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"))
        .await;
    assert_eq!(doc["run_id"], AP_RUN);
    assert_eq!(doc["total"], 5);
    assert_eq!(doc["returned"], 5);
    assert_eq!(doc["truncated"], false);
    let rows = doc["rows"].as_array().unwrap();
    let r0 = &rows[0];
    assert_eq!(
        r0,
        &json!({"i": 0, "start_ms": 0.0, "end_ms": 80.0, "latency_ms": 80.0, "ttft_ms": 30.0, "itl_avg_ms": 1.0,
            "input_tokens": 100.0, "output_tokens": 50.0, "worker": 0, "error_code": null})
    );
    assert_eq!(rows[1]["start_ms"], 500.0);
    assert_eq!(rows[1]["worker"], 1);
    assert_eq!(rows[2]["worker"], 0);
    // a string code, no code (failed: 0), an integer code
    assert_eq!(rows[2]["error_code"], 429);
    assert_eq!(rows[3]["error_code"], 0);
    assert_eq!(rows[4]["error_code"], 400);
    assert_eq!(rows[4]["start_ms"], 3000.0);
    // only the documented keys
    for r in rows {
        let mut keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "end_ms",
                "error_code",
                "i",
                "input_tokens",
                "itl_avg_ms",
                "latency_ms",
                "output_tokens",
                "start_ms",
                "ttft_ms",
                "worker"
            ]
        );
    }
}

#[tokio::test]
async fn aiperf_requests_samples_a_large_file_to_5000_rows() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    let mut text = String::new();
    for i in 0..20_000i64 {
        text.push_str(
            &json!({"metadata": {"request_start_ns": 5_000_000_000 + i * 1_000_000,
                "request_end_ns": 5_000_000_000 + i * 1_000_000 + 10_000_000,
                "worker_id": format!("w{}", i % 7)},
                "metrics": {"request_latency": {"value": i, "unit": "ms"}}})
            .to_string(),
        );
        text.push('\n');
    }
    write(&d.join("artifacts/profile_export.jsonl"), &text);
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"))
        .await;
    assert_eq!(doc["total"], 20_000);
    assert_eq!(doc["returned"], 5000);
    assert_eq!(doc["truncated"], true);
    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows.len(), runviz::REQUESTS_MAX_ROWS);
    assert_eq!(rows[0]["i"], 0);
    assert_eq!(rows[4999]["i"], 19_999);
    let idx: Vec<u64> = rows.iter().map(|r| r["i"].as_u64().unwrap()).collect();
    assert!(idx.windows(2).all(|w| w[0] < w[1] && w[1] - w[0] <= 5));
    // the sampled row carries its own numbers; workers are numbered by first appearance over the whole file
    assert_eq!(rows[4999]["latency_ms"], 19_999.0);
    assert_eq!(rows[4999]["start_ms"], 19_999.0);
    assert_eq!(rows[0]["worker"], 0);
    let max_worker = rows.iter().filter_map(|r| r["worker"].as_u64()).max();
    assert_eq!(max_worker, Some(6));
}

#[tokio::test]
async fn aiperf_requests_hostile_lines() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    let huge = MARK.repeat(10_000); // about 100 KiB, twice per line: under the 256 KiB line cap
    let lines = [
        // a huge worker id gets no index; huge error text is never read
        json!({"metadata": {"request_start_ns": 10, "worker_id": huge}, "error": {"code": 500, "message": huge}})
            .to_string(),
        // non-finite and wrongly typed numbers become null
        r#"{"metadata": {"request_start_ns": 20, "request_end_ns": "x"}, "metrics": {"request_latency": {"value": NaN, "unit": "ms"}, "time_to_first_token": {"value": Infinity, "unit": "ms"}, "inter_token_latency": {"value": [1, 2], "unit": "ms"}, "input_sequence_length": {"value": 1e999, "unit": "tokens"}}}"#.to_owned(),
        // an unknown unit is not guessed
        json!({"metadata": {"request_start_ns": 30}, "metrics": {"request_latency": {"value": 5, "unit": "fortnights"}}})
            .to_string(),
        "not json".to_owned(),
        "[1, 2, 3]".to_owned(),
        json!({"error": {"code": 99_999_999}}).to_string(),
        json!({"error": {"code": "4000000"}}).to_string(),
        json!({"error": {}}).to_string(),
        // a line over the line cap is not parsed at all: counted as skipped, never a request
        json!({"metadata": {"request_start_ns": 1, "worker_id": "w"}, "pad": MARK.repeat(30_000)}).to_string(),
        // so is a short line with more values than a parse may build
        format!("{{\"metrics\": [{}0]}}", "0,".repeat(runviz::JSON_MAX_NODES)),
    ];
    write(
        &d.join("artifacts/profile_export.jsonl"),
        &(lines.join("\n") + "\n"),
    );
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"))
        .await;
    // the two lines that are not JSON objects are not requests; the two over a cap are skipped
    assert_eq!(doc["total"], 6);
    assert_eq!(doc["skipped"], 2);
    assert_eq!(doc["truncated"], true);
    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows[0]["worker"], Value::Null);
    assert_eq!(rows[0]["error_code"], 500);
    assert_eq!(rows[1]["start_ms"], 0.00001);
    for k in [
        "end_ms",
        "latency_ms",
        "ttft_ms",
        "itl_avg_ms",
        "input_tokens",
        "error_code",
    ] {
        assert_eq!(rows[1][k], Value::Null, "{k}");
    }
    assert_eq!(rows[2]["latency_ms"], Value::Null);
    // codes out of range or too long count as "failed without a usable code"
    assert_eq!(rows[3]["error_code"], 0);
    assert_eq!(rows[4]["error_code"], 0);
    // an empty error object is no failure; a line without metadata has no start
    assert_eq!(rows[5]["error_code"], Value::Null);
    assert_eq!(rows[5]["start_ms"], Value::Null);
    assert!(doc.to_string().len() < 10_000);
}

#[tokio::test]
async fn aiperf_requests_file_over_the_cap_is_cut_at_a_line() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    let line =
        json!({"metadata": {"request_start_ns": 1}, "pad": "x".repeat(1000)}).to_string() + "\n";
    let n = (17 * 1024 * 1024) / line.len();
    write(&d.join("artifacts/profile_export.jsonl"), &line.repeat(n));
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"))
        .await;
    let total = doc["total"].as_u64().unwrap();
    let max = (16 * 1024 * 1024) / line.len() as u64;
    assert_eq!(total, max, "only whole lines inside the 16 MiB cap");
    assert_eq!(doc["truncated"], true);
    assert_eq!(doc["returned"], 5000);
}

#[tokio::test]
async fn aiperf_requests_without_a_records_file_is_empty() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    std::fs::remove_file(d.join("artifacts/profile_export.jsonl")).unwrap();
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"))
        .await;
    assert_eq!(
        doc,
        json!({"run_id": AP_RUN, "total": 0, "returned": 0, "skipped": 0, "truncated": false, "rows": []})
    );
}

// ── SkillEvaluator summary ──────────────────────────────────────────────────────────────────────

fn seed_skilleval(lab: &Lab) {
    let d = lab.run_dir("skillevaluator", SE_RUN);
    write(
        &d.join("run.json"),
        &json!({"run_id": SE_RUN, "skill": "demo-skill", "tiers": "tier1", "verdict": "failed",
            "error": MARK, "failed_checks": [MARK], "tier1": {"status": "failed"}})
        .to_string(),
    );
    let finding = |sev: Option<&str>| {
        let mut f = json!({"category": "SCHEMA", "check": MARK, "message": MARK, "title": MARK,
            "suggestion": MARK, "file_path": MARK});
        if let Some(s) = sev {
            f["severity"] = json!(s);
        }
        f
    };
    let tier1 = json!({
        "overall_status": "failed",
        "results": [
            {"validator": "Schema & Repository Governance", "passed": false, "status": "failed",
             "findings": [finding(Some("medium")), finding(Some("HIGH")), finding(None), finding(Some("weird")),
                          finding(Some("critical")), finding(Some("low"))]},
            {"validator": "QUALITY", "passed": true, "status": "passed", "findings": []},
            {"validator": format!("<script>{MARK}"), "passed": true, "status": "passed", "findings": []},
            {"validator": "RUBRIC_EVAL", "passed": true, "status": MARK, "findings": [finding(Some("info"))]},
        ],
        "quality_summary": [{"overall_score": 83.0, "grade": "B", "skill_name": "demo-skill",
            "dimensions": {"correctness": {"score": 30.5, "weight": 0.35, "issues": [{"message": MARK}]},
                "discoverability": {"score": 20.0, "issues": [{"suggestion": MARK}]},
                "Bad Dim": {"score": 1.0}, "efficiency": {"score": "high"}}}],
        "rubric_eval": {"execution_status": "succeeded", "overall_score": 83.6, "summary": MARK,
            "checks": [
                {"id": "description_clarity", "criterion": MARK, "importance": "high", "pass": true,
                 "judge_pass": true, "score": 8, "notes": MARK},
                {"id": "example_quality", "criterion": MARK, "importance": MARK, "pass": false,
                 "judge_pass": "yes", "score": 4.5, "notes": MARK},
                {"id": format!("{MARK} id"), "criterion": MARK, "score": 1},
            ]},
    });
    write(&d.join("skillevaluator-tier1.json"), &tier1.to_string());
    let d3 = lab.run_dir("skillevaluator", SE_RUN3);
    write(
        &d3.join("run.json"),
        &json!({"run_id": SE_RUN3, "skill": "demo-skill", "tiers": "tier3", "verdict": "succeeded",
            "conclusions": [{"title": MARK, "message": MARK}],
            "recommendations": [{"title": MARK, "message": MARK}],
            "tier3": {"execution_status": "succeeded", "errors": [MARK],
                "agents": {
                    "opencode": {"model": "lab-model", "skill_lift": 0.25, "with_skill": 0.75,
                        "without_skill": 0.5, "execution_status": "succeeded", "errors": [MARK],
                        "metrics": {"correctness": {"with_skill": 0.8, "without_skill": 0.6, "delta": 0.2},
                            "Bad Metric": {"with_skill": 1}, "speed": {"with_skill": "fast"}}},
                    "bad agent!": {"model": "m"},
                    "other": {"model": format!("bad model {MARK}")}
                }}})
        .to_string(),
    );
}

#[tokio::test]
async fn skilleval_summary_tier1_counts_and_scores_only() {
    let lab = lab();
    seed_skilleval(&lab);
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/skillevaluator/runs/{SE_RUN}/summary"
        ))
        .await;
    assert_eq!(doc["skill"], "demo-skill");
    assert_eq!(doc["tiers"], "tier1");
    assert_eq!(doc["verdict"], "failed");
    assert_eq!(doc["tier3"], Value::Null);
    let t1 = &doc["tier1"];
    let v = t1["validators"].as_array().unwrap();
    assert_eq!(v.len(), 3, "the validator with a non-name is left out");
    assert_eq!(
        v[0],
        json!({"name": "Schema & Repository Governance", "passed": false, "status": "failed",
            "findings": {"critical": 1, "high": 1, "medium": 1, "low": 1, "info": 1}})
    );
    assert_eq!(v[1]["findings"]["info"], 0);
    assert_eq!(v[2]["name"], "RUBRIC_EVAL");
    assert_eq!(v[2]["status"], Value::Null);
    assert_eq!(v[2]["findings"]["info"], 1);
    assert_eq!(
        t1["rubric"],
        json!([
            {"id": "description_clarity", "importance": "high", "score": 8, "pass": true, "judge_pass": true},
            {"id": "example_quality", "importance": null, "score": 4.5, "pass": false, "judge_pass": null},
        ])
    );
    assert_eq!(
        t1["dimensions"],
        json!({"correctness": 30.5, "discoverability": 20.0})
    );
}

#[tokio::test]
async fn skilleval_summary_tier3_numbers_without_text() {
    let lab = lab();
    seed_skilleval(&lab);
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/skillevaluator/runs/{SE_RUN3}/summary"
        ))
        .await;
    assert_eq!(doc["tiers"], "tier3");
    assert_eq!(doc["tier1"], Value::Null);
    let agents = doc["tier3"]["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    assert_eq!(
        agents[0],
        json!({"name": "opencode", "model": "lab-model", "skill_lift": 0.25, "with_skill": 0.75,
            "without_skill": 0.5, "metrics": [
                {"name": "correctness", "with_skill": 0.8, "without_skill": 0.6, "delta": 0.2},
                {"name": "speed", "with_skill": null, "without_skill": null, "delta": null}]})
    );
    assert_eq!(agents[1]["name"], "other");
    assert_eq!(agents[1]["model"], Value::Null);
    assert!(!doc.to_string().contains("conclusions"));
    assert!(!doc.to_string().contains("recommendations"));
}

#[tokio::test]
async fn skilleval_summary_with_an_oversized_or_broken_tier1_file() {
    let lab = lab();
    seed_skilleval(&lab);
    let p = lab
        .state
        .join("skillevaluator/runs")
        .join(SE_RUN)
        .join("skillevaluator-tier1.json");
    std::fs::write(&p, "{not json").unwrap();
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/skillevaluator/runs/{SE_RUN}/summary"
        ))
        .await;
    assert_eq!(doc["tier1"], Value::Null);
    // past the result cap (8 MiB) the file is not parsed at all
    let big = format!("{{\"pad\": \"{}\"}}", "x".repeat(9 * 1024 * 1024));
    std::fs::write(&p, big).unwrap();
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/skillevaluator/runs/{SE_RUN}/summary"
        ))
        .await;
    assert_eq!(doc["tier1"], Value::Null);
}

// ── Data Designer profile ───────────────────────────────────────────────────────────────────────

fn seed_dd(lab: &Lab, records: &str) {
    let d = lab.run_dir("datadesigner", DD_RUN);
    write(
        &d.join("run.json"),
        &json!({"run_id": DD_RUN, "script": "demo.py", "mode": "preview", "verdict": "succeeded", "records": 6})
            .to_string(),
    );
    write(&d.join("records.jsonl"), records);
}

fn dd_records() -> String {
    [
        json!({"category": "Books", "review": format!("{MARK} one"), "stars": 5, "ok": true, "tags": ["a"], "meta": {"k": 1}}),
        json!({"category": "Books", "review": format!("{MARK} two!"), "stars": 4, "ok": false, "tags": [], "meta": null}),
        json!({"category": "Games", "review": format!("{MARK} three"), "stars": 3.5, "ok": true}),
        json!({"category": "Books", "review": null, "stars": 1}),
        json!({"category": "Toys", "stars": "n/a"}),
        json!({"category": "Games", "review": "", "stars": 2, "What the model said": MARK}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n")
        + "\nnot json\n"
}

fn column<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

#[tokio::test]
async fn dd_profile_viewer_gets_aggregates_without_values() {
    let lab = lab();
    seed_dd(&lab, &dd_records());
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(doc["values_hidden"], true);
    assert_eq!(doc["rows"], 6);
    // the sentence-shaped column name is not an identifier: dropped, counted, and the answer says it is thinned
    assert_eq!(doc["columns_dropped"], 1);
    assert_eq!(doc["truncated"], true);
    let names: Vec<&str> = doc["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["category", "review", "stars", "ok", "tags", "meta"]);
    for c in doc["columns"].as_array().unwrap() {
        assert_eq!(c["top"], Value::Null);
    }
    let review = column(&doc, "review");
    assert_eq!(
        review["types"],
        json!({"string": 4, "number": 0, "bool": 0, "null": 2, "object": 0, "array": 0})
    );
    assert_eq!(review["non_null"], 4);
    assert_eq!(review["distinct"], 4);
    assert_eq!(review["distinct_capped"], false);
    assert_eq!(
        review["length"],
        json!({"min": 0.0, "p50": 14.0, "p95": 16.0, "max": 16.0})
    );
    assert_eq!(review["numeric"], Value::Null);
    // numeric quantiles are cell values (nearest rank): admin-only, like the top values
    let stars = column(&doc, "stars");
    assert_eq!(stars["types"]["number"], 5);
    assert_eq!(stars["types"]["string"], 1);
    assert_eq!(stars["numeric"], Value::Null);
    for c in doc["columns"].as_array().unwrap() {
        assert_eq!(c["numeric"], Value::Null);
    }
    let meta = column(&doc, "meta");
    assert_eq!(meta["types"]["object"], 1);
    assert_eq!(meta["types"]["null"], 5);
    assert_eq!(meta["non_null"], 1);
}

#[tokio::test]
async fn dd_profile_admin_gets_the_top_values() {
    let lab = lab();
    seed_dd(&lab, &dd_records());
    let r = lab
        .get(
            &format!("/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"),
            Some(&lab.admin),
        )
        .await;
    assert_eq!(r.status, 200);
    let doc = r.json();
    assert_eq!(doc["values_hidden"], false);
    assert_eq!(
        column(&doc, "stars")["numeric"],
        json!({"min": 1.0, "p50": 3.5, "p95": 5.0, "max": 5.0})
    );
    assert_eq!(column(&doc, "review")["numeric"], Value::Null);
    // a dropped column stays dropped for an admin too
    assert_eq!(doc["columns_dropped"], 1);
    assert!(
        doc["columns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["name"] != "What the model said")
    );
    assert_eq!(
        column(&doc, "category")["top"],
        json!([{"value": "Books", "count": 3}, {"value": "Games", "count": 2}, {"value": "Toys", "count": 1}])
    );
    assert_eq!(
        column(&doc, "ok")["top"],
        json!([{"value": true, "count": 2}, {"value": false, "count": 1}])
    );
    assert_eq!(
        column(&doc, "tags")["top"],
        json!([{"value": "[\"a\"]", "count": 1}, {"value": "[]", "count": 1}])
    );
    // the admin sees the model-written values (that is the point of the role split)
    assert!(r.text().contains(MARK));
    // loopback trust is admin too
    let r = common::TestReq::new(
        Arc::clone(&lab.st),
        "GET",
        &format!("/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"),
    )
    .send()
    .await;
    assert_eq!(r.json()["values_hidden"], false);
}

#[tokio::test]
async fn dd_profile_caps_columns_rows_and_value_sizes() {
    let lab = lab();
    let mut text = String::new();
    // 300 columns: only the first 200 are profiled
    let mut wide = serde_json::Map::new();
    for i in 0..300 {
        wide.insert(format!("c{i:03}"), json!(i));
    }
    wide.insert(format!("bad\u{1}{MARK}"), json!(1));
    text.push_str(&Value::Object(wide).to_string());
    text.push('\n');
    // a large value (under the 1 MiB line cap): measured as the records route shows it (2 KiB cap), the top value
    // cut to 64
    text.push_str(&json!({"c000": "y".repeat(900_000)}).to_string());
    text.push('\n');
    // more than 2000 records: only the first 2000 are read
    for i in 0..2500 {
        text.push_str(&json!({"c001": i}).to_string());
        text.push('\n');
    }
    seed_dd(&lab, &text);
    let r = lab
        .get(
            &format!("/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"),
            Some(&lab.admin),
        )
        .await;
    assert_eq!(r.status, 200);
    let doc = r.json();
    assert!(!r.text().contains(MARK));
    assert_eq!(doc["truncated"], true);
    assert_eq!(doc["rows"], 2000);
    let cols = doc["columns"].as_array().unwrap();
    assert_eq!(cols.len(), runviz::PROFILE_MAX_COLUMNS);
    // 100 columns past the 200th and the one with a bad name
    assert_eq!(doc["columns_dropped"], 101);
    assert_eq!(cols[199]["name"], "c199");
    let c0 = column(&doc, "c000");
    assert_eq!(c0["length"]["max"], 2048.0);
    let top = c0["top"].as_array().unwrap();
    let long = top
        .iter()
        .find(|t| t["value"].as_str().is_some_and(|s| s.starts_with('y')))
        .unwrap();
    assert_eq!(long["value"].as_str().unwrap().chars().count(), 64);
    // more than 50 distinct values: no top values, even for an admin
    let c1 = column(&doc, "c001");
    assert_eq!(c1["distinct"], runviz::PROFILE_DISTINCT_CAP);
    assert_eq!(c1["distinct_capped"], true);
    assert_eq!(c1["top"], Value::Null);
    let c2 = column(&doc, "c002");
    assert_eq!(
        (&c2["distinct"], &c2["distinct_capped"]),
        (&json!(1), &json!(false))
    );
    assert_eq!(c2["top"], json!([{"value": 2, "count": 1}]));
    assert!(r.body.len() < 400_000, "{}", r.body.len());
}

#[tokio::test]
async fn dd_profile_counts_distinct_values_up_to_the_cap() {
    let lab = lab();
    let mut text = String::new();
    for i in 0..1500 {
        text.push_str(&json!({"id": format!("v{i}"), "same": "x"}).to_string());
        text.push('\n');
    }
    seed_dd(&lab, &text);
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    let id = column(&doc, "id");
    assert_eq!(id["distinct"], runviz::PROFILE_DISTINCT_CAP);
    assert_eq!(id["distinct_capped"], true);
    assert_eq!(column(&doc, "same")["distinct"], 1);
}

#[tokio::test]
async fn dd_profile_without_records_is_empty() {
    let lab = lab();
    seed_dd(&lab, "");
    let p = lab
        .state
        .join("datadesigner/runs")
        .join(DD_RUN)
        .join("records.jsonl");
    std::fs::remove_file(p).unwrap();
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(
        doc,
        json!({"run_id": DD_RUN, "rows": 0, "columns": [], "columns_dropped": 0, "values_hidden": true,
            "truncated": false})
    );
}

#[tokio::test]
async fn dd_profile_says_truncated_when_the_byte_cap_stops_it() {
    // 1000 records of about 100 KB: the 64 MiB read cap ends the profile early, and the answer says so
    let lab = lab();
    let mut text = String::new();
    for i in 0..1000 {
        text.push_str(&json!({"id": i, "body": "z".repeat(100_000)}).to_string());
        text.push('\n');
    }
    seed_dd(&lab, &text);
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    let rows = doc["rows"].as_u64().unwrap();
    assert!(rows > 600 && rows < 700, "{rows}");
    assert_eq!(doc["truncated"], true);
}

#[tokio::test]
async fn dd_profile_skips_a_line_over_the_line_cap_and_says_so() {
    // a first line over the 1 MiB line cap is skipped (never held whole), the records after it are profiled
    let lab = lab();
    let mut text = json!({"id": 0, "body": "z".repeat(1_100_000)}).to_string();
    text.push('\n');
    for i in 1..4 {
        text.push_str(&json!({"id": i}).to_string());
        text.push('\n');
    }
    seed_dd(&lab, &text);
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(doc["rows"], 3);
    assert_eq!(doc["truncated"], true);
    assert_eq!(column(&doc, "id")["non_null"], 3);
    // the same with nothing after it: rows 0, still truncated
    seed_dd(&lab, &json!({"body": "z".repeat(1_100_000)}).to_string());
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(doc["rows"], 0);
    assert_eq!(doc["truncated"], true);
}

#[tokio::test]
async fn hardlinked_files_are_not_read() {
    let lab = lab();
    let d = seed_aiperf(&lab);
    seed_dd(&lab, &dd_records());
    // a hardlink in the run directory to a file elsewhere (nlink 2) is refused like a symlink
    let outside = lab.state.join("outside.jsonl");
    std::fs::write(&outside, dd_records()).unwrap();
    let rec = lab
        .state
        .join("datadesigner/runs")
        .join(DD_RUN)
        .join("records.jsonl");
    std::fs::remove_file(&rec).unwrap();
    std::fs::hard_link(&outside, &rec).unwrap();
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(doc["rows"], 0);
    let summary = d.join("artifacts/profile_export_aiperf.json");
    let other = lab.state.join("other.json");
    std::fs::hard_link(&summary, &other).unwrap();
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"))
        .await;
    assert_eq!(doc["metrics"], json!({}));
    // run.json itself: a hardlinked one makes the run unknown
    let meta_link = lab.state.join("meta-link.json");
    std::fs::hard_link(d.join("run.json"), &meta_link).unwrap();
    let r = lab
        .get(
            &format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"),
            Some(&lab.viewer),
        )
        .await;
    assert_eq!(r.status, 404);
}

#[test]
fn json_nodes_bounds_the_values_a_parse_builds() {
    assert_eq!(runviz::json_nodes(b"0"), 1);
    assert_eq!(runviz::json_nodes(br#"{"a": [1, 2, {"b": "x,y:[{"}]}"#), 8);
    let big = format!("[{}0]", "0,".repeat(runviz::JSON_MAX_NODES));
    assert!(runviz::parse_bounded(big.as_bytes()).is_none());
    let ok = format!("[{}0]", "0,".repeat(1000));
    assert!(runviz::parse_bounded(ok.as_bytes()).is_some());
}

// ── errors, roles, symlinks ─────────────────────────────────────────────────────────────────────

const ROUTES: [(&str, &str, &str); 4] = [
    ("aiperf", "summary", AP_RUN),
    ("skillevaluator", "summary", SE_RUN),
    ("aiperf", "requests", AP_RUN),
    ("datadesigner", "profile", DD_RUN),
];

#[tokio::test]
async fn other_systems_unknown_runs_bad_ids_and_roles() {
    let lab = lab();
    seed_aiperf(&lab);
    seed_skilleval(&lab);
    seed_dd(&lab, &dd_records());
    for (sys, route, run) in ROUTES {
        let url = format!("/api/v1/systems/{sys}/runs/{run}/{route}");
        // viewer 200, admin 200, no key from another address 401
        assert_eq!(lab.get(&url, Some(&lab.viewer)).await.status, 200, "{url}");
        assert_eq!(lab.get(&url, Some(&lab.admin)).await.status, 200, "{url}");
        let r = lab.get(&url, None).await;
        assert_eq!(r.status, 401, "{url}");
        assert_eq!(r.code(), "unauthorized");
        // every system without the route: 404, whatever the run id
        for sid in SystemId::ALL {
            let served = match route {
                "summary" => [SystemId::Aiperf, SystemId::Skillevaluator].contains(&sid),
                "requests" => sid == SystemId::Aiperf,
                _ => sid == SystemId::Datadesigner,
            };
            if served {
                continue;
            }
            for rid in [run, "bad id"] {
                let r = lab
                    .get(
                        &format!("/api/v1/systems/{}/runs/{rid}/{route}", sid.as_str()),
                        Some(&lab.viewer),
                    )
                    .await;
                assert_eq!(r.status, 404, "{} {route}", sid.as_str());
                assert_eq!(r.code(), "not_found");
            }
        }
        // unknown run: 404
        let r = lab
            .get(
                &format!("/api/v1/systems/{sys}/runs/20990101T000000-000-nope/{route}"),
                Some(&lab.viewer),
            )
            .await;
        assert_eq!(r.status, 404, "{url}");
        assert_eq!(r.code(), "not_found");
        assert!(r.text().contains("unknown run"));
        // malformed run ids: 422, nothing read
        for bad in [
            "bad%20id",
            "..",
            "%2E%2E",
            "20261101T120000-000-",
            "20261101T120000-000-a%2F..%2F..",
            "20261101T120000-000-a%00",
            "x",
        ] {
            let r = lab
                .get(
                    &format!("/api/v1/systems/{sys}/runs/{bad}/{route}"),
                    Some(&lab.viewer),
                )
                .await;
            assert!(
                r.status == 422 || r.status == 404,
                "{sys} {route} {bad}: {}",
                r.status
            );
            if r.status == 422 {
                assert_eq!(r.code(), "invalid_request");
            }
        }
        let r = lab
            .get(
                &format!("/api/v1/systems/{sys}/runs/bad%20id/{route}"),
                Some(&lab.viewer),
            )
            .await;
        assert_eq!(r.status, 422);
        assert_eq!(r.code(), "invalid_request");
    }
    // AIPerf run ids are stricter than the other two (lowercase preset)
    let r = lab
        .get(
            "/api/v1/systems/aiperf/runs/20261101T120000-000-Smoke/summary",
            Some(&lab.viewer),
        )
        .await;
    assert_eq!(r.status, 422);
}

#[tokio::test]
async fn symlinked_run_dirs_and_files_are_not_followed() {
    let lab = lab();
    let real = seed_aiperf(&lab);
    seed_dd(&lab, &dd_records());
    // a run directory that is a symlink to a real run: unknown
    let link = lab.state.join("aiperf/runs/20261102T120000-000-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let r = lab
        .get(
            "/api/v1/systems/aiperf/runs/20261102T120000-000-link/summary",
            Some(&lab.viewer),
        )
        .await;
    assert_eq!(r.status, 404);
    // a records file that is a symlink: not read
    let outside = lab.state.join("outside.jsonl");
    std::fs::write(&outside, dd_records()).unwrap();
    let rec = lab
        .state
        .join("datadesigner/runs")
        .join(DD_RUN)
        .join("records.jsonl");
    std::fs::remove_file(&rec).unwrap();
    std::os::unix::fs::symlink(&outside, &rec).unwrap();
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(doc["rows"], 0);
    // an artifacts directory that is a symlink: no profile, no requests
    let art = real.join("artifacts");
    let moved = lab.state.join("moved-artifacts");
    std::fs::rename(&art, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &art).unwrap();
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/summary"))
        .await;
    assert_eq!(doc["metrics"], json!({}));
    let doc = lab
        .viewer_json(&format!("/api/v1/systems/aiperf/runs/{AP_RUN}/requests"))
        .await;
    assert_eq!(doc["total"], 0);
    // a FIFO in place of the records file neither blocks nor is read
    std::fs::remove_file(&rec).unwrap();
    nix::unistd::mkfifo(&rec, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let doc = lab
        .viewer_json(&format!(
            "/api/v1/systems/datadesigner/runs/{DD_RUN}/profile"
        ))
        .await;
    assert_eq!(doc["rows"], 0);
}

#[tokio::test]
async fn the_adapter_port_defaults_answer_no_such_endpoint() {
    let lab = lab();
    use nemo_console::adapters::base::AdapterError;
    let gym = &lab.adapters[&SystemId::Gym];
    assert_eq!(
        gym.run_summary(AP_RUN).await.unwrap_err(),
        AdapterError::NoSuchEndpoint
    );
    assert_eq!(
        gym.run_requests(AP_RUN).await.unwrap_err(),
        AdapterError::NoSuchEndpoint
    );
    assert_eq!(
        gym.run_profile(DD_RUN, true).await.unwrap_err(),
        AdapterError::NoSuchEndpoint
    );
    let dd = &lab.adapters[&SystemId::Datadesigner];
    assert_eq!(
        dd.run_summary(DD_RUN).await.unwrap_err(),
        AdapterError::NoSuchEndpoint
    );
    let ap = &lab.adapters[&SystemId::Aiperf];
    assert!(matches!(
        ap.run_requests("../x").await.unwrap_err(),
        AdapterError::Invalid(_)
    ));
    assert!(matches!(
        ap.run_requests("20990101T000000-000-nope")
            .await
            .unwrap_err(),
        AdapterError::UnknownId(_)
    ));
}

// ── event data (D-41) ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn evaluator_events_carry_the_benchmark_numbers() {
    let lab = lab();
    let d = lab.run_dir("evaluator", "20261101T120000-000-smoke");
    write(
        &d.join("run.json"),
        &json!({"run_id": "20261101T120000-000-smoke", "suite": "smoke", "model": "lab-model",
            "verdict": "succeeded", "ts": 5.0, "duration_ms": 100,
            "results": [
                {"bench": "gsm8k", "status": "ok", "metric": "acc", "score": 0.8, "ci_low": 0.7, "ci_high": 0.9,
                 "samples": 10, "tokens": 1234, "elapsed_s": 12.5, "failure_rate": 0.0},
                {"bench": "mmlu", "status": "ok", "score": 0.5, "ci_low": "low", "tokens": true}
            ]})
        .to_string(),
    );
    let ev = lab.adapters[&SystemId::Evaluator]
        .recent_events(10)
        .await
        .unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].kind, "evaluator.run");
    let b = &ev[0].data["benchmarks"];
    assert_eq!(
        b[0],
        json!({"bench": "gsm8k", "status": "ok", "metric": "acc", "score": 0.8, "samples": 10,
            "ci_low": 0.7, "ci_high": 0.9, "tokens": 1234, "elapsed_s": 12.5, "failure_rate": 0.0})
    );
    for k in ["ci_low", "ci_high", "tokens", "elapsed_s", "failure_rate"] {
        assert_eq!(b[1][k], Value::Null, "{k}");
    }
    // the same data reaches the events route
    let r = lab
        .get(
            "/api/v1/systems/evaluator/events?limit=5",
            Some(&lab.viewer),
        )
        .await;
    assert_eq!(r.json()[0]["data"]["benchmarks"][0]["tokens"], 1234);
}

#[tokio::test]
async fn gym_events_carry_at_most_20_rewards() {
    let lab = lab();
    let mut rewards: Vec<Value> = (0..30).map(|i| json!(f64::from(i) / 30.0)).collect();
    rewards.insert(1, json!("x"));
    rewards.insert(2, json!(null));
    let d = lab.run_dir("gym", "20261101T120000-000-smoke");
    write(
        &d.join("run.json"),
        &json!({"run_id": "20261101T120000-000-smoke", "model": "lab-model", "verdict": "succeeded",
            "ts": 5.0, "rewards": rewards, "metrics": {"rollouts": 30}})
        .to_string(),
    );
    let d2 = lab.run_dir("gym", "20261101T110000-000-smoke");
    write(
        &d2.join("run.json"),
        &json!({"run_id": "20261101T110000-000-smoke", "verdict": "failed", "ts": 4.0}).to_string(),
    );
    let ev = lab.adapters[&SystemId::Gym]
        .recent_events(10)
        .await
        .unwrap();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[0].kind, "gym.smoke");
    let r = ev[0].data["rewards"].as_array().unwrap();
    assert_eq!(r.len(), 20);
    assert_eq!(r[0], 0.0);
    assert!(r.iter().all(Value::is_number));
    assert_eq!(ev[1].data["rewards"], Value::Null);
}

// ── OpenAPI ─────────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_openapi_entries_of_the_run_routes_match_the_code() {
    let doc: Value = serde_json::from_str(nemo_console::api::admin::OPENAPI_JSON).unwrap();
    for route in ["summary", "requests", "profile"] {
        let path = format!("/api/v1/systems/{{system_id}}/runs/{{run_id}}/{route}");
        let get = &doc["paths"][&path]["get"];
        let params = get["parameters"].as_array().unwrap();
        let mut names: Vec<(&str, &str)> = params
            .iter()
            .map(|p| (p["name"].as_str().unwrap(), p["in"].as_str().unwrap()))
            .collect();
        names.sort_unstable();
        // the routes take no query parameter
        assert_eq!(names, [("run_id", "path"), ("system_id", "path")], "{path}");
        let def = nemo_console::api::ROUTES
            .iter()
            .find(|r| r.path == path)
            .unwrap();
        assert_eq!(def.methods, ["GET"]);
        assert_eq!(def.role, nemo_console::api::RoleDep::Viewer);
        assert!(get["description"].as_str().unwrap().contains("9.15"));
    }
    let profile =
        doc["paths"]["/api/v1/systems/{system_id}/runs/{run_id}/profile"]["get"]["description"]
            .as_str()
            .unwrap();
    assert!(profile.contains("values_hidden"));
    assert!(profile.contains(&runviz::PROFILE_MAX_COLUMNS.to_string()));
}
