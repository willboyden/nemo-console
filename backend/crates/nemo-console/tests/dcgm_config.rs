//! Port of backend/tests/dcgm/test_config.py: counters.csv read, validate (field/type check, honest
//! authoritative=False), post_apply health + rollback signal.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/dcgm/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::HashSet;
use std::sync::Arc;

use env::{Env, is_logs, is_restart};
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::dcgm::{
    DcgmAdapter, DcgmTiming, KNOWN_FIELDS, known_field, parse_counters,
};
use nemo_console::domain::models::{PostCheck, Severity, ValidationResult};
use support::{fixed, fixtures, fx, ok, rc};

fn msgs(res: &ValidationResult) -> Vec<String> {
    res.diagnostics
        .iter()
        .map(|d| {
            let sev = match d.severity {
                Severity::Error => "error",
                Severity::Warn => "warn",
                Severity::Info => "info",
            };
            format!(
                "{sev}:{}:{}",
                d.line.map_or("None".to_owned(), |l| l.to_string()),
                d.message
            )
        })
        .collect()
}

#[test]
fn test_known_fields_cover_the_live_counters_file() {
    let (rows, diags) = parse_counters(&fx("dcgm", "counters.good.csv"));
    assert!(diags.is_empty() && rows.len() > 20);
    assert!(rows.iter().all(|(_, f, _)| known_field(f).is_some()));
    assert!(
        known_field("DCGM_FI_DRIVER_VERSION") == Some('l')
            && known_field("DCGM_FI_DEV_GPU_TEMP") == Some('g')
    );
    assert_eq!(KNOWN_FIELDS.len(), 86);
    let unique: HashSet<&str> = KNOWN_FIELDS.iter().map(|(f, _)| *f).collect();
    assert_eq!(unique.len(), 86);
}

#[tokio::test]
async fn test_spec_and_read() {
    let env = Env::new().await;
    let a = env.adapter();
    let specs = a.config_files().unwrap();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert!(spec.id == "counters" && spec.editable && spec.format == "text" && !spec.authoritative);
    assert!(!spec.validators.is_empty() && spec.validators[0].contains("86"));
    let cf = a.read_config("counters").await.unwrap();
    assert!(
        cf.content == std::fs::read_to_string(&env.counters).unwrap()
            && cf.sha256.len() == 64
            && cf.mtime > 0.0
    );
    assert_eq!(a.config_path("counters").unwrap(), env.counters);
}

fn adapter_with_counters(env: &Env, counters: &std::path::Path) -> DcgmAdapter {
    let ctx = support::ctx(
        Arc::clone(&env.runner),
        &[("dcgm", &env.url()), ("prometheus", &env.url())],
        &[("dcgm_counters", counters)],
        &env.state(),
    );
    DcgmAdapter::new(ctx)
}

#[tokio::test]
async fn test_config_path_rejects_unknown_symlink_and_missing() {
    let env = Env::new().await;
    let a = env.adapter();
    for bad_id in ["routes", "../counters", "counters.csv", ""] {
        assert!(matches!(
            a.config_path(bad_id),
            Err(AdapterError::UnknownId(_))
        ));
    }
    let link = env.tmp.path().join("link.csv");
    std::os::unix::fs::symlink(&env.counters, &link).unwrap();
    assert!(matches!(
        adapter_with_counters(&env, &link).config_path("counters"),
        Err(AdapterError::UnknownId(_))
    ));
    std::fs::remove_file(&env.counters).unwrap();
    assert!(matches!(
        adapter_with_counters(&env, &env.counters).config_path("counters"),
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_validate_live_file_ok_not_authoritative() {
    let env = Env::new().await;
    let res = env
        .adapter()
        .validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    assert!(res.ok && !res.authoritative && res.diagnostics.is_empty());
    let note = res.note.unwrap();
    assert!(note.contains("proxy") && note.contains("image"));
}

#[tokio::test]
async fn test_validate_broken_candidate() {
    let env = Env::new().await;
    let bad_csv = "# comment
DCGM_FI_DEV_GPU_TMP, gauge, typo of a real field
DCGM_FI_DEV_SM_CLOCK, histogram, wrong type
DCGM_FI_DEV_MEM_CLOCK, gauge
DCGM_FI_DEV_POWER_USAGE, gauge, ok
DCGM_FI_DEV_POWER_USAGE, gauge, duplicate
dcgm_gpu_temp, gauge, 1.x name
DCGM_FI_DEV_SERIAL, label, serial
DCGM_FI_DEV_QUITE_NEW_THING_XYZ, gauge, maybe a newer field
DCGM_FI_DEV_FB_USED, label, wrong kind
not a field, gauge, x
"; // line numbers asserted below: typo=2 type=3 cols=4 dup=6 1.x=7 serial=8 new=9 kind=10 name=11
    let res = env
        .adapter()
        .validate_config("counters", bad_csv)
        .await
        .unwrap();
    let m = msgs(&res);
    assert!(!res.ok && !res.authoritative);
    let any = |p: &str, needle: &str| m.iter().any(|x| x.starts_with(p) && x.contains(needle));
    assert!(any("error:2:", "DCGM_FI_DEV_GPU_TEMP"));
    assert!(any("error:3:", "gauge, counter, label"));
    assert!(any("error:4:", "three"));
    assert!(any("error:6:", "line 5"));
    assert!(any("error:7:", "1.x"));
    assert!(any("warn:8:", "serial"));
    assert!(any("warn:9:", ""));
    assert!(any("warn:10:", "label"));
    assert!(any("error:11:", ""));
}

#[tokio::test]
async fn test_validate_needs_an_active_metric_and_caps_size() {
    let env = Env::new().await;
    let a = env.adapter();
    let res = a
        .validate_config(
            "counters",
            "# only comments\nDCGM_FI_DRIVER_VERSION, label, v\n",
        )
        .await
        .unwrap();
    assert!(
        !res.ok
            && res
                .diagnostics
                .iter()
                .any(|d| d.message.contains("no active gauge/counter"))
    );
    let res = a
        .validate_config("counters", &"#".repeat(300 * 1024))
        .await
        .unwrap();
    assert!(!res.ok && res.diagnostics[0].message.contains("256 KiB"));
    assert!(matches!(
        a.validate_config("other", "x").await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_validate_never_writes_the_live_file() {
    let env = Env::new().await;
    let before = std::fs::read(&env.counters).unwrap();
    env.adapter()
        .validate_config("counters", "garbage")
        .await
        .unwrap();
    assert_eq!(std::fs::read(&env.counters).unwrap(), before);
}

// ── post_apply ──────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_post_apply_healthy_no_restart() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap(); // freezes the baseline like a real apply
    let res = a.post_apply("counters", false).await.unwrap();
    assert!(res.ok && res.post_check == PostCheck::Healthy && !res.restarted);
    assert!(env.runner.called(&["docker", "restart"]).is_empty());
}

#[tokio::test]
async fn test_post_apply_restart_calls_docker_restart_and_polls() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    // Python answers ConnectError twice; here the exporter answers 503 twice (the adapter sees "not answering" alike)
    env.metrics.queue(&[(503, "starting"), (503, "starting")]);
    let res = a.post_apply("counters", true).await.unwrap();
    assert!(
        res.ok && res.restarted && res.post_check == PostCheck::Healthy,
        "{}",
        res.detail
    );
    assert_eq!(
        env.runner.called(&["docker", "restart"]),
        vec![vec![
            "docker".to_owned(),
            "restart".into(),
            "ai-lab-observability-dcgm-exporter-1".into()
        ]]
    );
}

#[tokio::test]
async fn test_post_apply_unhealthy_when_metrics_stop_answering() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    env.metrics.set(500, "boom");
    let res = a.post_apply("counters", false).await.unwrap();
    assert!(
        !res.ok && res.post_check == PostCheck::Unhealthy && res.detail.contains("did not answer")
    );
}

fn without(text: &str, needle: &str) -> String {
    support_lines(text)
        .into_iter()
        .filter(|l| !l.contains(needle))
        .collect()
}

fn support_lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_owned).collect()
}

#[tokio::test]
async fn test_post_apply_unhealthy_when_a_served_family_vanishes() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    // exporter now serves no temperature although the file still configures it
    env.metrics.set(
        200,
        &without(&fx("dcgm", "metrics.txt"), "DCGM_FI_DEV_GPU_TEMP"),
    );
    let res = a.post_apply("counters", false).await.unwrap();
    assert!(res.post_check == PostCheck::Unhealthy && res.detail.contains("DCGM_FI_DEV_GPU_TEMP"));
}

fn drop_temp_from_file(env: &Env) {
    let text = std::fs::read_to_string(&env.counters).unwrap();
    let changed = text.replace(
        "DCGM_FI_DEV_GPU_TEMP,    gauge, GPU temperature (in C).",
        "# DCGM_FI_DEV_GPU_TEMP, gauge, off",
    );
    assert_ne!(changed, text);
    std::fs::write(&env.counters, changed).unwrap();
}

#[tokio::test]
async fn test_intentionally_removed_family_is_not_required() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    drop_temp_from_file(&env);
    env.metrics.set(
        200,
        &without(&fx("dcgm", "metrics.txt"), "DCGM_FI_DEV_GPU_TEMP"),
    );
    let res = a.post_apply("counters", false).await.unwrap();
    assert!(
        res.ok && res.post_check == PostCheck::Healthy,
        "{}",
        res.detail
    );
}

#[tokio::test]
async fn test_no_reload_detected_without_restart() {
    // The file drops a field but the exporter keeps serving it -> it did not re-read -> unhealthy (rollback).
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    drop_temp_from_file(&env);
    let res = a.post_apply("counters", false).await.unwrap();
    assert!(
        res.post_check == PostCheck::Unhealthy
            && res.detail.contains("did not re-read")
            && res.detail.contains("DCGM_FI_DEV_GPU_TEMP")
    );
}

#[tokio::test]
async fn test_logged_errors_make_it_unhealthy() {
    let env = Env::new().await;
    env.runner.insert_first(
        is_logs,
        fixed(ok("time=2026-10-24T11:20:00.000Z level=ERROR msg=\"Failed to reload counters\" error=\"bad field\"\n")),
    );
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    let res = a.post_apply("counters", false).await.unwrap();
    assert!(res.post_check == PostCheck::Unhealthy && res.detail.contains("Failed to reload"));
}

#[tokio::test]
async fn test_restart_failure_is_unhealthy() {
    let env = Env::new().await;
    env.runner.insert_first(
        is_restart,
        fixed(rc(1, "", "Error response from daemon: no such container")),
    );
    let res = env.adapter().post_apply("counters", true).await.unwrap();
    assert!(res.post_check == PostCheck::Unhealthy && !res.restarted);
}

#[tokio::test]
async fn test_prof_families_late_are_a_note_not_a_failure() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap(); // baseline includes DCGM_FI_PROF_*
    env.metrics
        .set(200, &without(&fx("dcgm", "metrics.txt"), "DCGM_FI_PROF_"));
    let res = a.post_apply("counters", true).await.unwrap();
    assert!(
        res.ok && res.detail.contains("DCGM_FI_PROF_"),
        "{}",
        res.detail
    );
}

#[tokio::test]
async fn test_baseline_is_frozen_between_validate_and_apply() {
    let env = Env::new().await;
    let a = env.adapter();
    a.validate_config("counters", &fx("dcgm", "counters.good.csv"))
        .await
        .unwrap();
    env.metrics.set(200, "DCGM_FI_DEV_FB_USED{gpu=\"0\"} 1\n"); // broken exporter state
    a.metrics().await.unwrap(); // a UI poll during the apply window
    assert!(a.baseline().contains("DCGM_FI_DEV_GPU_TEMP")); // not shrunk to the broken state
    let res = a.post_apply("counters", false).await.unwrap();
    assert_eq!(res.post_check, PostCheck::Unhealthy);
    let _ = DcgmTiming::default();
}

#[test]
fn test_fixture_dir_has_no_serial_or_real_uuid() {
    let uuid = fancy_regex::Regex::new(r"GPU-(?!0{8}-0000-0000-0000-00000000000[01])[0-9a-f]{8}-")
        .unwrap();
    let serial = fancy_regex::Regex::new(r#""serial_num": "(?!REDACTED)"#).unwrap();
    for e in std::fs::read_dir(fixtures("dcgm")).unwrap() {
        let p = e.unwrap().path();
        if !p.is_file() || p.extension().is_some_and(|x| x == "rs") {
            continue; // the Rust support files live next to the copied fixtures
        }
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(!uuid.is_match(&t).unwrap(), "{}", p.display());
        assert!(!serial.is_match(&t).unwrap(), "{}", p.display());
    }
}
