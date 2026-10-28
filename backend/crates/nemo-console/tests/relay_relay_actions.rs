//! Port of backend/tests/relay/test_relay_actions.py.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod relay_support;
mod switchyard_support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use nemo_console::adapters::base::{AdapterError, CommandResult, SystemAdapter};
use nemo_console::domain::models::{Danger, PostCheck};
use relay_support::{Router, fx, make_adapter, okr, standard_runner};
use serde_json::{Map, Value};
use switchyard_support::{PrefixRunner, Reply, rc};

fn restart() -> Vec<String> {
    [
        "systemctl",
        "--user",
        "restart",
        "nemo-relay-gateway.service",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

fn show(pid: u64, restarts: u64) -> String {
    format!(
        "ActiveEnterTimestamp=@1790223677\nActiveState=active\nSubState=running\nMainPID={pid}\nNRestarts={restarts}\n\
         Result=success\nLoadState=loaded\nUnitFileState=enabled\n"
    )
}

fn none() -> Map<String, Value> {
    Map::new()
}

#[tokio::test]
async fn test_actions_and_danger_levels() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let got: BTreeMap<String, Danger> =
        m.a.actions()
            .unwrap()
            .into_iter()
            .map(|x| (x.id, x.danger))
            .collect();
    let want: BTreeMap<String, Danger> = [
        ("gateway_restart", Danger::Disruptive),
        ("gateway_status", Danger::Safe),
        ("doctor_offline", Danger::Safe),
        ("agents_list", Danger::Safe),
        ("plugins_validate", Danger::Safe),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn test_post_apply_restarts_and_waits_for_settled_health() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let res = m.a.post_apply("routes", true).await.unwrap();
    assert!(res.ok && res.restarted && res.post_check == PostCheck::Healthy);
    let calls = m.run.calls();
    assert!(calls.contains(&restart()));
    let shows = calls
        .iter()
        .filter(|c| c[..4] == ["systemctl", "--user", "--timestamp=unix", "show"])
        .count();
    assert!(shows >= m.a.settle_checks as usize);
}

#[tokio::test]
async fn test_post_apply_without_restart_is_skipped() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let res = m.a.post_apply("routes", false).await.unwrap();
    assert!(res.ok && !res.restarted && res.post_check == PostCheck::Skipped);
    assert!(!m.run.calls().contains(&restart()));
}

#[tokio::test]
async fn test_post_apply_readonly_file_refused() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let res = m.a.post_apply("plugins", true).await.unwrap();
    assert!(!res.ok && !m.run.calls().contains(&restart()));
}

#[tokio::test]
async fn test_post_apply_unhealthy_when_gateway_never_answers() {
    let r = Router::new().await;
    r.gateway.get("/health", Reply::Close);
    let m = make_adapter(&r, None, None, None, |a| a.restart_timeout = 0.05);
    let res = m.a.post_apply("routes", true).await.unwrap();
    assert!(!res.ok && res.restarted && res.post_check == PostCheck::Unhealthy);
    assert!(res.detail.contains("not healthy"));
}

#[tokio::test]
async fn test_post_apply_detects_crash_loop_after_banner() {
    let r = Router::new().await;
    let n = Arc::new(Mutex::new(0u64));
    let run = standard_runner(None);
    run.on_fn(
        &["systemctl", "--user", "--timestamp=unix", "show"],
        move |_a| {
            let mut k = n.lock().unwrap();
            *k += 1;
            okr(&show(100 + *k, *k)) // pid + NRestarts move every poll
        },
    );
    let m = make_adapter(&r, Some(run), None, None, |a| a.restart_timeout = 0.05);
    let res = m.a.post_apply("routes", true).await.unwrap();
    assert!(res.post_check == PostCheck::Unhealthy && res.detail.contains("crash loop"));
}

#[tokio::test]
async fn test_post_apply_restart_command_failure() {
    let r = Router::new().await;
    let run = standard_runner(None);
    run.on(
        &[
            "systemctl",
            "--user",
            "restart",
            "nemo-relay-gateway.service",
        ],
        CommandResult::new(1, "", "Failed to restart"),
    );
    let m = make_adapter(&r, Some(run), None, None, |_| {});
    let res = m.a.post_apply("routes", true).await.unwrap();
    assert!(
        !res.ok
            && res.post_check == PostCheck::Unhealthy
            && res.detail.contains("Failed to restart")
    );
}

#[tokio::test]
async fn test_run_gateway_restart() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let res = m.a.run_action("gateway_restart", &none()).await.unwrap();
    assert!(res.ok && m.run.calls().contains(&restart()) && res.problems.is_empty());
}

#[tokio::test]
async fn test_run_gateway_restart_unhealthy_reports_problem() {
    let r = Router::new().await;
    r.gateway.get("/health", Reply::Close);
    let m = make_adapter(&r, None, None, None, |a| a.restart_timeout = 0.05);
    let res = m.a.run_action("gateway_restart", &none()).await.unwrap();
    assert!(
        !res.ok
            && res.problems[0].code == "relay.gateway.restart_unhealthy"
            && res.problems[0].fix.is_some()
    );
}

#[tokio::test]
async fn test_safe_actions() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let st = m.a.run_action("gateway_status", &none()).await.unwrap();
    assert!(st.ok && st.output.contains("nemo-relay-gateway.service"));
    let ag = m.a.run_action("agents_list", &none()).await.unwrap();
    assert!(
        ag.ok
            && ag.output.contains("claude: pass configured=True")
            && ag.output.contains("codex: info")
    );
    let dr = m.a.run_action("doctor_offline", &none()).await.unwrap();
    assert!(
        dr.ok
            && dr.output.contains("plugin host validation: pass")
            && dr.output.contains("nvidia.switchyard")
    );
    assert!(
        dr.output.contains("CLI's own environment")
            && dr.output.contains("PASS while the gateway still fails")
    );
    let pv = m.a.run_action("plugins_validate", &none()).await.unwrap();
    assert!(pv.ok && pv.output.contains("nvidia.switchyard: valid=True"));
    assert!(pv.output.contains("attestation=signature_required"));
    assert!(
        pv.output.contains("authoritative") || pv.output.contains("gateway still fails to start")
    );
    assert!(!m.run.calls().contains(&restart()));
}

#[tokio::test]
async fn test_gateway_status_inactive_rc3_is_still_an_answer() {
    let r = Router::new().await;
    let run = standard_runner(None);
    run.on(
        &["systemctl", "--user", "status"],
        rc(3, "Active: inactive (dead)\n", ""),
    );
    let m = make_adapter(&r, Some(run), None, None, |_| {});
    assert!(m.a.run_action("gateway_status", &none()).await.unwrap().ok);
}

#[tokio::test]
async fn test_plugins_validate_failure_and_no_plugins() {
    let r = Router::new().await;
    let run = standard_runner(None);
    run.on(
        &["nemo-relay", "plugins", "validate"],
        rc(2, &fx("plugins_validate_notfound.json"), ""),
    );
    let m = make_adapter(&r, Some(run), None, None, |_| {});
    assert!(
        !m.a.run_action("plugins_validate", &none())
            .await
            .unwrap()
            .ok
    );
    let m2 = make_adapter(&r, Some(PrefixRunner::new()), None, None, |_| {});
    let res = m2.a.run_action("plugins_validate", &none()).await.unwrap();
    assert!(!res.ok && res.output.contains("no registered plugins"));
}

#[tokio::test]
async fn test_unknown_action_is_keyerror() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    assert!(matches!(
        m.a.run_action("format_disk", &none()).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_playground() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let p = m.a.playground().unwrap().unwrap();
    assert_eq!(p.wire, "openai_chat");
    assert_eq!(p.models, ["auto-local", "direct-lightning", "direct-27b"]);
    assert_eq!(p.default_model, "auto-local");
    assert!(p.note.contains("401") && p.note.contains("env NAME"));
    assert_eq!(
        m.a.playground_url().unwrap(),
        format!("{}/v1/chat/completions", r.gateway.uri())
    );
}

#[tokio::test]
async fn test_playground_falls_back_when_routes_unreadable() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    std::fs::write(m.a.config_path("routes").unwrap(), "[[[ broken").unwrap();
    let p = m.a.playground().unwrap().unwrap();
    // mirrors clients/nemo-relay/routes.toml (2026-10-17); the default is the route LiteLLM's `switchyard` forwards to
    assert_eq!(
        p.models,
        [
            "super-lightning",
            "super-glm",
            "super-vl",
            "bolt",
            "direct-lightning",
            "auto-local",
            "direct-27b",
            "direct-flashnext"
        ]
    );
    assert_eq!(p.default_model, "super-lightning");
}

#[tokio::test]
async fn test_plugin_ids_starting_with_dash_are_never_passed_and_dashdash_precedes_ids() {
    let r = Router::new().await;
    let mut rows: Value = serde_json::from_str(&fx("plugins_list.json")).unwrap();
    let first = rows["data"][0].clone();
    let mut a = first.clone();
    a["id"] = Value::from("--help");
    let mut b = first;
    b["id"] = Value::from("-x");
    rows["data"].as_array_mut().unwrap().push(a);
    rows["data"].as_array_mut().unwrap().push(b);
    let run = standard_runner(None);
    run.on(
        &["nemo-relay", "plugins", "list", "--json"],
        okr(&rows.to_string()),
    );
    let m = make_adapter(&r, Some(run), None, None, |_| {});
    m.a.run_action("plugins_validate", &none()).await.unwrap();
    let validates: Vec<Vec<String>> = m
        .run
        .calls()
        .into_iter()
        .filter(|c| c.len() >= 3 && c[..3] == ["nemo-relay", "plugins", "validate"])
        .collect();
    assert_eq!(
        validates,
        vec![
            [
                "nemo-relay",
                "plugins",
                "validate",
                "--json",
                "--",
                "nvidia.switchyard"
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
        ]
    );
}
