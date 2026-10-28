//! Port of backend/tests/aiperf/test_ap_adapter.py: status, metrics, config validation (profiles.yaml schema),
//! actions listing. Offline (fake runner, fake router on an ephemeral port).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/aiperf/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use env::{ApEnv, LAUNCHER, Opts};
use futures::StreamExt;
use nemo_console::adapters::aiperf::{AiPerfAdapter, parse_profiles, render_argv, summarize};
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::domain::models::{
    Danger, Health, MetricsSnapshot, ParamType, PostCheck, Role, Severity, SystemStatus,
};
use nemo_console::services::runner::{AIPERF_BINARIES, AIPERF_PROFILE, max_timeout};
use serde_json::{Value, json};
use support::{FakeRunner, fx};

fn codes(st: &SystemStatus) -> HashSet<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

fn pts(snap: &MetricsSnapshot, key: &str) -> HashMap<String, f64> {
    snap.points
        .iter()
        .filter(|p| p.key == key)
        .map(|p| {
            (
                p.labels
                    .get("q")
                    .or_else(|| p.labels.get("verdict"))
                    .cloned()
                    .unwrap_or_default(),
                p.value,
            )
        })
        .collect()
}

fn seed_run(a: &AiPerfAdapter, run_id: &str, meta: Value) -> PathBuf {
    let d = a.runs_dir().join(run_id);
    std::fs::create_dir_all(&d).unwrap();
    let preset = run_id.splitn(3, '-').nth(2).unwrap();
    let mut base = json!({"run_id": run_id, "preset": preset, "ts": 1_700_000_000.0, "verdict": "succeeded",
                          "model": "aitop/nemotron-lightning", "concurrency": 2});
    for (k, v) in meta.as_object().unwrap() {
        base[k] = v.clone();
    }
    std::fs::write(d.join("run.json"), base.to_string()).unwrap();
    d
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-12)
}

// ── status ─────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_status_up() {
    let env = ApEnv::new().await;
    let (a, run) = env.make(None, &Opts::default());
    let st = a.status().await.unwrap();
    assert!(
        st.system.as_str() == "aiperf"
            && st.health == Health::Up
            && st.version.as_deref() == Some("0.13.0")
            && st.problems.is_empty()
    );
    assert!(st.summary.starts_with("3 preset(s)"), "{}", st.summary);
    assert_eq!(
        st.endpoints
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["router (load target)"]
    );
    assert_eq!(st.endpoints[0].auth, "bearer");
    let names: HashSet<String> = st.components.iter().map(|c| c.name.clone()).collect();
    assert!(names.contains(LAUNCHER) && names.contains(&format!("router {}", env.router.uri())));
    assert_eq!(
        run.argvs(),
        vec![vec![LAUNCHER.to_owned(), "--version".into()]]
    );
    a.status().await.unwrap(); // the version probe is TTL-cached
    assert_eq!(run.count(&[LAUNCHER, "--version"]), 1);
}

#[tokio::test]
async fn test_status_absent_launcher_is_down_with_a_fix() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(Some(FakeRunner::new()), &Opts::default()); // every argv -> rc 127
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Down && codes(&st).contains("ap.not_installed"));
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ap.not_installed")
        .unwrap();
    assert!(
        p.severity == Severity::Error
            && p.fix.clone().unwrap().contains("clients/aiperf/install.sh")
    );
    assert!(
        !st.components
            .iter()
            .any(|c| c.state == "running" && c.name == LAUNCHER)
    );
}

#[tokio::test]
async fn test_status_install_fix_uses_the_repo_path_when_present() {
    let env = ApEnv::new().await;
    let repo = env.tmp().join("repo");
    std::fs::create_dir_all(repo.join("clients").join("aiperf")).unwrap();
    std::fs::write(repo.join("clients/aiperf/install.sh"), "#!/bin/sh\n").unwrap();
    let (a, _) = env.make(
        Some(FakeRunner::new()),
        &Opts {
            repo: Some(repo.clone()),
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ap.not_installed")
        .unwrap();
    assert!(
        p.fix
            .clone()
            .unwrap()
            .starts_with(&repo.join("clients/aiperf/install.sh").display().to_string())
    );
}

#[tokio::test]
async fn test_status_router_down_degrades() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(
        None,
        &Opts {
            router_down: true,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Degraded && codes(&st).contains("ap.router_down"));
    assert!(
        st.components
            .iter()
            .any(|c| c.kind == "http" && c.state == "stopped")
    );
}

#[tokio::test]
async fn test_status_missing_key_warns_with_mint_key_fix() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(
        None,
        &Opts {
            key_file: false,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    let p = st.problems.iter().find(|p| p.code == "ap.no_key").unwrap();
    assert!(p.severity == Severity::Warn && p.fix.clone().unwrap().contains("mint-key.sh"));
}

#[tokio::test]
async fn test_status_never_reads_the_key_file() {
    use std::os::unix::fs::PermissionsExt;
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let key = env.key_path();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap(); // unreadable: existence only
    let st = a.status().await.unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!codes(&st).contains("ap.no_key"));
}

#[tokio::test]
async fn test_status_missing_tokenizer_warns_with_the_vendored_fix() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(
        None,
        &Opts {
            tokenizer: false,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ap.no_tokenizer")
        .unwrap();
    // The fix restores the vendored file and installs it: never a download.
    let fix = p.fix.clone().unwrap();
    assert!(
        st.health == Health::Degraded
            && fix.contains("install-tokenizer")
            && !fix.contains("prefetch")
    );
}

#[tokio::test]
async fn test_status_vendored_tokenizer_alone_is_ready() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(
        None,
        &Opts {
            tokenizer: false,
            ..Opts::default()
        },
    );
    let vendor = env.aiperf_dir().join("vendor");
    std::fs::create_dir_all(&vendor).unwrap();
    std::fs::write(vendor.join("o200k_base.tiktoken"), b"x").unwrap();
    assert!(!codes(&a.status().await.unwrap()).contains("ap.no_tokenizer"));
}

#[tokio::test]
async fn test_status_profiles_missing_and_invalid() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let prof = env.aiperf_dir().join("profiles.yaml");
    std::fs::remove_file(&prof).unwrap();
    assert!(codes(&a.status().await.unwrap()).contains("ap.no_profiles"));
    std::fs::write(&prof, "version: 1\nprofiles:\n  smoke:\n    model: m\n").unwrap();
    let st = a.status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "ap.profiles_invalid")
        .unwrap();
    assert!(
        p.message.contains("'concurrency' is required") && p.message.contains("line"),
        "{}",
        p.message
    );
    std::fs::write(&prof, "profiles: [unclosed\n").unwrap();
    assert!(codes(&a.status().await.unwrap()).contains("ap.profiles_invalid")); // a broken file is data
}

#[tokio::test]
async fn test_status_summary_shows_the_newest_run() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    seed_run(
        &a,
        "20261024T100000-001-smoke-lightning",
        json!({"metrics": {"ttft_p50_ms": 29.96, "output_tps": 1533.0}}),
    );
    let st = a.status().await.unwrap();
    assert!(
        st.summary.contains("last run: smoke-lightning")
            && st.summary.contains("TTFT p50 30 ms")
            && st.summary.contains("1533 output tok/s")
    );
}

// ── metrics ────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_metrics_without_runs_has_only_the_count() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let snap = a.metrics().await.unwrap();
    assert!(snap.available);
    assert_eq!(
        snap.points
            .iter()
            .map(|p| (p.key.as_str(), p.value))
            .collect::<Vec<_>>(),
        [("ap.runs", 0.0)]
    );
}

#[tokio::test]
async fn test_metrics_from_the_captured_summary() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let doc: Value = serde_json::from_str(&fx("aiperf", "profile_export_aiperf.json")).unwrap();
    let m = summarize(doc.as_object().unwrap());
    assert!(
        approx(m["ttft_p50_ms"], 29.963_736_5) && approx(m["ttft_p99_ms"], 40.695_334_159_999_994)
    );
    assert!(
        approx(m["itl_p50_ms"], 0.791_200_833_333_333_2)
            && approx(m["output_tps"], 1_532.991_017_636_744_1)
    );
    assert!(
        approx(m["request_tps"], 23.952_984_650_574_127)
            && m["requests"] == 8.0
            && m["error_rate_pct"] == 0.0
    );
    seed_run(
        &a,
        "20261024T100000-001-smoke-lightning",
        json!({"metrics": m}),
    );
    seed_run(
        &a,
        "20261024T110000-001-smoke-lightning",
        json!({"verdict": "failed", "error": "boom"}),
    ); // newer, not usable
    let snap = a.metrics().await.unwrap();
    let t = pts(&snap, "ap.ttft_ms");
    assert!(
        t.len() == 2 && approx(t["p50"], 29.963_736_5) && approx(t["p99"], 40.695_334_159_999_994)
    );
    assert!(approx(
        pts(&snap, "ap.itl_ms")["p50"],
        0.791_200_833_333_333_2
    ));
    assert!(approx(
        pts(&snap, "ap.output_tps")[""],
        1_532.991_017_636_744_1
    ));
    assert!(approx(
        pts(&snap, "ap.request_tps")[""],
        23.952_984_650_574_127
    ));
    assert_eq!(pts(&snap, "ap.runs"), HashMap::from([(String::new(), 2.0)]));
    let by_verdict = pts(&snap, "ap.runs_by_verdict");
    assert_eq!(
        by_verdict,
        HashMap::from([
            ("succeeded".into(), 1.0),
            ("failed".into(), 1.0),
            ("error".into(), 0.0),
            ("interrupted".into(), 0.0)
        ])
    );
    let units: HashSet<&str> = snap
        .points
        .iter()
        .filter(|p| p.key == "ap.output_tps")
        .map(|p| p.unit.as_str())
        .collect();
    assert_eq!(units, HashSet::from(["tok/s"]));
}

#[tokio::test]
async fn test_metrics_tolerates_missing_keys() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    seed_run(
        &a,
        "20261024T100000-001-smoke-lightning",
        json!({"metrics": {"ttft_p50_ms": 12.0}}),
    ); // no p99, itl, tps
    let snap = a.metrics().await.unwrap();
    assert_eq!(
        pts(&snap, "ap.ttft_ms"),
        HashMap::from([("p50".into(), 12.0)])
    );
    assert!(
        !snap
            .points
            .iter()
            .any(|p| ["ap.itl_ms", "ap.output_tps", "ap.request_tps"].contains(&p.key.as_str()))
    );
    assert!(summarize(json!({}).as_object().unwrap()).is_empty());
    assert!(
        summarize(
            json!({"time_to_first_token": "garbage", "request_count": {"avg": true}})
                .as_object()
                .unwrap()
        )
        .is_empty()
    );
}

#[tokio::test]
async fn test_summarize_never_fabricates_non_numbers() {
    let doc = json!({"time_to_first_token": {"p50": "fast", "p99": null}, "output_token_throughput": {"avg": 5}});
    let m = summarize(doc.as_object().unwrap());
    assert_eq!(
        m.into_iter().collect::<Vec<_>>(),
        [("output_tps".to_owned(), 5.0)]
    );
}

// ── events ─────────────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_events_are_metadata_only() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    seed_run(
        &a,
        "20261024T100000-001-smoke-lightning",
        json!({"requests": 8, "isl": 128, "osl": 64, "duration_ms": 900, "metrics": {"ttft_p50_ms": 30.0, "output_tps": 1500.0},
               "error": "sk-abcdefghijklmnop0123 leaked?"}),
    );
    let evs = a.recent_events(100).await.unwrap();
    assert_eq!(evs.len(), 1);
    let ev = &evs[0];
    assert!(ev.kind == "aiperf.run" && ev.system.as_str() == "aiperf" && ev.ts == 1_700_000_000.0);
    assert!(
        ev.data["preset"] == "smoke-lightning"
            && ev.data["ttft_p50_ms"] == 30.0
            && ev.data["requests"] == 8
    );
    let blob = serde_json::to_string(ev).unwrap();
    assert!(
        !ev.data.contains_key("error") && !ev.data.contains_key("metrics") && !blob.contains("sk-")
    );
}

#[tokio::test]
async fn test_follow_events_announces_each_run_state_once() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let got = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let g = Arc::clone(&got);
    let a2 = Arc::clone(&a);
    let task = tokio::spawn(async move {
        let mut s = a2.follow_events(Some(0.0));
        while let Some(Ok(ev)) = s.next().await {
            g.lock()
                .unwrap()
                .push(ev.data["verdict"].as_str().unwrap_or("").to_owned());
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let d = seed_run(
        &a,
        "20261024T100000-001-smoke-lightning",
        json!({"verdict": "running", "ts": 9e9}),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let done = json!({"run_id": d.file_name().unwrap().to_str().unwrap(), "preset": "smoke-lightning", "ts": 9e9, "verdict": "succeeded"});
    std::fs::write(d.join("run.json"), done.to_string()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    // a stored "running" with no live task reads as interrupted
    assert_eq!(*got.lock().unwrap(), ["interrupted", "succeeded"]);
}

#[tokio::test]
async fn test_a_run_left_running_by_a_dead_process_reads_as_interrupted() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    seed_run(
        &a,
        "20261024T100000-001-smoke-lightning",
        json!({"verdict": "running"}),
    );
    let evs = a.recent_events(100).await.unwrap();
    assert!(evs.len() == 1 && evs[0].data["verdict"] == "interrupted");
}

// ── config: profiles.yaml ──────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_config_file_surface() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let specs = a.config_files().unwrap();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert!(spec.id == "profiles" && spec.editable && spec.authoritative && spec.format == "yaml");
    assert_eq!(
        a.config_path("profiles").unwrap(),
        env.aiperf_dir().join("profiles.yaml")
    );
    assert!(matches!(
        a.config_path("other"),
        Err(AdapterError::UnknownId(_))
    ));
    let cf = a.read_config("profiles").await.unwrap();
    assert!(cf.content == fx("aiperf", "profiles.yaml") && cf.sha256.len() == 64);
    let res = a.post_apply("profiles", false).await.unwrap();
    assert!(res.ok && res.post_check == PostCheck::Healthy && !res.restarted);
    assert!(matches!(
        a.post_apply("nope", false).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_config_path_refuses_a_symlink() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let real = env.tmp().join("elsewhere.yaml");
    std::fs::write(&real, "version: 1\n").unwrap();
    let p = env.aiperf_dir().join("profiles.yaml");
    std::fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink(&real, &p).unwrap();
    assert!(matches!(
        a.config_path("profiles"),
        Err(AdapterError::UnknownId(_))
    ));
    let acts = a.actions().unwrap();
    assert!(
        acts.iter().find(|x| x.id == "profile").unwrap().params[0]
            .choices
            .is_none()
    );
}

#[test]
fn test_the_shipped_profiles_validate_clean() {
    let (presets, diags) = parse_profiles(&fx("aiperf", "profiles.yaml")).unwrap();
    assert!(diags.is_empty());
    let mut names: Vec<&String> = presets.keys().collect();
    names.sort();
    assert_eq!(
        names,
        [
            "latency-lightning",
            "smoke-lightning",
            "throughput-lightning"
        ]
    );
    let s = &presets["smoke-lightning"];
    assert_eq!(
        (
            s.model.as_str(),
            s.concurrency,
            s.requests,
            s.warmup_requests,
            s.isl,
            s.osl
        ),
        ("aitop/nemotron-lightning", 2, 8, 1, 128, 64)
    );
    assert!(
        !presets
            .values()
            .any(|p| p.model.to_lowercase().starts_with("glm"))
    ); // the main model is never a preset
}

fn errs(text: &str) -> Vec<String> {
    parse_profiles(text)
        .unwrap()
        .1
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect()
}

const MINIMAL: &str = "version: 1\nprofiles:\n  p1:\n    model: m\n    concurrency: 1\n    requests: 2\n    isl: 8\n    osl: 8\n";

#[test]
fn test_validator_rejects() {
    let m = MINIMAL;
    let cases: Vec<(String, &str)> = vec![
        ("profiles: [unclosed\n".into(), "YAML syntax"),
        ("- a\n- b\n".into(), "must be a mapping"),
        ("version: 2\nprofiles: {}\n".into(), "'version' must be 1"),
        (
            "version: true\nprofiles: {}\n".into(),
            "'version' must be 1",
        ),
        ("version: 1\n".into(), "at least one preset"),
        ("version: 1\nprofiles: {}\n".into(), "at least one preset"),
        (
            "version: 1\nprofils: {}\nprofiles: {a: {}}\n".into(),
            "unknown top-level key 'profils' (did you mean 'profiles'?)",
        ),
        (m.replace("p1:", "Bad_Name:"), "must match"),
        (m.replace("p1:", "p1: 5\n  p2:"), "must be a mapping"),
        (
            m.replace("concurrency: 1", "concurrency: 0"),
            "'concurrency' must be an integer from 1 to 64",
        ),
        (
            m.replace("concurrency: 1", "concurrency: 65"),
            "'concurrency' must be an integer from 1 to 64",
        ),
        (
            m.replace("concurrency: 1", "concurrency: true"),
            "'concurrency' must be an integer",
        ),
        (
            m.replace("concurrency: 1", "concurrency: '2'"),
            "'concurrency' must be an integer",
        ),
        (
            m.replace("concurrency: 1", "concurrency: 1.5"),
            "'concurrency' must be an integer",
        ),
        (
            m.replace("requests: 2", "requests: 2001"),
            "'requests' must be an integer from 1 to 2000",
        ),
        (
            m.replace("isl: 8", "isl: 40000"),
            "'isl' must be an integer from 1 to 32768",
        ),
        (
            m.replace("osl: 8", "osl: 5000"),
            "'osl' must be an integer from 1 to 4096",
        ),
        (m.replace("    osl: 8\n", ""), "'osl' is required"),
        (m.replace("    model: m\n", ""), "'model' is required"),
        (
            m.replace("model: m", "model: 'a b; rm -rf /'"),
            "'model' is required",
        ),
        (
            format!("{m}    endpoint_type: embeddings\n"),
            "'endpoint_type' must be one of chat, completions",
        ),
        (
            format!("{m}    streaming: yes please\n"),
            "'streaming' must be true or false",
        ),
        (
            format!("{m}    concurency: 3\n"),
            "unknown key 'concurency' (did you mean 'concurrency'?)",
        ),
        (
            format!("{m}    warmup_requests: 101\n"),
            "'warmup_requests' must be an integer from 0 to 100",
        ),
        (
            format!("{m}    timeout_s: 0\n"),
            "'timeout_s' must be an integer from 1 to 600",
        ),
        (
            format!("{m}    seed: -1\n"),
            "'seed' must be an integer from 0 to 2147483647",
        ),
        (
            format!("{m}    description: |\n      two\n      lines\n"),
            "'description' must be a single line",
        ),
    ];
    for (text, needle) in cases {
        let got = errs(&text);
        assert!(got.iter().any(|m| m.contains(needle)), "{text:?}: {got:?}");
    }
}

#[test]
fn test_validator_warns_but_does_not_fail() {
    let text = MINIMAL
        .replace("requests: 2", "requests: 1")
        .replace("concurrency: 1", "concurrency: 4")
        + "    streaming: false\n";
    let (presets, diags) = parse_profiles(&text).unwrap();
    assert!(presets.contains_key("p1"));
    assert!(diags.iter().all(|d| d.severity == Severity::Warn) && !diags.is_empty());
    assert!(
        diags.iter().any(|d| d.message.contains("not measured"))
            && diags.iter().any(|d| d.message.contains("fewer requests"))
    );
}

#[test]
fn test_validator_reports_line_numbers() {
    let text = MINIMAL.replace("isl: 8", "isl: 0");
    let errors: Vec<_> = parse_profiles(&text)
        .unwrap()
        .1
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1);
    let want = text.lines().position(|l| l == "    isl: 0").unwrap() + 1;
    assert_eq!(errors[0].line, Some(i64::try_from(want).unwrap()));
    let d2 = parse_profiles("profiles: [unclosed\n").unwrap().1;
    assert!(d2.len() == 1 && d2[0].line.is_some());
}

#[test]
fn test_validator_drops_only_the_bad_preset() {
    let text = format!(
        "{MINIMAL}  p2:\n    model: m\n    concurrency: 0\n    requests: 1\n    isl: 1\n    osl: 1\n"
    );
    let (presets, diags) = parse_profiles(&text).unwrap();
    assert_eq!(presets.keys().collect::<Vec<_>>(), ["p1"]);
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count(),
        1
    );
}

#[test]
fn test_validator_caps_size_and_count() {
    assert!(!errs(&format!("x: {}", "a".repeat(300_000))).is_empty());
    let many: String = std::iter::once("version: 1\nprofiles:\n".to_owned())
        .chain((0..40).map(|i| format!("  p{i}:\n    model: m\n    concurrency: 1\n    requests: 1\n    isl: 1\n    osl: 1\n")))
        .collect();
    assert!(errs(&many).iter().any(|m| m.contains("at most 32")));
}

#[tokio::test]
async fn test_validate_config_is_authoritative() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let good = a
        .validate_config("profiles", &fx("aiperf", "profiles.yaml"))
        .await
        .unwrap();
    assert!(
        good.ok
            && good.authoritative
            && good.validator == "nemo-console aiperf profiles schema"
            && good.note.is_some()
    );
    let bad = a
        .validate_config("profiles", &MINIMAL.replace("isl: 8", "isl: 0"))
        .await
        .unwrap();
    assert!(!bad.ok && bad.diagnostics[0].severity == Severity::Error);
    assert!(matches!(
        a.validate_config("other", "").await,
        Err(AdapterError::UnknownId(_))
    ));
}

// ── actions listing + the runner contract ─────────────────────────────────────────────────────
#[tokio::test]
async fn test_actions_come_from_the_profiles_file() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let acts = a.actions().unwrap();
    let (prof, rep) = (&acts[0], &acts[1]);
    assert!(prof.id == "profile" && prof.danger == Danger::Disruptive && prof.role == Role::Admin);
    assert!(prof.params[0].type_ == ParamType::Enum);
    assert_eq!(
        prof.params[0].choices,
        Some(vec![
            "latency-lightning".into(),
            "smoke-lightning".into(),
            "throughput-lightning".into()
        ])
    );
    assert!(
        prof.params[0].default == json!("latency-lightning") && !prof.params[0].requires_restart
    );
    assert!(
        rep.id == "report"
            && rep.danger == Danger::Safe
            && rep.params[0].type_ == ParamType::String
    );
    std::fs::write(
        env.aiperf_dir().join("profiles.yaml"),
        fx("aiperf", "profiles.yaml") + "  extra-one:\n    model: m\n    concurrency: 1\n    requests: 1\n    isl: 1\n    osl: 1\n",
    )
    .unwrap();
    assert!(
        a.actions().unwrap()[0].params[0]
            .choices
            .clone()
            .unwrap()
            .contains(&"extra-one".to_owned())
    );
}

#[test]
fn test_render_argv_golden_and_runner_contract() {
    let (presets, _) = parse_profiles(&fx("aiperf", "profiles.yaml")).unwrap();
    let art = Path::new("/state/aiperf/runs/R/artifacts");
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
        "/state/aiperf/runs/R/artifacts",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert_eq!(
        render_argv(LAUNCHER, &presets["smoke-lightning"], art),
        want
    );
    assert_eq!(
        render_argv(LAUNCHER, &presets["latency-lightning"], art)[..7],
        [
            "aiperf-local",
            "profile",
            "--model",
            "aitop/nemotron-lightning",
            "--endpoint-type",
            "chat",
            "--streaming"
        ]
    );
    let mut p = presets["smoke-lightning"].clone();
    p.streaming = false;
    p.warmup_requests = 0;
    let no_stream = render_argv(LAUNCHER, &p, art);
    assert!(
        !no_stream.contains(&"--streaming".to_owned())
            && !no_stream.contains(&"--warmup-request-count".to_owned())
    );
    for p in presets.values() {
        let argv = render_argv(LAUNCHER, p, art);
        assert!(
            max_timeout(&argv) == AIPERF_PROFILE
                && AIPERF_PROFILE == 3600.0
                && AIPERF_BINARIES.contains(&LAUNCHER)
        );
        assert_eq!(
            max_timeout(&[LAUNCHER.to_owned(), "--version".into()]),
            600.0
        );
        // nothing secret can be in this argv: no credential flag, no header, no URL (the wrapper owns all three)
        for f in ["--api-key", "-H", "--header", "--url", "-u", "--config"] {
            assert!(!argv.contains(&f.to_owned()));
        }
        assert!(!argv.iter().any(|a| a.starts_with("sk-")));
    }
}

#[tokio::test]
async fn test_the_adapter_is_registered_and_labelled() {
    let env = ApEnv::new().await;
    let (a, _) = env.make(None, &Opts::default());
    let settings = nemo_console::config::settings_for_test(
        json!({"state_dir": env.tmp().join("st").to_string_lossy()}),
    );
    let built = nemo_console::adapters::build_adapter(
        nemo_console::domain::models::SystemId::Aiperf,
        a.ctx(),
        &settings,
    )
    .unwrap();
    assert!(built.id().as_str() == "aiperf" && built.label() == "AIPerf");
}
