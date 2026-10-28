//! Port of backend/tests/dynamo/test_adapter.py: adapter behaviour with a fake runner, a fake frontend and CAPTURED
//! real fixtures (frontend/worker inspect, metrics, logs).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod dynamo_support;
mod switchyard_support;

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dynamo_support::*;
use nemo_console::adapters::base::{AdapterError, RunnerError, SystemAdapter};
use nemo_console::adapters::dynamo::CardPolicy;
use nemo_console::config::DEFAULT_MODEL_START;
use nemo_console::domain::models::{
    Danger, Health, MetricPoint, MetricsSnapshot, PostCheck, Severity, SystemStatus,
};
use serde_json::{Map, Value, json};
use switchyard_support::{Reply, fail, ok, take_events};

const RESTORE: &str = DEFAULT_MODEL_START;

fn none() -> Map<String, Value> {
    Map::new()
}

fn params(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn has(st: &SystemStatus, code: &str) -> bool {
    codes(st).iter().any(|c| c == code)
}

// ── status ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_status_up_real_fixtures() {
    let f = Fix::new().await;
    wire(
        &f.runner,
        vec![frontend_doc(), worker_doc(), engine(false)],
        "",
    );
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Up && st.version.as_deref() == Some("1.5.0"));
    assert!(st.summary.contains("unsloth/Qwen3.8-27B-NVFP4"));
    assert_eq!(
        st.components
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        [FE, WK]
    );
    assert!(st.components.iter().all(|c| c.state == "running"));
    // no exclusive engine runs while Dynamo holds the card -> the exclusive engines' aliases and routes are dark
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dynamo.holds_card")
        .unwrap();
    assert_eq!(p.severity, Severity::Info);
    for dark in [
        "nemotron-3.5-bolt (route bolt)",
        "glm-5.3-flash-next-local",
        "qwen3.8-flash-next and -quick",
        "qwen3.8-27b-1m (direct-27b)",
    ] {
        assert!(p.message.contains(dark), "{dark}");
    }
    // neutral: the switch script without an alias only lists engines; it never tears the running stack down
    assert_eq!(p.fix.as_deref(), Some(RESTORE));
    assert!(!p.fix.as_deref().unwrap().contains("qwen3.8-flash-next"));
    assert!(!has(&st, "dynamo.structural_tag.off")); // the live worker has the flag
    let roles: BTreeSet<&str> = st.endpoints.iter().map(|e| e.role.as_str()).collect();
    assert_eq!(roles, BTreeSet::from(["api", "health", "metrics"]));
}

#[tokio::test]
async fn test_status_loading_when_models_empty() {
    let f = Fix::new().await;
    wire(&f.runner, vec![frontend_doc(), worker_doc()], "");
    http_live(&f.mock, false);
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Degraded && st.summary.starts_with("loading"));
    assert!(
        st.components[1]
            .detail
            .as_deref()
            .unwrap()
            .contains("loading")
    );
}

#[tokio::test]
async fn test_status_off_when_nothing_exists_and_down_when_not_opt_in() {
    let f = Fix::new().await;
    wire(&f.runner, vec![], "");
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Off && codes(&st) == ["dynamo.off"]);
    assert!(st.components.iter().all(|c| c.state == "absent"));
    // opt-in switched off: the old error-level report, with the configured start command as the fix
    let st2 = f
        .make_with(|a| {
            a.policy = CardPolicy {
                opt_in: false,
                ..CardPolicy::default()
            }
        })
        .status()
        .await
        .unwrap();
    assert!(
        st2.health == Health::Down
            && has(&st2, "dynamo.frontend.down")
            && has(&st2, "dynamo.worker.absent")
    );
    assert!(st2.components.iter().all(|c| c.state == "absent"));
    for p in &st2.problems {
        assert_eq!(
            p.fix.as_deref(),
            Some("scripts/use-model.sh qwen3.8-27b-dynamo")
        );
        assert!(!p.fix.as_deref().unwrap().contains("docker compose up"));
    }
}

#[tokio::test]
async fn test_status_frontend_up_worker_absent() {
    let f = Fix::new().await;
    wire(&f.runner, vec![frontend_doc()], "");
    http_live(&f.mock, false);
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Degraded && has(&st, "dynamo.worker.absent"));
}

#[tokio::test]
async fn test_status_flags_missing_structural_tag_and_wrong_parser_dialect() {
    let f = Fix::new().await;
    let mut w = worker_doc();
    let mut cmd: Vec<Value> = w["Config"]["Cmd"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c.as_str() != Some("--dyn-enable-structural-tag"))
        .cloned()
        .collect();
    cmd.push(json!("--tool-call-parser"));
    cmd.push(json!("hermes"));
    w["Config"]["Cmd"] = Value::Array(cmd);
    wire(&f.runner, vec![frontend_doc(), w, engine(false)], "");
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    assert!(has(&st, "dynamo.structural_tag.off") && has(&st, "dynamo.parser.dialect"));
    assert!(st.problems.iter().all(|p| p.fix.is_some()));
}

#[tokio::test]
async fn test_status_contended_card_and_kv_router() {
    let f = Fix::new().await;
    let mut fe = frontend_doc();
    fe["Config"]["Cmd"]
        .as_array_mut()
        .unwrap()
        .extend([json!("--router-mode"), json!("kv")]);
    wire(&f.runner, vec![fe, worker_doc(), engine(true)], "");
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    assert!(has(&st, "dynamo.card.contended") && has(&st, "dynamo.router.kv"));
}

#[tokio::test]
async fn test_status_never_raises() {
    let f = Fix::new().await;
    f.runner.on_err(
        is_docker("inspect"),
        RunnerError::Os("RuntimeError", "socket gone".into()),
    );
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Unknown && st.problems[0].fix.is_some());
}

// ── metrics ─────────────────────────────────────────────────────────────────────────────────────

fn by_key(snap: &MetricsSnapshot) -> BTreeMap<String, Vec<MetricPoint>> {
    let mut out: BTreeMap<String, Vec<MetricPoint>> = BTreeMap::new();
    for p in &snap.points {
        out.entry(p.key.clone()).or_default().push(p.clone());
    }
    out
}

#[tokio::test]
async fn test_metrics_map_real_names() {
    let f = Fix::new().await;
    wire(&f.runner, vec![worker_doc()], "");
    wire_worker_exec(&f.runner, None);
    http_live(&f.mock, true);
    let snap = f.make().metrics().await.unwrap();
    let k = by_key(&snap);
    assert!(snap.available && k["dyn.up"][0].value == 1.0 && k["dyn.worker_up"][0].value == 1.0);
    assert_eq!(k["dyn.models"][0].value, 1.0);
    assert_eq!(
        k["dyn.requests_total"][0]
            .labels
            .get("status")
            .map(String::as_str),
        Some("success")
    );
    assert_eq!(k["dyn.requests_total"][0].value, 1.0);
    assert_eq!(k["dyn.in_flight"][0].value, 0.0);
    let toks: BTreeMap<String, f64> = k["dyn.tokens"]
        .iter()
        .map(|p| (p.labels["kind"].clone(), p.value))
        .collect();
    assert_eq!(
        toks,
        BTreeMap::from([
            ("cached".into(), 0.0),
            ("completion".into(), 40.0),
            ("prompt".into(), 18.0)
        ])
    );
    assert!(k["dyn.kv_cache_usage"][0].value == 0.0 && k["dyn.kv_cache_usage"][0].unit == "ratio");
    // lifetime quantile of the single 0.302 s TTFT sample lands inside its (0.22, 0.47] bucket
    let p50 = k["dyn.ttft_ms"]
        .iter()
        .find(|p| p.labels["q"] == "p50")
        .unwrap();
    assert!(220.0 < p50.value && p50.value <= 470.0);
    let itl: BTreeSet<String> = k["dyn.itl_ms"]
        .iter()
        .map(|p| p.labels["q"].clone())
        .collect();
    assert_eq!(itl, BTreeSet::from(["p50".into(), "p95".into()]));
    assert!((k["dyn.spec_accept_ratio"][0].value - 26.0 / 30.0).abs() < 1e-9);
    assert!(
        !snap
            .points
            .iter()
            .any(|x| x.key == "dyn.tokens" && x.labels["kind"] == "reasoning")
    ); // never invented
}

#[tokio::test]
async fn test_metrics_tolerates_missing_worker_and_empty_frontend() {
    let f = Fix::new().await;
    wire(&f.runner, vec![], "");
    f.mock.get("/metrics", Reply::text(200, "# nothing\n"));
    let snap = f.make().metrics().await.unwrap();
    let keys: BTreeSet<&str> = snap.points.iter().map(|p| p.key.as_str()).collect();
    assert!(snap.available && keys == BTreeSet::from(["dyn.up", "dyn.worker_up"]));
    assert_eq!(by_key(&snap)["dyn.worker_up"][0].value, 0.0);
}

#[tokio::test]
async fn test_metrics_frontend_down_is_data() {
    let f = Fix::new().await;
    f.mock.get("/metrics", Reply::Close);
    let snap = f.make().metrics().await.unwrap();
    assert!(!snap.available && snap.points.is_empty() && snap.error.is_some());
}

#[tokio::test]
async fn test_worker_read_uses_exec_not_host_ip() {
    let f = Fix::new().await;
    wire(&f.runner, vec![worker_doc()], "");
    wire_worker_exec(&f.runner, None);
    http_live(&f.mock, true);
    f.make().metrics().await.unwrap();
    let ex = f
        .runner
        .argvs()
        .into_iter()
        .find(|a| a[..2] == ["docker", "exec"])
        .unwrap();
    assert!(ex[2] == WK && ex[3] == "python3" && ex[5].contains("8081"));
}

// ── events ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_recent_events_redacted_bounded_and_denoised() {
    let f = Fix::new().await;
    f.runner.on_fn(
        |a: &[String]| is_logs(a) && a.last().is_some_and(|x| x == FE),
        |_a| ok(&fx("frontend_logs.txt")),
    );
    let extra = "2026-09-24T08:09:30.000000000Z INFO auth Authorization: Bearer abcdefghijkl token=hf_ABCDEFGHIJKLMNOP\n";
    let wl = fx("worker_logs.txt") + extra;
    f.runner.on_fn(
        |a: &[String]| is_logs(a) && a.last().is_some_and(|x| x == WK),
        move |_a| ok(&wl),
    );
    wire(&f.runner, vec![frontend_doc(), worker_doc()], "");
    let evs = f.make().recent_events(30).await.unwrap();
    assert!(!evs.is_empty() && evs.len() <= 30);
    assert!(
        evs.iter()
            .all(|e| e.kind == "dynamo.log" && e.system.as_str() == "dynamo")
    );
    assert!(evs.windows(2).all(|w| w[0].ts <= w[1].ts));
    let blob: Vec<&str> = evs.iter().map(|e| e.summary.as_str()).collect();
    let blob = blob.join("\n");
    assert!(
        !blob.contains("abcdefghijkl") && !blob.contains("hf_ABCDEF") && !blob.contains('\x1b')
    );
    assert!(!blob.contains("/root/") && !blob.contains("/home/") && !blob.contains("/usr/local"));
    assert!(!blob.contains("http response sent")); // 2 s health-poll spam dropped
    assert!(
        evs.iter()
            .all(|e| matches!(e.data["component"].as_str(), Some("frontend" | "worker")))
    );
    let tail = f.runner.argvs().into_iter().find(|a| is_logs(a)).unwrap();
    let i = tail.iter().position(|x| x == "--tail").unwrap();
    assert!(tail[i + 1].parse::<usize>().unwrap() <= 1000);
}

#[tokio::test]
async fn test_recent_events_refuses_unverified_container() {
    let f = Fix::new().await;
    let mut imposter = worker_doc();
    imposter["Config"]["Labels"]["com.docker.compose.project"] = json!("somebody-else");
    f.runner.on(is_logs, ok("2026-09-24T08:00:00Z hello\n"));
    wire(&f.runner, vec![frontend_doc(), imposter], "");
    let evs = f.make().recent_events(10).await.unwrap();
    assert!(evs.iter().all(|e| e.data["component"] == "frontend"));
}

#[tokio::test]
async fn test_follow_events_yields_new_lines_and_terminates() {
    let f = Fix::new().await;
    let lines =
        "2026-09-24T09:00:01.100000000Z INFO first\n2026-09-24T09:00:02.200000000Z WARN second\n";
    f.runner.on(is_logs, ok(lines));
    wire(&f.runner, vec![worker_doc()], "");
    let ad = f.make();
    let got = take_events(Arc::clone(&ad).follow_events(Some(1_790_000_000.0)), 2, 10).await;
    assert_eq!(
        got.iter()
            .map(|e| e.data["level"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["INFO", "WARN"]
    );
    let log = f.runner.argvs().into_iter().find(|a| is_logs(a)).unwrap();
    assert!(log.iter().any(|x| x == "--since"));
}

// ── config files + params ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_config_files_and_readonly_compose() {
    let f = Fix::new().await;
    let ad = f.make();
    let specs = ad.config_files().unwrap();
    let compose = specs.iter().find(|s| s.id == "compose").unwrap();
    assert!(
        !compose.editable
            && compose
                .why_readonly
                .as_deref()
                .unwrap()
                .contains("override")
    );
    assert!(specs.iter().find(|s| s.id == "override").unwrap().editable);
    assert_eq!(ad.config_path("compose").unwrap(), f.compose);
    assert_eq!(
        ad.config_path("override").unwrap().file_name().unwrap(),
        "override.yml"
    );
    assert!(matches!(
        ad.config_path("../../etc/passwd"),
        Err(AdapterError::UnknownId(_))
    ));
    let cf = ad.read_config("compose").await.unwrap();
    assert!(cf.content.contains("dynamo.vllm") && cf.sha256.len() == 64);
    let empty = ad.read_config("override").await.unwrap();
    assert!(empty.content.is_empty() && empty.mtime == 0.0);
}

#[tokio::test]
async fn test_validate_override() {
    let f = Fix::new().await;
    let ad = f.make();
    f.runner.on(is_docker("compose"), ok(""));
    let r = ad
        .validate_config("override", &fx("golden_override.yml"))
        .await
        .unwrap();
    assert!(r.ok && r.authoritative, "{:?}", r.diagnostics);
    assert_eq!(
        std::fs::read_dir(f.state().join("dynamo/tmp"))
            .unwrap()
            .count(),
        0
    ); // temp file cleaned up
    assert!(!f.state().join("dynamo/override.yml").exists()); // validate never writes the live file
    let r2 = ad
        .validate_config(
            "override",
            "services:\n  dynamo-worker:\n    privileged: true\n",
        )
        .await
        .unwrap();
    assert!(!r2.ok && r2.diagnostics[0].message.contains("only `command`"));
    assert!(
        !ad.validate_config("override", "services:\n  evil:\n    command: [x]\n")
            .await
            .unwrap()
            .ok
    );
    let flag = "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --trust-remote-code]\n";
    assert!(!ad.validate_config("override", flag).await.unwrap().ok);
    let secret = "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --model, sk-abcdefghijklmnop]\n";
    assert!(!ad.validate_config("override", secret).await.unwrap().ok);
    let nost = "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --max-num-seqs, '4']\n";
    let r4 = ad.validate_config("override", nost).await.unwrap();
    assert!(
        r4.ok
            && r4
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Warn && d.message.contains("structural"))
    );
    assert!(!ad.validate_config("override", "a: [").await.unwrap().ok);
}

#[tokio::test]
async fn test_validate_uses_compose_config_and_reports_failure() {
    let f = Fix::new().await;
    f.runner.on(
        is_docker("compose"),
        fail("services.dynamo-worker.command must be a list"),
    );
    let r = f
        .make()
        .validate_config("override", &fx("golden_override.yml"))
        .await
        .unwrap();
    assert!(
        !r.ok
            && r.diagnostics
                .last()
                .unwrap()
                .message
                .contains("docker compose config")
    );
    let argv = compose_calls(&f.runner)[0].clone(); // after the inspect that finds the compose files in use
    assert!(argv[..2] == ["docker", "compose"] && argv[argv.len() - 2..] == ["config", "-q"]);
    assert_eq!(argv.iter().filter(|a| *a == "-f").count(), 2);
}

#[tokio::test]
async fn test_preview_is_pure_and_apply_writes_0600() {
    let f = Fix::new().await;
    let ad = f.make();
    let values = params(
        json!({"max_model_len": 131072, "gpu_memory_utilization": 0.6, "num_speculative_tokens": 3,
                               "enable_prefix_caching": false, "router_mode": "kv", "reasoning_parser": "none"}),
    );
    let preview = ad.preview_params(&values).await.unwrap();
    assert_eq!(preview, fx("golden_override.yml"));
    assert!(!f.state().join("dynamo").exists()); // preview touched nothing
    f.runner.on(is_docker("compose"), ok(""));
    wire(&f.runner, vec![], ""); // no worker: write only, no recreate
    let res = ad.apply_params(&values).await.unwrap();
    let ov = f.state().join("dynamo/override.yml");
    assert!(
        res.ok
            && res.post_check == PostCheck::Skipped
            && !res.restarted
            && res.new_sha256.is_some()
    );
    assert_eq!(std::fs::read_to_string(&ov).unwrap(), preview);
    assert_eq!(
        std::fs::metadata(&ov).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(ov.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(ad.current_values()["router_mode"], "kv");
    assert!(
        !f.runner
            .argvs()
            .iter()
            .any(|a| a[..2] == ["docker", "compose"] && a.iter().any(|x| x == "up"))
    );
}

#[tokio::test]
async fn test_apply_rejects_bad_values_and_rolls_back_bad_compose() {
    let f = Fix::new().await;
    let ad = f.make();
    let r = ad
        .apply_params(&params(json!({"max_num_seqs": 0})))
        .await
        .unwrap();
    assert!(!r.ok && !f.state().join("dynamo/override.yml").exists());
    f.runner.on(is_docker("compose"), fail("invalid"));
    let r2 = ad
        .apply_params(&params(json!({"max_num_seqs": 8})))
        .await
        .unwrap();
    assert!(
        !r2.ok && r2.detail.contains("rejected") && !f.state().join("dynamo/override.yml").exists()
    );
}

#[tokio::test]
async fn test_apply_recreates_worker_then_health_polls() {
    let f = Fix::new().await;
    let ad = f.make();
    let old = worker_doc();
    let mut new = old.clone();
    new["State"]["StartedAt"] = json!("2026-09-24T09:30:00.000000000Z");
    let up_done = Arc::new(Mutex::new(false));
    let u1 = Arc::clone(&up_done);
    f.runner.on_fn(is_docker("inspect"), move |_a| {
        inspect_json(&[if *u1.lock().unwrap() {
            new.clone()
        } else {
            old.clone()
        }])
    });
    let u2 = Arc::clone(&up_done);
    f.runner.on_fn(is_docker("compose"), move |a: &[String]| {
        if a.iter().any(|x| x == "up") {
            *u2.lock().unwrap() = true;
        }
        ok("")
    });
    http_live(&f.mock, true);
    let res = ad
        .apply_params(&params(json!({"max_num_seqs": 16})))
        .await
        .unwrap();
    assert!(
        res.ok && res.restarted && res.post_check == PostCheck::Healthy,
        "{res:?}"
    );
    let up = f
        .runner
        .argvs()
        .into_iter()
        .find(|a| a[..2] == ["docker", "compose"] && a.iter().any(|x| x == "up"))
        .unwrap();
    assert_eq!(
        up[up.len() - 4..],
        s(&["up", "-d", "dynamo-frontend", "dynamo-worker"])
    );
    assert!(!up.iter().any(|x| x == "-v"));
    let nf = up.iter().filter(|x| *x == "-f").count();
    assert!(nf == 1 || nf == 2);
    assert!(up[up.iter().position(|x| x == "-f").unwrap() + 1].ends_with("docker-compose.yml"));
}

#[tokio::test]
async fn test_apply_unhealthy_restores_previous_override() {
    let f = Fix::new().await;
    let ad = f.make_with(|a| a.health_timeout = 0.05);
    let ov = f.state().join("dynamo/override.yml");
    std::fs::create_dir_all(ov.parent().unwrap()).unwrap();
    std::fs::write(&ov, "# previous\nservices: {}\n").unwrap();
    f.runner
        .on(is_docker("inspect"), inspect_json(&[worker_doc()])); // StartedAt never changes
    f.runner.on(is_docker("compose"), ok(""));
    http_live(&f.mock, true);
    let res = ad
        .apply_params(&params(json!({"max_num_seqs": 16})))
        .await
        .unwrap();
    assert!(!res.ok && res.post_check == PostCheck::Unhealthy && res.rolled_back);
    assert_eq!(
        std::fs::read_to_string(&ov).unwrap(),
        "# previous\nservices: {}\n"
    );
}

#[tokio::test]
async fn test_post_apply() {
    let f = Fix::new().await;
    let ad = f.make();
    assert!(!ad.post_apply("compose", true).await.unwrap().ok);
    let r = ad.post_apply("override", false).await.unwrap();
    assert!(r.ok && r.post_check == PostCheck::Skipped);
}

// ── actions ─────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_action_catalog_matches_contract() {
    let f = Fix::new().await;
    let acts: BTreeMap<String, Danger> = f
        .make()
        .actions()
        .unwrap()
        .into_iter()
        .map(|a| (a.id, a.danger))
        .collect();
    let want: BTreeMap<String, Danger> = [
        ("preflight", Danger::Safe),
        ("up", Danger::Disruptive),
        ("down", Danger::Disruptive),
        ("restart", Danger::Disruptive),
        ("up_mock", Danger::Safe),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(acts, want);
}

#[tokio::test]
async fn test_preflight_lists_sharers_by_uuid_and_ignores_other_card() {
    let f = Fix::new().await;
    let other_card = engine_doc(true, FN, "ai-lab-vllm-qwen38", OTHER_UUID);
    wire(
        &f.runner,
        vec![worker_doc(), engine(true), other_card],
        &format!("{WK}\n{FE}\n{ENG}\n{FN}\n"),
    );
    let res = f.make().run_action("preflight", &none()).await.unwrap();
    assert!(res.ok && res.output.contains(ENG) && !res.output.contains(&format!("{FN} ("))); // other card
    assert!(res.output.contains("touches no other container") && res.output.contains(RESTORE));
    assert!(compose_calls(&f.runner).is_empty()); // safe: nothing was started/stopped
}

#[tokio::test]
async fn test_up_refused_while_engine_runs() {
    let f = Fix::new().await;
    wire(&f.runner, vec![engine(true)], &format!("{ENG}\n"));
    let res = f.make().run_action("up", &none()).await.unwrap();
    assert!(!res.ok && res.output.contains("refused"));
    assert!(
        res.problems[0]
            .fix
            .as_deref()
            .unwrap()
            .contains("stop_sharing_engine=true")
    );
    assert!(compose_calls(&f.runner).is_empty());
    assert!(
        !f.runner
            .argvs()
            .iter()
            .any(|a| a[..2] == ["docker", "stop"])
    );
}

#[tokio::test]
async fn test_up_with_stop_sharing_engine_stops_exactly_that_container() {
    let f = Fix::new().await;
    let ad = f.make();
    let state = Arc::new(Mutex::new((false, false))); // (engine_down, up)
    let wk = worker_doc();
    let s1 = Arc::clone(&state);
    f.runner
        .on_fn(is_docker("inspect"), move |argv: &[String]| {
            let st = *s1.lock().unwrap();
            let mut docs = Vec::new();
            if argv[2..].iter().any(|x| x == ENG) {
                docs.push(engine(!st.0));
            }
            if argv[2..].iter().any(|x| x == WK) && st.1 {
                docs.push(wk.clone());
            }
            inspect_json(&docs)
        });
    f.runner.on(is_docker("ps"), ok(&format!("{ENG}\n")));
    let s2 = Arc::clone(&state);
    f.runner
        .on_fn(is_docker("compose"), move |argv: &[String]| {
            let mut st = s2.lock().unwrap();
            if argv.last().is_some_and(|x| x == "down") {
                st.0 = true;
            }
            if argv.iter().any(|x| x == "up") {
                st.1 = true;
            }
            ok("")
        });
    http_live(&f.mock, true);
    let res = ad
        .run_action("up", &params(json!({"stop_sharing_engine": true})))
        .await
        .unwrap();
    assert!(res.ok, "{}", res.output);
    let calls = compose_calls(&f.runner);
    let down = calls
        .iter()
        .find(|a| a.last().is_some_and(|x| x == "down"))
        .unwrap()
        .clone();
    assert_eq!(
        down,
        s(&[
            "docker",
            "compose",
            "-f",
            "/repo/inference/vllm-qwen38/docker-compose.yml",
            "down"
        ])
    );
    let cwd = f
        .runner
        .calls()
        .into_iter()
        .find(|c| c.argv == down)
        .unwrap()
        .cwd;
    assert_eq!(cwd, Some(PathBuf::from("/repo/inference/vllm-qwen38")));
    assert!(
        !f.runner
            .argvs()
            .iter()
            .any(|a| a[..2] == ["docker", "stop"] || a.iter().any(|x| x == "-v" || x == "rm"))
    );
    assert!(
        !f.runner
            .argvs()
            .iter()
            .any(|a| a.join(" ").contains("use-model"))
    );
    assert!(calls[0].last().is_some_and(|x| x == "down") && calls[1].iter().any(|x| x == "up")); // stop first, then up
}

#[tokio::test]
async fn test_stop_sharing_never_touches_other_or_unverified_containers() {
    let f = Fix::new().await;
    let comfy = engine_doc(true, "comfyui-b-1", "comfyui", UUID);
    wire(
        &f.runner,
        vec![engine(true), comfy],
        &format!("{ENG}\ncomfyui-b-1\n"),
    );
    let res = f
        .make()
        .run_action("up", &params(json!({"stop_sharing_engine": true})))
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("comfyui-b-1") && compose_calls(&f.runner).is_empty());
}

#[tokio::test]
async fn test_up_with_impostor_labels_is_refused() {
    let f = Fix::new().await;
    wire(
        &f.runner,
        vec![engine_doc(true, ENG, "evil", UUID)],
        &format!("{ENG}\n"),
    );
    let res = f
        .make()
        .run_action("up", &params(json!({"stop_sharing_engine": true})))
        .await
        .unwrap();
    assert!(!res.ok && res.problems[0].code == "dynamo.stop.unverified");
    assert!(
        !compose_calls(&f.runner)
            .iter()
            .any(|a| a.last().is_some_and(|x| x == "down"))
    );
}

#[tokio::test]
async fn test_up_refused_with_mocker_running() {
    let f = Fix::new().await;
    let mut m = worker_doc();
    m["Name"] = json!("/ai-lab-dynamo-dynamo-mocker-1");
    wire(&f.runner, vec![m], "");
    let res = f.make().run_action("up", &none()).await.unwrap();
    assert!(!res.ok && res.output.contains("mocker") && compose_calls(&f.runner).is_empty());
}

#[tokio::test]
async fn test_down_never_uses_volumes_flag_and_no_other_card() {
    let f = Fix::new().await;
    f.runner.on(is_docker("compose"), ok("done"));
    let res = f.make().run_action("down", &none()).await.unwrap();
    let a = compose_calls(&f.runner)[0].clone();
    assert!(res.ok && a.last().is_some_and(|x| x == "down"));
    assert!(!a.iter().any(|x| x == "-v" || x == "--volumes") && a.iter().any(|x| x == "--profile"));
}

#[tokio::test]
async fn test_up_mock_uses_profile_and_refuses_with_real_worker() {
    let f = Fix::new().await;
    wire(&f.runner, vec![worker_doc()], "");
    let res = f.make().run_action("up_mock", &none()).await.unwrap();
    assert!(!res.ok && compose_calls(&f.runner).is_empty());
    f.runner.clear_rules();
    f.runner.clear_calls();
    wire(&f.runner, vec![], "");
    f.runner.on(is_docker("compose"), ok(""));
    let res2 = f.make().run_action("up_mock", &none()).await.unwrap();
    let up = compose_calls(&f.runner)
        .into_iter()
        .find(|a| a.iter().any(|x| x == "up"))
        .unwrap();
    assert!(res2.ok && up.iter().any(|x| x == "--profile") && up.iter().any(|x| x == "mock"));
    assert_eq!(up[up.len() - 2..], s(&["dynamo-frontend", "dynamo-mocker"]));
}

#[tokio::test]
async fn test_restart_force_recreates_and_unknown_action_is_keyerror() {
    let f = Fix::new().await;
    let ad = f.make();
    let old = worker_doc();
    let mut new = old.clone();
    new["State"]["StartedAt"] = json!("2026-09-24T09:31:00.000000000Z");
    let up = Arc::new(Mutex::new(false));
    let u1 = Arc::clone(&up);
    f.runner.on_fn(is_docker("inspect"), move |_a| {
        inspect_json(&[if *u1.lock().unwrap() {
            new.clone()
        } else {
            old.clone()
        }])
    });
    let u2 = Arc::clone(&up);
    f.runner.on_fn(is_docker("compose"), move |_a| {
        *u2.lock().unwrap() = true;
        ok("")
    });
    http_live(&f.mock, true);
    let res = ad.run_action("restart", &none()).await.unwrap();
    assert!(
        res.ok
            && compose_calls(&f.runner)[0]
                .iter()
                .any(|x| x == "--force-recreate")
    );
    assert!(matches!(
        ad.run_action("rm_rf", &none()).await,
        Err(AdapterError::UnknownId(_))
    ));
}

// ── playground ──────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_playground() {
    let f = Fix::new().await;
    let ad = f.make();
    let pg = ad.playground().unwrap().unwrap();
    assert!(pg.wire == "openai_chat" && pg.default_model == "unsloth/Qwen3.8-27B-NVFP4");
    wire(&f.runner, vec![frontend_doc(), worker_doc()], "");
    http_live(&f.mock, true);
    ad.status().await.unwrap();
    let pg2 = ad.playground().unwrap().unwrap();
    assert_eq!(pg2.models, ["unsloth/Qwen3.8-27B-NVFP4"]);
    assert_eq!(
        ad.playground_url().unwrap(),
        format!("{}/v1/chat/completions", f.mock.uri())
    );
}

#[test]
fn test_json_fixtures_are_real_captures() {
    assert_eq!(fxj("frontend_health.json")["status"], "healthy");
}

#[tokio::test]
async fn test_sharers_ignores_container_that_stopped_after_ps() {
    let f = Fix::new().await;
    wire(&f.runner, vec![engine(false)], &format!("{ENG}\n"));
    f.runner.on(is_docker("compose"), ok(""));
    let res = f.make().run_action("preflight", &none()).await.unwrap();
    assert!(res.ok && res.output.contains("No other container is running"));
}

fn last3(out: &str) -> Vec<String> {
    let lines: Vec<String> = out.lines().map(str::to_owned).collect();
    lines[lines.len() - 3..].to_vec()
}

#[tokio::test]
async fn test_preflight_machine_readable_tail() {
    let f = Fix::new().await;
    // Dynamo holds the card, the 27B is stopped and nothing exclusive runs -> listed as stopped, the aliases dark
    wire(
        &f.runner,
        vec![worker_doc(), engine(false)],
        &format!("{WK}\n"),
    );
    let out = f
        .make()
        .run_action("preflight", &none())
        .await
        .unwrap()
        .output;
    let dark = "router_aliases_dark: nemotron-3.5-bolt (route bolt), glm-5.3-flash-next-local, qwen3.8-flash-next and \
                -quick (auto-local and auto-flashnext capable tier), qwen3.8-27b-1m (direct-27b)";
    assert_eq!(
        last3(&out),
        [
            format!("shares_card: {ENG} (stopped)"),
            "stoppable: none".into(),
            dark.into()
        ]
    );
    assert!(out.contains("No other container is running")); // prose kept
    // the 27B running, Dynamo not the holder -> it is the one stop_sharing_engine may stop
    f.runner.clear_rules();
    wire(&f.runner, vec![engine(true)], &format!("{ENG}\n"));
    let out = f
        .make()
        .run_action("preflight", &none())
        .await
        .unwrap()
        .output;
    assert_eq!(
        last3(&out),
        [
            format!("shares_card: {ENG} (running)"),
            format!("stoppable: {ENG} (running)"),
            "router_aliases_dark: none".into()
        ]
    );
    // the default model running on the card (TP2: both UUIDs) -> listed, but never offered for stopping
    f.runner.clear_rules();
    wire(
        &f.runner,
        vec![flashnext_doc(true, None)],
        &format!("{FN}\n"),
    );
    let out = f
        .make()
        .run_action("preflight", &none())
        .await
        .unwrap()
        .output;
    assert_eq!(
        last3(&out),
        [
            format!("shares_card: {FN} (running)"),
            "stoppable: none".into(),
            "router_aliases_dark: none".into()
        ]
    );
    // nothing at all
    f.runner.clear_rules();
    wire(&f.runner, vec![worker_doc()], &format!("{WK}\n"));
    let out = last3(
        &f.make()
            .run_action("preflight", &none())
            .await
            .unwrap()
            .output,
    );
    assert_eq!(out[..2], s(&["shares_card: none", "stoppable: none"]));
    assert!(out[2].starts_with("router_aliases_dark: nemotron"));
    f.runner.clear_rules();
    wire(&f.runner, vec![], "");
    let out = f
        .make()
        .run_action("preflight", &none())
        .await
        .unwrap()
        .output;
    assert_eq!(
        last3(&out),
        s(&[
            "shares_card: none",
            "stoppable: none",
            "router_aliases_dark: none"
        ])
    );
}

// ── hand-edited override: closed allowlist (audit payloads) ─────────────────────────────────────

const W: &str = "python3, -m, dynamo.vllm";
const MODEL: &str = "/root/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B-NVFP4/snapshots/16b6615af3548b88e2d8e382457bc705b00479cf";
const SERVED: &str = "unsloth/Qwen3.8-27B-NVFP4";

fn errors(res: &nemo_console::domain::models::ValidationResult) -> String {
    res.diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join(" | ")
}

async fn verdict(f: &Fix, cmd: &str) -> nemo_console::domain::models::ValidationResult {
    f.runner.on(is_docker("compose"), ok(""));
    f.make()
        .validate_config(
            "override",
            &format!("services:\n  dynamo-worker:\n    command: [{cmd}]\n"),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn test_override_rejects_single_dash_flag() {
    let f = Fix::new().await;
    f.runner.on(is_docker("compose"), ok(""));
    let doc = format!("services:\n  dynamo-worker:\n    command: [{W}, -cc, '{{\"x\":1}}']\n");
    let r = f.make().validate_config("override", &doc).await.unwrap();
    assert!(!r.ok && errors(&r).contains("-cc"));
    assert!(compose_calls(&f.runner).is_empty()); // refused before compose ever sees it
}

#[tokio::test]
async fn test_override_rejects_new_interpolation_in_argv() {
    let f = Fix::new().await;
    f.runner.on(is_docker("compose"), ok(""));
    let doc =
        format!("services:\n  dynamo-worker:\n    command: [{W}, --model, '${{HF_TOKEN}}']\n");
    let r = f.make().validate_config("override", &doc).await.unwrap();
    assert!(!r.ok && errors(&r).contains("`$`"));
}

#[tokio::test]
async fn test_override_rejects_model_and_served_name_changes() {
    let f = Fix::new().await;
    f.runner.on(is_docker("compose"), ok(""));
    let r = f
        .make()
        .validate_config("override", &format!("services:\n  dynamo-worker:\n    command: [{W}, --served-model-name, /etc/passwd]\n"))
        .await
        .unwrap();
    assert!(!r.ok && errors(&r).contains("--served-model-name must equal"));
    let r2 = f
        .make()
        .validate_config(
            "override",
            &format!("services:\n  dynamo-worker:\n    command: [{W}, --model, /etc/shadow]\n"),
        )
        .await
        .unwrap();
    assert!(!r2.ok && errors(&r2).contains("--model must equal"));
    let same = format!(
        "services:\n  dynamo-worker:\n    command: [{W}, --model, {MODEL}, --served-model-name, {SERVED}]\n"
    );
    assert!(
        f.make()
            .validate_config("override", &same)
            .await
            .unwrap()
            .ok
    );
}

#[tokio::test]
async fn test_override_env_interpolation_and_unknown_keys_rejected() {
    let f = Fix::new().await;
    f.runner.on(is_docker("compose"), ok(""));
    let doc = "services:\n  dynamo-frontend:\n    environment:\n      DYN_LOGGING_JSONL: \"${LITELLM_API_KEY}\"\n";
    let r = f.make().validate_config("override", doc).await.unwrap();
    assert!(!r.ok && errors(&r).contains("`$`"));
    let r2 = f
        .make()
        .validate_config(
            "override",
            "services:\n  dynamo-frontend:\n    environment:\n      LD_PRELOAD: /x.so\n",
        )
        .await
        .unwrap();
    assert!(!r2.ok && errors(&r2).contains("not defined in the base compose"));
    let r3 = f
        .make()
        .validate_config("override", "services:\n  dynamo-worker:\n    environment:\n      DYN_DEFAULT_THINKING_MODE: enabled\n")
        .await
        .unwrap();
    assert!(!r3.ok); // key absent from the base worker env
    let good = "services:\n  dynamo-frontend:\n    environment:\n      DYN_LOGGING_JSONL: 'true'\n";
    assert!(f.make().validate_config("override", good).await.unwrap().ok);
}

#[tokio::test]
async fn test_override_fails_closed_without_base_compose() {
    let f = Fix::new().await;
    std::fs::remove_file(&f.compose).unwrap();
    let r = f
        .make()
        .validate_config(
            "override",
            &format!("services:\n  dynamo-worker:\n    command: [{W}, --model, x]\n"),
        )
        .await
        .unwrap();
    assert!(!r.ok);
}

#[tokio::test]
async fn test_generated_override_still_passes_the_closed_allowlist() {
    let f = Fix::new().await;
    f.runner.on(is_docker("compose"), ok(""));
    let ad = f.make();
    let text = ad
        .preview_params(&params(
            json!({"max_model_len": 131072, "router_mode": "kv", "enable_prefix_caching": false,
                                       "reasoning_parser": "none", "num_speculative_tokens": 3}),
        ))
        .await
        .unwrap();
    assert!(text.contains("${MAX_NUM_SEQS:-64}")); // base interpolation carried verbatim is allowed
    let r = ad.validate_config("override", &text).await.unwrap();
    assert!(r.ok && r.authoritative, "{:?}", r.diagnostics);
}

// ── second audit: duplicate flags, speculative-config, path/JSON values ─────────────────────────

#[tokio::test]
async fn test_duplicate_model_flag_last_wins_is_rejected() {
    let f = Fix::new().await;
    let r = verdict(&f, &format!("{W}, --model, {MODEL}, --model, evil/repo")).await;
    assert!(!r.ok && errors(&r).contains("more than once"));
    let r2 = verdict(
        &f,
        &format!("{W}, --served-model-name, {SERVED}, --served-model-name, evil/x"),
    )
    .await;
    assert!(!r2.ok && errors(&r2).contains("more than once"));
}

#[tokio::test]
async fn test_duplicate_detection_covers_equals_and_underscore_forms() {
    let f = Fix::new().await;
    let r = verdict(&f, &format!("{W}, --model, {MODEL}, --model=evil/repo")).await;
    assert!(!r.ok && errors(&r).contains("more than once"));
    let r2 = verdict(
        &f,
        &format!("{W}, --max-num-seqs, '4', --max_num_seqs, '8'"),
    )
    .await;
    assert!(!r2.ok && errors(&r2).contains("more than once"));
    let r3 = verdict(
        &f,
        &format!("{W}, --enable-prefix-caching, --no-enable-prefix-caching"),
    )
    .await;
    assert!(!r3.ok && errors(&r3).contains("more than once"));
    let r4 = verdict(&f, &format!("{W}, --model=evil/repo")).await; // `=` form must obey the model rule too
    assert!(!r4.ok && errors(&r4).contains("--model must equal"));
}

#[tokio::test]
async fn test_speculative_config_is_restricted_to_mtp() {
    let f = Fix::new().await;
    let evil = r#"'{"method":"draft_model","model":"evil/x"}'"#;
    let r = verdict(&f, &format!("{W}, --speculative-config, {evil}")).await;
    assert!(!r.ok && errors(&r).contains("speculative-config"));
    for bad in [
        r#"'{"method":"mtp","num_speculative_tokens":2,"model":"evil/x"}'"#,
        r#"'{"method":"mtp","num_speculative_tokens":99}'"#,
        r#"'{"method":"mtp","num_speculative_tokens":true}'"#,
        r#"'{"method":"mtp"}'"#,
        "'not json'",
        "'[1]'",
    ] {
        assert!(
            !verdict(&f, &format!("{W}, --speculative-config, {bad}"))
                .await
                .ok,
            "{bad}"
        );
    }
    let good = r#"'{"method":"mtp","num_speculative_tokens":3}'"#;
    assert!(
        verdict(&f, &format!("{W}, --speculative-config, {good}"))
            .await
            .ok
    );
}

#[tokio::test]
async fn test_path_or_json_values_must_equal_base() {
    let f = Fix::new().await;
    for cmd in [
        format!("{W}, --dyn-tool-call-parser, /etc/passwd"),
        format!("{W}, --max-num-seqs, ../../x"),
        format!("{W}, --dyn-reasoning-parser, '{{\"a\":1}}'"),
        format!("{W}, --request-plane, /tmp/x"),
        format!("{W}, --discovery-backend=/tmp/x"),
    ] {
        let r = verdict(&f, &cmd).await;
        assert!(!r.ok && errors(&r).contains("base compose"), "{cmd}");
    }
    assert!(
        verdict(
            &f,
            &format!("{W}, --request-plane, tcp, --max-num-seqs, '4'")
        )
        .await
        .ok
    );
}

#[tokio::test]
async fn test_stray_positional_tokens_rejected() {
    let f = Fix::new().await;
    assert!(!verdict(&f, &format!("{W}, stray")).await.ok);
}

// ── card model (addendum 9.4): Dynamo is opt-in; exclusive / coexisting / ignored / unknown sharers ───

#[tokio::test]
async fn test_status_off_is_info_not_error_when_containers_exited() {
    let f = Fix::new().await;
    // the live shape on 2026-10-06: frontend Exited (0), worker Exited (1) 8 days earlier, flashnext serving
    wire(
        &f.runner,
        vec![
            stopped(frontend_doc(), 0),
            stopped(worker_doc(), 1),
            flashnext_doc(true, None),
        ],
        "",
    );
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Off && st.summary.starts_with("off"));
    assert_eq!(
        st.problems
            .iter()
            .map(|p| (p.severity, p.code.as_str(), p.fix.as_deref()))
            .collect::<Vec<_>>(),
        [(Severity::Info, "dynamo.off", Some(START))]
    );
    assert_eq!(
        st.components
            .iter()
            .map(|c| (c.name.as_str(), c.state.as_str()))
            .collect::<Vec<_>>(),
        [(FE, "stopped"), (WK, "stopped")]
    );
    assert_eq!(st.components[1].detail.as_deref(), Some("exited (code 1)"));
    assert!(!st.problems.iter().any(|p| p.severity == Severity::Error));
    assert!(f.mock.seen().is_empty()); // nothing is polled over HTTP while off
}

#[tokio::test]
async fn test_status_crash_looping_worker_is_not_off_and_contends() {
    let f = Fix::new().await;
    wire(
        &f.runner,
        vec![
            frontend_doc(),
            restarting(worker_doc()),
            flashnext_doc(true, None),
        ],
        "",
    );
    http_live(&f.mock, false);
    let st = f.make().status().await.unwrap();
    assert!(st.health == Health::Degraded && !has(&st, "dynamo.off"));
    let wk = st.components.iter().find(|c| c.name == WK).unwrap();
    assert!(wk.state == "failed" && wk.detail.as_deref().unwrap().contains("restarting"));
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dynamo.card.contended")
        .unwrap();
    assert!(p.severity == Severity::Error && p.message.contains(FN) && p.message.contains(START));
    assert!(
        !p.fix.as_deref().unwrap().contains("docker stop")
            && p.fix.as_deref().unwrap().contains("`down`")
    );
    assert!(
        st.problems
            .iter()
            .find(|p| p.code == "dynamo.worker.stopped")
            .unwrap()
            .message
            .ends_with("restarting (crash loop)")
    );
    // frontend stopped too, worker still looping: still error-level, never "off"
    f.runner.clear_rules();
    wire(
        &f.runner,
        vec![stopped(frontend_doc(), 0), restarting(worker_doc())],
        "",
    );
    let st2 = f.make().status().await.unwrap();
    assert!(
        st2.health == Health::Down
            && has(&st2, "dynamo.frontend.down")
            && has(&st2, "dynamo.worker.stopped")
    );
    assert_eq!(
        st2.problems
            .iter()
            .find(|p| p.code == "dynamo.frontend.down")
            .unwrap()
            .fix
            .as_deref(),
        Some(START)
    );
}

#[tokio::test]
async fn test_card_contended_by_any_exclusive_engine_on_the_card_only() {
    let f = Fix::new().await;
    wire(
        &f.runner,
        vec![frontend_doc(), worker_doc(), flashnext_doc(true, None)],
        "",
    );
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    assert!(has(&st, "dynamo.card.contended") && !has(&st, "dynamo.holds_card"));
    // flashnext in single-GPU mode on the OTHER card: no contention, and its aliases are not dark either
    f.runner.clear_rules();
    wire(
        &f.runner,
        vec![
            frontend_doc(),
            worker_doc(),
            flashnext_doc(true, Some(&[OTHER_UUID])),
        ],
        "",
    );
    let st2 = f.make().status().await.unwrap();
    assert!(
        !has(&st2, "dynamo.card.contended")
            && !has(&st2, "dynamo.holds_card")
            && st2.health == Health::Up
    );
    // a coexisting engine beside the worker is not contention
    f.runner.clear_rules();
    wire(
        &f.runner,
        vec![frontend_doc(), worker_doc(), gpu(LIGHTNING)],
        "",
    );
    assert!(!has(
        &f.make().status().await.unwrap(),
        "dynamo.card.contended"
    ));
}

#[tokio::test]
async fn test_up_refused_while_default_model_holds_card_fix_is_switch_script() {
    let f = Fix::new().await;
    let docs = vec![
        flashnext_doc(true, None),
        gpu(LIGHTNING),
        gpu_all(OLLAMA),
        gpu_all(DCGM),
    ];
    let ps: Vec<String> = docs.iter().map(name_of).collect();
    wire(&f.runner, docs, &ps.join("\n"));
    for p in [json!({}), json!({"stop_sharing_engine": true})] {
        // the param can never stop the default model
        let res = f.make().run_action("up", &params(p)).await.unwrap();
        assert!(!res.ok && res.problems[0].code == "dynamo.up.exclusive_engine");
        assert_eq!(res.problems[0].fix.as_deref(), Some(START));
        assert!(res.output.contains(FN) && !res.output.contains(LIGHTNING));
        assert!(!res.output.contains(OLLAMA) && !res.output.contains(DCGM));
    }
    assert!(compose_calls(&f.runner).is_empty());
    assert!(
        !f.runner
            .argvs()
            .iter()
            .any(|a| a[..2] == ["docker", "stop"])
    );
}

#[tokio::test]
async fn test_up_proceeds_beside_coexisting_and_ignored_containers() {
    let f = Fix::new().await;
    let docs = vec![gpu(LIGHTNING), gpu_all(OLLAMA), gpu_all(DCGM)];
    let ps: Vec<String> = docs.iter().map(name_of).collect();
    let up = Arc::new(Mutex::new(false));
    let u1 = Arc::clone(&up);
    let d1 = docs.clone();
    f.runner
        .on_fn(is_docker("inspect"), move |argv: &[String]| {
            let mut live = d1.clone();
            if *u1.lock().unwrap() {
                live.push(worker_doc());
            }
            let want: Vec<&String> = argv[2..].iter().collect();
            let hit: Vec<Value> = live
                .into_iter()
                .filter(|d| want.iter().any(|w| **w == name_of(d)))
                .collect();
            inspect_json(&hit)
        });
    f.runner.on(is_docker("ps"), ok(&ps.join("\n")));
    let u2 = Arc::clone(&up);
    f.runner
        .on_fn(is_docker("compose"), move |argv: &[String]| {
            let mut g = u2.lock().unwrap();
            *g = *g || argv.iter().any(|x| x == "up");
            ok("")
        });
    http_live(&f.mock, true);
    let res = f.make().run_action("up", &none()).await.unwrap();
    assert!(res.ok, "{}", res.output);
    let tails: Vec<Vec<String>> = compose_calls(&f.runner)
        .iter()
        .map(|a| a[a.len() - 4..].to_vec())
        .collect();
    assert_eq!(
        tails,
        [s(&["up", "-d", "dynamo-frontend", "dynamo-worker"])]
    );
    assert!(
        !f.runner
            .argvs()
            .iter()
            .any(|a| a[..2] == ["docker", "stop"] || a.last().is_some_and(|x| x == "down"))
    ); // nothing stopped
}

#[tokio::test]
async fn test_up_refuses_unknown_container_and_names_it_without_docker_stop() {
    let f = Fix::new().await;
    wire(
        &f.runner,
        vec![gpu_doc("comfyui-b-1", None, false, true, "comfyui")],
        "comfyui-b-1\n",
    );
    let res = f
        .make()
        .run_action("up", &params(json!({"stop_sharing_engine": true})))
        .await
        .unwrap();
    assert!(!res.ok && res.problems[0].code == "dynamo.up.card_in_use");
    assert!(res.problems[0].message.contains("comfyui-b-1"));
    let fix = res.problems[0].fix.clone().unwrap();
    assert!(!fix.contains("docker stop") && !res.output.contains("docker stop"));
    assert!(fix.contains("comfyui-b-1") && compose_calls(&f.runner).is_empty());
    let pf = f.make().run_action("preflight", &none()).await.unwrap();
    assert_eq!(
        pf.problems
            .iter()
            .map(|p| p.code.as_str())
            .collect::<Vec<_>>(),
        ["dynamo.card.unknown"]
    );
    assert!(
        !pf.problems[0]
            .fix
            .as_deref()
            .unwrap()
            .contains("docker stop")
    );
}

#[tokio::test]
async fn test_stop_sharing_engine_stops_only_the_27b_never_flashnext() {
    let f = Fix::new().await;
    wire(
        &f.runner,
        vec![flashnext_doc(true, None), engine(true)],
        &format!("{FN}\n{ENG}\n"),
    );
    let res = f
        .make()
        .run_action("up", &params(json!({"stop_sharing_engine": true})))
        .await
        .unwrap();
    assert!(
        !res.ok
            && res.problems[0].fix.as_deref() == Some(START)
            && compose_calls(&f.runner).is_empty()
    );
    let acts = f.make().actions().unwrap();
    let up = acts.iter().find(|a| a.id == "up").unwrap();
    assert!(
        up.params[0].help.contains(ENG)
            && !up.params[0].help.contains(FN)
            && up.description.contains(START)
    );
    assert!(!up.description.contains("docker stop"));
}

#[tokio::test]
async fn test_policy_settings_drive_every_text() {
    let f = Fix::new().await;
    let pol = CardPolicy {
        start: "my/start dyn".into(),
        restore: "my/restore".into(),
        exclusive: s(&["my-excl-1"]),
        coexisting: s(&["my-co-1"]),
        ignored: s(&["my-mon-1"]),
        dark: s(&["alias-a", "route-b"]),
        ..CardPolicy::default()
    };
    wire(&f.runner, vec![frontend_doc(), worker_doc()], "");
    http_live(&f.mock, true);
    let p1 = pol.clone();
    let st = f.make_with(|a| a.policy = p1).status().await.unwrap();
    let hc = st
        .problems
        .iter()
        .find(|p| p.code == "dynamo.holds_card")
        .unwrap();
    assert!(hc.fix.as_deref() == Some("my/restore") && hc.message.ends_with("alias-a, route-b"));
    f.runner.clear_rules();
    wire(
        &f.runner,
        vec![gpu("my-excl-1"), gpu("my-co-1"), gpu_all("my-mon-1")],
        "my-excl-1\nmy-co-1\nmy-mon-1\n",
    );
    let p2 = pol.clone();
    let res = f
        .make_with(|a| a.policy = p2)
        .run_action("up", &none())
        .await
        .unwrap();
    assert!(
        !res.ok
            && res.problems[0].fix.as_deref() == Some("my/start dyn")
            && !res.output.contains("my-co-1")
    );
    f.runner.clear_rules();
    wire(&f.runner, vec![], "");
    let st = f.make_with(|a| a.policy = pol).status().await.unwrap();
    assert_eq!(st.problems[0].fix.as_deref(), Some("my/start dyn"));
}
