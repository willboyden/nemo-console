//! Rust-native tests of the NeMo Evaluator adapter's security and robustness controls (no Python counterpart ids):
//! the wrapper validates a private temp copy that is removed, a planted summary.json symlink is never read, nel's own
//! result files are never read, YAML bombs stay bounded, the YAML loader never panics, and a runtime shutdown marks
//! a live run interrupted.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "evaluator_helpers.rs"]
mod helpers;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use helpers::*;
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::services::pyyaml::safe_load;
use proptest::prelude::*;
use serde_json::json;

#[tokio::test]
async fn test_wrapper_validates_a_private_temp_copy_that_is_removed() {
    let f = Fx::new().await;
    let seen: Arc<Mutex<Vec<(String, u32, String)>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    let r = standard_runner();
    r.on(
        &[LAUNCHER, "suites"],
        func(move |argv| {
            let p = Path::new(&argv[3]);
            s2.lock().unwrap().push((
                argv[3].clone(),
                mode_of(p),
                std::fs::read_to_string(p).unwrap(),
            ));
            res("", 0, "")
        }),
    );
    let a = f.adapter(&r);
    let text = fx("suites_good.yaml");
    assert!(a.validate_config("suites.yaml", &text).await.unwrap().ok);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (path, mode, content) = &seen[0];
    assert!(path.starts_with(f.path().join("state/evaluator/tmp").to_str().unwrap()));
    assert_eq!(*mode, 0o600);
    assert_eq!(*content, text);
    assert!(!Path::new(path).exists(), "the candidate copy is deleted");
    assert_eq!(mode_of(&f.path().join("state/evaluator/tmp")), 0o700);
}

#[tokio::test]
async fn test_a_planted_summary_symlink_is_never_read() {
    let f = Fx::new().await;
    let secret = f.path().join("elsewhere.json");
    std::fs::write(&secret, fx("summary_smoke.json")).unwrap();
    let r = standard_runner();
    let s2 = secret.clone();
    r.on(
        &[LAUNCHER, "run"],
        func(move |argv| {
            let i = argv.iter().position(|a| a == "--output-dir").unwrap();
            let out = Path::new(&argv[i + 1]);
            std::fs::create_dir_all(out).unwrap();
            std::os::unix::fs::symlink(&s2, out.join("summary.json")).unwrap();
            res("", 0, "")
        }),
    );
    let a = f.adapter(&r);
    assert!(
        a.run_action("run", &params(json!({"suite": "smoke-lightning"})))
            .await
            .unwrap()
            .ok
    );
    assert!(a.live().wait(5.0).await);
    let meta = read_json(&only_run_dir(&a).join("run.json"));
    assert_eq!(meta["verdict"], "error");
    assert!(meta.get("results").is_none());
}

#[tokio::test]
async fn test_yaml_bombs_in_suites_stay_bounded() {
    let f = Fx::new().await;
    let mut bomb = fx("suites_good.yaml");
    bomb.push_str("x0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for i in 1..10 {
        let p = i - 1;
        bomb.push_str(&format!(
            "x{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n"
        ));
    }
    let a = f.adapter_with(&FakeRunner::new(), Some(&bomb), true, None);
    let t0 = Instant::now();
    let v = a.validate_config("suites.yaml", &bomb).await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(
        !v.ok
            && v.diagnostics
                .iter()
                .any(|d| d.message.contains("unknown top-level key 'x0'"))
    );
    let deep = "[".repeat(300_000);
    let e = a.validate_config("suites.yaml", &deep).await;
    assert!(e.is_err() || !e.unwrap().ok);
}

#[test]
fn test_runtime_shutdown_marks_a_live_run_interrupted() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (runs, keep) = rt.block_on(async {
        let f = Fx::new().await;
        let gate = Gate::new();
        let r = standard_runner();
        r.on(
            &[LAUNCHER, "run"],
            gated(writes_summary("summary_smoke.json", 0, None), &gate),
        );
        let a = f.adapter(&r);
        assert!(
            a.run_action("run", &params(json!({"suite": "smoke-lightning"})))
                .await
                .unwrap()
                .ok
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        let Fx { tmp, router } = f;
        drop(router);
        (a.runs_dir(), (tmp, a, gate))
    });
    drop(rt);
    let dir = std::fs::read_dir(&runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let meta = read_json(&dir.join("run.json"));
    assert_eq!(meta["verdict"], "interrupted");
    assert_eq!(meta["interrupted_by"], "console stop");
    drop(keep);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]
    /// The YAML loader never panics or hangs, whatever the input (it returns a value or a Python error).
    #[test]
    fn prop_safe_load_never_panics(s in "[-:?\\[\\]{},#&*!|>'\" \n\t.~=<a1%@`\\\\0x]{0,60}") {
        let _ = safe_load(&s);
    }
}
