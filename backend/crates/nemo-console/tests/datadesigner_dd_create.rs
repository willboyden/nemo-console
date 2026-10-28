//! Port of backend/tests/datadesigner/test_dd_create.py: the ONE background `create` run, destination containment,
//! formats, the records cap, interrupted runs, and the core's confirm + param checks. Offline: fakes + captured
//! fixtures only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
#[path = "datadesigner_helpers.rs"]
mod helpers;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use helpers::*;
use nemo_console::adapters::base::{CommandRunner, SystemAdapter};
use nemo_console::adapters::datadesigner::MAX_PREVIEW_RECORDS;
use nemo_console::config::settings_for_test;
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::SystemId;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Value, json};

fn live() -> serde_json::Map<String, Value> {
    params(json!({"script": "reviews_smoke.py", "num_records": 10}))
}

fn with(
    mut p: serde_json::Map<String, Value>,
    k: &str,
    v: Value,
) -> serde_json::Map<String, Value> {
    p.insert(k.into(), v);
    p
}

fn gated_create(gate: &Gate) -> Arc<FakeRunner> {
    let run = standard_runner();
    run.on(&[LAUNCHER, "create"], gated(create_default(), gate));
    run
}

fn lines(p: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

// ── start in the background ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_starts_in_the_background_and_returns_at_once() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let run = gated_create(&gate);
    let a = f.adapter(&run);
    let res = tokio::time::timeout(Duration::from_secs(2), a.run_action("create", &live()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        res.ok
            && res.output.contains("Started dataset build")
            && res.output.contains("in the background")
    );
    let rid = a.live_run_id().unwrap();
    assert!(rid.ends_with("-reviews_smoke.py"));
    let run_dir = a.runs_dir().join(&rid);
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "running");
    assert_eq!(meta["mode"], "create");
    assert_eq!(meta["dataset_name"], "dataset_reviews_smoke");
    assert_eq!(meta["destination"], "dataset_reviews_smoke");
    assert_eq!(meta["format"], "parquet");
    assert_eq!(meta["num_records_requested"], 10);
    assert_eq!(mode_of(&run_dir), 0o700);
    assert_eq!(mode_of(&run_dir.join("run.json")), 0o600);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let calls = run.calls();
    let call = calls
        .iter()
        .find(|c| c.get(1).map(String::as_str) == Some("create"))
        .unwrap();
    assert_eq!(call[..2], [LAUNCHER, "create"]);
    assert!(call.contains(&"--no-tui".to_owned()) && call.contains(&"--num-records".to_owned()));
    let i = call.iter().position(|x| x == "--dataset-name").unwrap();
    assert_eq!(call[i + 1], "dataset_reviews_smoke");
    assert_eq!(
        run.cwds().last().unwrap().as_deref(),
        Some(run_dir.as_path())
    );
    let st = a.status().await.unwrap();
    assert!(
        st.components
            .iter()
            .any(|c| c.name == "create reviews_smoke.py" && c.state == "running")
    );
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["verdict"], "running");
    assert!(ev.summary.contains("running for"));
    let p: HashMap<String, f64> = a
        .metrics()
        .await
        .unwrap()
        .points
        .iter()
        .map(|p| (p.key.clone(), p.value))
        .collect();
    assert_eq!(p["dd.live_running"], 1.0);
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(rep.ok && rep.output.contains("Running for"));
    gate.set();
    finish(&a).await;
    assert_eq!(run_json(&a, &rid)["verdict"], "succeeded");
    assert!(a.live_run_id().is_none());
}

#[tokio::test]
async fn test_one_create_at_a_time() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let run = gated_create(&gate);
    let a = f.adapter(&run);
    let first = a.run_action("create", &live()).await.unwrap();
    let second = a
        .run_action("create", &params(json!({"script": "tiny.yaml"})))
        .await
        .unwrap();
    assert!(first.ok && !second.ok);
    assert_eq!(
        codes(&second.problems).into_iter().collect::<Vec<_>>(),
        ["dd.create.busy"]
    );
    assert!(
        second.output.contains("already running")
            && second.output.contains(&a.live_run_id().unwrap())
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        run.calls()
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("create"))
            .count(),
        1
    );
    assert_eq!(std::fs::read_dir(a.runs_dir()).unwrap().count(), 1);
    gate.set();
    finish(&a).await;
    assert!(
        a.run_action("create", &params(json!({"script": "tiny.yaml"})))
            .await
            .unwrap()
            .ok
    );
    finish(&a).await;
}

// ── completion: records.jsonl + the user copy ──────────────────────────────────────────────────

#[tokio::test]
async fn test_completion_records_metadata_and_copies_the_parquet_batches() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let res = a
        .run_action(
            "create",
            &params(
                json!({"script": "reviews_smoke.py", "num_records": 2, "format": "parquet",
                           "dataset_name": "smoke_set", "destination_subdir": "smoke/nested"}),
            ),
        )
        .await
        .unwrap();
    assert!(res.ok);
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "succeeded");
    assert_eq!(meta["records"], json!(2.0));
    assert_eq!(meta["batches"], 1);
    assert_eq!(meta["dataset_name"], "smoke_set");
    assert_eq!(meta["destination"], "smoke/nested");
    let dest = datasets_root(&a).join("smoke/nested");
    assert!(dest.join("batch_00000.parquet").is_file());
    assert_eq!(
        std::fs::read(dest.join("batch_00000.parquet")).unwrap(),
        std::fs::read(fxp("parquet-files/batch_00000.parquet")).unwrap()
    );
    assert_eq!(
        lines(&a.runs_dir().join(&rid).join("records.jsonl")).len(),
        2
    );
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["num_records"], json!(2.0));
    assert_eq!(ev.data["dataset_name"], "smoke_set");
    assert_eq!(ev.data["format"], "parquet");
    assert!(ev.summary.contains("succeeded") && ev.summary.contains("smoke_set"));
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(
        rep.ok
            && rep.output.contains("| product_category | review |")
            && rep.output.contains("batch")
    );
    let p: HashMap<String, f64> = a
        .metrics()
        .await
        .unwrap()
        .points
        .iter()
        .map(|p| (p.key.clone(), p.value))
        .collect();
    assert_eq!(p["dd.last_records"], 2.0);
    assert_eq!(p["dd.runs_by_verdict{verdict=succeeded}"], 1.0);
}

// several mock routers in one test: wiremock's drop blocks on the runtime, so not a current-thread one
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_converted_formats() {
    for fmt in ["csv", "jsonl", "json"] {
        let f = Fx::new().await;
        let a = f.adapter(&standard_runner());
        let res = a
            .run_action(
                "create",
                &params(json!({"script": "reviews_smoke.py", "num_records": 2, "format": fmt})),
            )
            .await
            .unwrap();
        assert!(res.ok);
        let rid = started_run_id(&res.output);
        finish(&a).await;
        assert_eq!(run_json(&a, &rid)["verdict"], "succeeded", "{fmt}");
        let target =
            datasets_root(&a).join(format!("dataset_reviews_smoke/dataset_reviews_smoke.{fmt}"));
        assert!(target.is_file(), "{fmt}");
        assert_eq!(mode_of(&target), 0o600);
        let text = std::fs::read_to_string(&target).unwrap();
        match fmt {
            "jsonl" => {
                let l: Vec<&str> = text.lines().collect();
                assert_eq!(l.len(), 2);
                assert_eq!(
                    serde_json::from_str::<Value>(l[0]).unwrap()["product_category"],
                    "Books"
                );
            }
            "csv" => {
                assert!(text.starts_with("product_category,review\r\n"));
                assert_eq!(text, csv_text(&rows()));
                let rec = lines(&a.runs_dir().join(&rid).join("records.jsonl"));
                assert_eq!(rec.len(), 2);
                assert_eq!(
                    serde_json::from_str::<Value>(&rec[0]).unwrap()["product_category"],
                    "Books"
                );
            }
            _ => {
                let data: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(data.as_array().unwrap().len(), 2);
                assert_eq!(data[1]["product_category"], "Electronics");
            }
        }
    }
}

#[tokio::test]
async fn test_multi_batch_json_and_csv_and_the_records_cap() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(
        &[LAUNCHER, "create"],
        writes_create(0, "", 3, Some(6), true, true),
    );
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "create",
            &params(json!({"script": "reviews_smoke.py", "num_records": 6, "format": "json"})),
        )
        .await
        .unwrap();
    assert!(res.ok);
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "succeeded");
    assert_eq!(meta["batches"], 3);
    assert_eq!(meta["records"], json!(6.0));
    let target = datasets_root(&a).join("dataset_reviews_smoke/dataset_reviews_smoke.json");
    let data: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(data.as_array().unwrap().len(), 6);
    assert_eq!(
        lines(&a.runs_dir().join(&rid).join("records.jsonl")).len(),
        6
    );
    // the console copy is capped even when the dataset is bigger
    let big: Vec<Value> = (0..MAX_PREVIEW_RECORDS + 500)
        .map(|i| json!({"product_category": format!("c{i}"), "review": "x".repeat(20)}))
        .collect();
    run.on(&[EXPORTER], export_responder(0, Some(big)));
    run.on(
        &[LAUNCHER, "create"],
        writes_create(
            0,
            "",
            1,
            Some(i64::try_from(MAX_PREVIEW_RECORDS + 500).unwrap()),
            true,
            true,
        ),
    );
    let a2 = f.adapter(&run);
    a2.run_action(
        "create",
        &params(
            json!({"script": "tiny.yaml", "num_records": 10, "format": "parquet",
                       "dataset_name": "big", "destination_subdir": "big"}),
        ),
    )
    .await
    .unwrap();
    let rid2 = a2.live_run_id().unwrap();
    finish(&a2).await;
    let meta2 = run_json(&a2, &rid2);
    assert_eq!(meta2["verdict"], "succeeded");
    assert_eq!(meta2["records"].as_f64().unwrap(), 2500.0);
    assert_eq!(
        lines(&a2.runs_dir().join(&rid2).join("records.jsonl")).len(),
        MAX_PREVIEW_RECORDS
    );
    let page = a2.records_page(&rid2, 5, 0).await.unwrap();
    assert_eq!(page["total"], 2500);
    assert_eq!(page["rows"].as_array().unwrap().len(), 5);
}

// ── refusals and failure-is-data ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_bad_values_are_refused_without_running() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    for (p, why) in [
        (params(json!({"script": "nope.py"})), "unknown script"),
        (with(live(), "num_records", json!(0)), "num_records"),
        (with(live(), "num_records", json!(true)), "num_records"),
        (with(live(), "num_records", json!(100_001)), "num_records"),
        (
            with(live(), "dataset_name", json!("bad name")),
            "dataset_name",
        ),
        (
            with(live(), "destination_subdir", json!("/etc")),
            "destination_subdir",
        ),
        (
            with(live(), "destination_subdir", json!("../outside")),
            "destination_subdir",
        ),
        (
            with(live(), "destination_subdir", json!("a/../b")),
            "destination_subdir",
        ),
        (
            with(live(), "destination_subdir", json!("a//b")),
            "destination_subdir",
        ),
        (with(live(), "format", json!("xlsx")), "format"),
    ] {
        let r = a.run_action("create", &p).await.unwrap();
        assert!(!r.ok && r.output.contains(why), "{p:?}: {}", r.output);
    }
    assert!(
        !run.calls()
            .iter()
            .any(|c| c.get(1).map(String::as_str) == Some("create"))
    );
    assert!(a.live_run_id().is_none() && !a.runs_dir().exists());
}

#[tokio::test]
async fn test_destination_may_not_escape_the_datasets_root() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let outside = f.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let root = datasets_root(&a);
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    std::fs::write(root.join("blocker"), "a file, not a directory\n").unwrap();
    let r = a
        .run_action(
            "create",
            &with(live(), "destination_subdir", json!("link/evil")),
        )
        .await
        .unwrap();
    assert!(!r.ok && r.output.contains("outside the datasets root"));
    let r2 = a
        .run_action(
            "create",
            &with(live(), "destination_subdir", json!("blocker")),
        )
        .await
        .unwrap();
    assert!(!r2.ok && r2.output.contains("not a directory"));
    assert!(a.live_run_id().is_none());
}

#[tokio::test]
async fn test_failed_create_is_data() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(
        &[LAUNCHER, "create"],
        ok_rc("", 1, "Model call failed: Bearer sk-abcdefghijklmnop"),
    );
    let a = f.adapter(&run);
    a.run_action("create", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "failed");
    assert!(
        meta["error"]
            .as_str()
            .unwrap()
            .contains("Model call failed")
    );
    assert!(
        !std::fs::read_to_string(a.runs_dir().join(&rid).join("run.json"))
            .unwrap()
            .contains("sk-abcdefghijklmnop")
    );
    let rep = a
        .run_action("report", &params(json!({"run_id": rid})))
        .await
        .unwrap();
    assert!(rep.output.contains("did not complete") && !rep.output.contains("sk-abcdefghijklmnop"));
}

#[tokio::test]
async fn test_create_without_metadata_or_batches_is_an_error() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(
        &[LAUNCHER, "create"],
        writes_create(0, "", 1, None, false, true),
    );
    let a = f.adapter(&run);
    a.run_action("create", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let m = run_json(&a, &rid);
    assert_eq!(m["verdict"], "error");
    assert!(m["error"].as_str().unwrap().contains("metadata.json"));
    run.on(
        &[LAUNCHER, "create"],
        writes_create(0, "", 1, None, true, false),
    );
    let b = f.adapter(&run);
    b.run_action("create", &live()).await.unwrap();
    let rid2 = b.live_run_id().unwrap();
    finish(&b).await;
    let m2 = run_json(&b, &rid2);
    assert_eq!(m2["verdict"], "error");
    assert!(m2["error"].as_str().unwrap().contains("batch"));
}

#[tokio::test]
async fn test_export_failure_fails_the_run_but_keeps_the_batches() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(&[EXPORTER], ok_rc("", 2, "parquet unreadable"));
    let a = f.adapter(&run);
    a.run_action("create", &live()).await.unwrap();
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "failed");
    assert!(meta["error"].as_str().unwrap().contains("converting"));
    assert!(
        datasets_root(&a)
            .join("dataset_reviews_smoke/batch_00000.parquet")
            .is_file()
    );
}

// ── interrupted ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_console_stop_cancels_the_build_and_marks_it_interrupted() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let a = f.adapter(&gated_create(&gate));
    a.run_action("create", &live()).await.unwrap();
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
    assert!(rep.output.contains("Interrupted"));
}

// ── through the core: confirm required, params validated against the ActionSpec ─────────────────

#[tokio::test]
async fn test_core_requires_confirm_and_validates_create_params() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let run = gated_create(&gate);
    let a = Arc::new(f.adapter(&run));
    let tmp = f.path();
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
        SystemId::Datadesigner,
        Arc::clone(&a) as Arc<dyn SystemAdapter>,
    );
    // another system (no run artifacts): the shared fake, so the 404 below does not depend on workstream A's switchyard
    adapters.insert(
        SystemId::Switchyard,
        common::FakeAdapter::new(SystemId::Switchyard, None) as Arc<dyn SystemAdapter>,
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
    let req = |m: &str, p: &str| common::TestReq::new(Arc::clone(&state), m, p);
    let url = "/api/v1/systems/datadesigner/actions/create";
    let specs = req("GET", "/api/v1/systems/datadesigner/actions")
        .send()
        .await
        .json();
    let spec = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "create")
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
        json!({"script": "reviews_smoke.py", "num_records": 100, "dataset_name": null,
               "destination_subdir": null, "format": "parquet"})
    );
    let r = req("POST", url)
        .json(json!({"params": live()}))
        .send()
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.code(), "confirm_required");
    for bad in [
        json!({"num_records": 0}),
        json!({"num_records": "2"}),
        json!({"format": "xlsx"}),
        json!({"script": "nope.py"}),
        json!({"dataset_name": "bad\nname"}),
        json!({"unknown": 1}),
    ] {
        let mut p = live();
        for (k, v) in bad.as_object().unwrap() {
            p.insert(k.clone(), v.clone());
        }
        let r = req("POST", url)
            .json(json!({"params": p, "confirm": true}))
            .send()
            .await;
        assert_eq!(r.status, 422, "{bad}");
        assert_eq!(r.code(), "invalid_param", "{bad}");
    }
    let r = req("POST", url)
        .json(
            json!({"params": with(live(), "destination_subdir", json!("../out")), "confirm": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], false);
    assert!(
        r.json()["output"]
            .as_str()
            .unwrap()
            .contains("datasets root")
    );
    assert!(
        !run.calls()
            .iter()
            .any(|c| c.get(1).map(String::as_str) == Some("create"))
    );
    let r = req("POST", url)
        .json(json!({"params": live(), "confirm": true}))
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], true);
    let rid = a.live_run_id().unwrap();
    // while the build runs (no records.jsonl yet) the records endpoint answers an EMPTY page, not 404
    let live_page = req(
        "GET",
        &format!("/api/v1/systems/datadesigner/runs/{rid}/records"),
    )
    .no_csrf()
    .send()
    .await;
    assert_eq!(live_page.status, 200);
    assert_eq!(live_page.json()["rows"], json!([]));
    assert_eq!(live_page.json()["total"], 0);
    gate.set();
    finish(&a).await;
    let rec = req(
        "GET",
        &format!("/api/v1/systems/datadesigner/runs/{rid}/records?limit=1"),
    )
    .no_csrf()
    .send()
    .await;
    assert_eq!(rec.status, 200);
    assert_eq!(rec.json()["total"], 2);
    assert_eq!(rec.json()["rows"].as_array().unwrap().len(), 1);
    let files = req(
        "GET",
        &format!("/api/v1/systems/datadesigner/runs/{rid}/files"),
    )
    .no_csrf()
    .send()
    .await;
    assert_eq!(files.status, 200);
    assert!(
        files.json()["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "records.jsonl")
    );
    assert_eq!(
        req("GET", "/api/v1/systems/datadesigner/runs/nope/records")
            .no_csrf()
            .send()
            .await
            .status,
        404
    );
    assert_eq!(
        req("GET", "/api/v1/systems/switchyard/runs/x/records")
            .no_csrf()
            .send()
            .await
            .status,
        404
    );
}
