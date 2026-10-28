//! Rust-native tests of the Data Designer adapter's security and robustness controls (no Python counterpart ids):
//! path containment of every file the adapter reads or copies, no option-like value reaching argv, bounded YAML and
//! JSON parsing, and the console-stop outcome of a live build when the runtime goes away.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "datadesigner_helpers.rs"]
mod helpers;

use std::path::Path;
use std::time::{Duration, Instant};

use helpers::*;
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::datadesigner::dest_subdir_ok;
use proptest::prelude::*;
use serde_json::json;

fn live() -> serde_json::Map<String, serde_json::Value> {
    params(json!({"script": "reviews_smoke.py", "num_records": 2, "format": "jsonl"}))
}

#[tokio::test]
async fn test_batch_files_outside_the_dataset_dir_are_never_read() {
    let f = Fx::new().await;
    let outside = f.path().join("outside.parquet");
    std::fs::write(&outside, "SECRET").unwrap();
    let run = standard_runner();
    let o2 = outside.clone();
    run.on(
        &[LAUNCHER, "create"],
        func(move |argv| {
            let at = |fl: &str| argv[argv.iter().position(|a| a == fl).unwrap() + 1].clone();
            let out = Path::new(&at("--artifact-path")).join(at("--dataset-name"));
            std::fs::create_dir_all(out.join("parquet-files")).unwrap();
            std::fs::copy(fxp("parquet-files/batch_00000.parquet"), out.join("parquet-files/batch_00000.parquet")).unwrap();
            std::os::unix::fs::symlink(&o2, out.join("parquet-files/link.parquet")).unwrap();
            let md = json!({"actual_num_records": 2, "file_paths": {"parquet-files": [
                "../../../../outside.parquet", o2.to_str().unwrap(), "parquet-files/link.parquet", "",
                "parquet-files/batch_00000.parquet"]}});
            std::fs::write(out.join("metadata.json"), md.to_string()).unwrap();
            res("", 0, "")
        }),
    );
    let a = f.adapter(&run);
    assert!(a.run_action("create", &live()).await.unwrap().ok);
    let rid = a.live_run_id().unwrap();
    finish(&a).await;
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "succeeded");
    assert_eq!(
        meta["batches"], 1,
        "only the contained regular batch file counts"
    );
    for c in run.calls().iter().filter(|c| c[0] == EXPORTER) {
        assert!(
            c[1].ends_with("/parquet-files/batch_00000.parquet"),
            "{c:?}"
        );
    }
}

#[tokio::test]
async fn test_run_files_never_follow_symlinks() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    a.run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    let rid = a.recent_events(1).await.unwrap()[0].data["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run_dir = a.runs_dir().join(&rid);
    let outside = f.path().join("secret-dir");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "SECRET").unwrap();
    std::os::unix::fs::symlink(&outside, run_dir.join("linked-dir")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), run_dir.join("linked.txt")).unwrap();
    let out = a.run_files(&rid).await.unwrap();
    let paths: Vec<&str> = out["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(
        paths
            .iter()
            .all(|p| !p.contains("linked") && !p.contains("secret")),
        "{paths:?}"
    );
    assert!(paths.contains(&"records.jsonl"));
}

#[tokio::test]
async fn test_a_symlinked_preview_dir_is_never_read() {
    let f = Fx::new().await;
    let outside = f.path().join("planted");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("dataset.parquet"), "SECRET").unwrap();
    let run = standard_runner();
    let o2 = outside.clone();
    run.on(
        &[LAUNCHER, "preview"],
        func(move |argv| {
            let o = argv.iter().position(|a| a == "-o").unwrap();
            std::os::unix::fs::symlink(
                &o2,
                Path::new(&argv[o + 1]).join("preview_results_20261023_222454"),
            )
            .unwrap();
            res("", 0, "")
        }),
    );
    let a = f.adapter(&run);
    let r = a
        .run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    assert!(
        !r.ok
            && r.output
                .contains("no preview_results_* directory was written"),
        "{}",
        r.output
    );
    assert!(!run.calls().iter().any(|c| c[0] == EXPORTER));
}

#[tokio::test]
async fn test_option_like_values_never_reach_argv() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    for (k, v) in [
        ("dataset_name", "--help"),
        ("dataset_name", "-x"),
        ("destination_subdir", "-x/y"),
        ("script", "--version"),
        ("script", "-n.py"),
    ] {
        let mut p = live();
        p.insert(k.into(), json!(v));
        let r = a.run_action("create", &p).await.unwrap();
        assert!(!r.ok, "{k}={v}");
    }
    let r = a
        .run_action("preview", &params(json!({"script": "--save-results"})))
        .await
        .unwrap();
    assert!(!r.ok && r.output.contains("unknown script"));
    let r = a
        .run_action("check_models", &params(json!({"script": "-h"})))
        .await
        .unwrap();
    assert!(!r.ok);
    assert!(
        run.calls()
            .iter()
            .all(|c| c[0] == LAUNCHER && c[1] == "--version"),
        "{:?}",
        run.calls()
    );
}

#[tokio::test]
async fn test_yaml_alias_bombs_and_deep_nesting_stay_bounded() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let cfg = f.path().join("home/.data-designer");
    // nine levels of ten aliases each: 10^9 values if copied; shared objects in Python (and here)
    let mut bomb = String::from(
        "model_configs:\n- alias: lab\n  model: m\na0: &a0 [x, x, x, x, x, x, x, x, x, x]\n",
    );
    for i in 1..10 {
        let p = i - 1;
        bomb.push_str(&format!(
            "a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n"
        ));
    }
    std::fs::write(cfg.join("model_configs.yaml"), &bomb).unwrap();
    let t0 = Instant::now();
    let aliases = a.aliases().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert_eq!(aliases.len(), 1);
    // 200k nested flow sequences: Python's composer hits its recursion limit (RecursionError escapes); never a
    // stack overflow here
    std::fs::write(cfg.join("model_configs.yaml"), "[".repeat(200_000)).unwrap();
    let e = a.aliases().unwrap_err();
    assert!(
        matches!(
            e.py_class(),
            "RecursionError" | "ParserError" | "ScannerError"
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn test_too_deep_record_lines_are_skipped_without_crashing() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let (rid, run_dir) = a.new_run_dir("reviews_smoke.py").unwrap();
    std::fs::write(
        run_dir.join("run.json"),
        json!({"run_id": rid, "verdict": "succeeded"}).to_string(),
    )
    .unwrap();
    let deep = format!("{}{}\n", "[".repeat(100_000), "]".repeat(100_000));
    std::fs::write(
        run_dir.join("records.jsonl"),
        format!("{deep}{{\"a\": 1}}\n"),
    )
    .unwrap();
    let page = a.records_page(&rid, 10, 0).await.unwrap();
    assert_eq!(page["rows"], json!([{"a": 1}]));
}

#[test]
fn test_runtime_shutdown_marks_a_live_create_interrupted() {
    // Python: the console stops -> the task is cancelled -> `except CancelledError` writes the outcome. Rust: the
    // server drops its runtime before exiting, which drops the task and its guard writes the same outcome.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (rid, runs, _keep) = rt.block_on(async {
        let f = Fx::new().await;
        let gate = Gate::new();
        let run = standard_runner();
        run.on(&[LAUNCHER, "create"], gated(create_default(), &gate));
        let a = f.adapter(&run);
        assert!(a.run_action("create", &live()).await.unwrap().ok);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let rid = a.live_run_id().unwrap();
        // the mock router goes while the runtime still runs (its drop blocks on it); the temp dir stays
        let Fx { tmp, router } = f;
        drop(router);
        (rid, a.runs_dir(), (tmp, a, gate))
    });
    drop(rt);
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(runs.join(&rid).join("run.json")).unwrap())
            .unwrap();
    assert_eq!(meta["verdict"], "interrupted");
    assert_eq!(meta["interrupted_by"], "console stop");
}

proptest! {
    /// Whatever `_dest_subdir_ok` accepts stays lexically under the datasets root: relative, no `.`/`..`/empty
    /// segment, no backslash, at most 8 segments.
    #[test]
    fn prop_accepted_destinations_stay_under_the_root(s in "[A-Za-z0-9._/\\\\-]{0,40}") {
        if dest_subdir_ok(&s) {
            prop_assert!(!s.starts_with('/') && !s.contains('\\'));
            let parts: Vec<&str> = s.split('/').collect();
            prop_assert!(parts.len() <= 8);
            prop_assert!(parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".."));
            let root = Path::new("/r");
            prop_assert!(root.join(&s).starts_with(root));
        }
    }
}
