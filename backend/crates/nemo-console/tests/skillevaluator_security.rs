//! Rust-native tests of the SkillEvaluator adapter's security and robustness controls (no Python counterpart ids):
//! an oversized or deeply nested result.json (written under the influence of the evaluated skill) is refused without
//! harm, the skill path never resolves outside the skills dir through a symlink, and a runtime shutdown marks a live
//! run interrupted.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "skillevaluator_helpers.rs"]
mod helpers;

use std::path::Path;
use std::time::Duration;

use helpers::*;
use nemo_console::adapters::base::SystemAdapter;
use serde_json::json;

fn live() -> serde_json::Map<String, serde_json::Value> {
    params(json!({"skill": "robust-scripts", "attempts": 1}))
}

fn planted(content: &'static [u8]) -> Resp {
    func(move |argv| {
        let i = argv.iter().position(|x| x == "--results-dir").unwrap();
        let out = Path::new(&argv[i + 1]).join(T3_SKILL).join(T3_RUN);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("result.json"), content).unwrap();
        res(&tier3_stdout(Some("succeeded"), None, 0), 0, "")
    })
}

// two mock routers in one test: not a current-thread runtime (wiremock's drop blocks on it)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_oversized_and_deeply_nested_results_are_refused() {
    static BIG: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        format!("{{\"pad\": \"{}\"}}", "x".repeat(9 * 1024 * 1024)).into_bytes()
    });
    static DEEP: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        format!("{}{}", "[".repeat(300_000), "]".repeat(300_000)).into_bytes()
    });
    for content in [&BIG[..], &DEEP[..]] {
        let f = Fx::new().await;
        let run = standard_runner("skillevaluator-local");
        run.on(&["skillevaluator-local", "tier3"], planted(content));
        let a = f.adapter(&run);
        assert!(a.run_action("evaluate_live", &live()).await.unwrap().ok);
        assert!(a.live().wait(10.0).await);
        let ev = &a.recent_events(1).await.unwrap()[0];
        assert_eq!(ev.data["verdict"], "error");
    }
}

#[tokio::test]
async fn test_a_symlinked_skill_dir_never_escapes_the_skills_root() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner("skillevaluator-local"));
    // a symlink INSIDE the root that points to another skill resolves to that skill (Python's resolve() rule);
    // one pointing outside never resolves
    let root = f.path().join("repo/.claude/skills");
    std::os::unix::fs::symlink(root.join("serve-model"), root.join("alias-skill")).unwrap();
    assert_eq!(
        a.skill_path("alias-skill").unwrap(),
        root.join("serve-model")
    );
    assert!(a.skill_path("linked").is_err());
    assert!(
        !a.skills().contains(&"alias-skill".to_owned()),
        "symlinks are never offered"
    );
}

#[test]
fn test_runtime_shutdown_marks_a_live_run_interrupted() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (rid, runs, keep) = rt.block_on(async {
        let f = Fx::new().await;
        let gate = Gate::new();
        let run = standard_runner("skillevaluator-local");
        run.on(
            &["skillevaluator-local", "tier3"],
            gated(writes_tier3(Some("tier3_result.json"), 0, ""), &gate),
        );
        let a = f.adapter(&run);
        assert!(a.run_action("evaluate_live", &live()).await.unwrap().ok);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let rid = a.live_run_id().unwrap();
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
    drop(keep);
}
