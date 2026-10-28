//! Port of backend/tests/datadesigner/test_dd_adapter.py: runner policy, status, the tool's own yaml config, config
//! scripts (CAS-backed), preview, report, check-models, show_config, records/files, events/metrics. Offline: fakes +
//! captured fixtures only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "datadesigner_helpers.rs"]
mod helpers;

use std::collections::HashMap;
use std::path::PathBuf;

use helpers::*;
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::datadesigner::CELL_CAP;
use nemo_console::domain::models::{Danger, Health, PostCheck, Role, Severity};
use nemo_console::services::environ::Environ;
use nemo_console::services::runner::{ALLOWED_BINARIES, DD_CREATE, SubprocessRunner, max_timeout};
use serde_json::{Value, json};

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

fn pts(m: &nemo_console::domain::models::MetricsSnapshot) -> HashMap<String, f64> {
    m.points.iter().map(|p| (p.key.clone(), p.value)).collect()
}

// ── runner policy ───────────────────────────────────────────────────────────────────────────────

#[test]
fn test_only_create_gets_the_long_cap() {
    assert_eq!(max_timeout(&argv(&[LAUNCHER, "create", "x"])), DD_CREATE);
    assert_eq!(DD_CREATE, 3600.0);
    for a in [
        argv(&[LAUNCHER, "preview", "x"]),
        argv(&[LAUNCHER, "validate", "x"]),
        argv(&[LAUNCHER, "check-models", "x"]),
        argv(&[LAUNCHER, "--version"]),
        argv(&[EXPORTER, "a.parquet", "b.jsonl"]),
    ] {
        assert_eq!(max_timeout(&a), 600.0, "{a:?}");
    }
}

#[test]
fn test_binaries_are_allowlisted() {
    assert!(ALLOWED_BINARIES.contains("data-designer-local"));
    assert!(ALLOWED_BINARIES.contains("data-designer-export"));
    let r = SubprocessRunner::with_environ(&[], Environ::from_pairs(&[("PATH", "/nonexistent")]));
    let e = r.resolve("data-designer").unwrap_err();
    assert!(e.to_string().contains("not allowed"));
}

// ── status ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_status_up_with_wrapper_and_yaml_config() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Up);
    assert_eq!(st.version.as_deref(), Some("0.9.3"));
    assert_eq!(st.components[0].name, LAUNCHER);
    assert!(
        st.components[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("OpenShell-contained")
    );
    assert!(
        st.summary.contains("2 scripts · 2 aliases"),
        "{}",
        st.summary
    );
    assert!(st.problems.is_empty(), "{:?}", st.problems);
    assert_eq!(st.endpoints[0].name, "router (generation)");
    assert_eq!(st.endpoints[0].auth, "bearer");
    assert_eq!(run.calls()[0], argv(&[LAUNCHER, "--version"]));
}

#[tokio::test]
async fn test_version_is_cached_between_polls() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    a.status().await.unwrap();
    a.status().await.unwrap();
    assert_eq!(
        run.calls()
            .iter()
            .filter(|c| c.last().unwrap() == "--version")
            .count(),
        1
    );
}

#[tokio::test]
async fn test_not_installed_is_down_with_the_lab_installer_fix() {
    let f = Fx::new().await;
    let a = f.adapter(&FakeRunner::new());
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dd.not_installed")
        .unwrap();
    let fix = p.fix.as_deref().unwrap();
    assert!(fix.contains("clients/data-designer/install.sh") && fix.contains("0.9.3"));
    let repo = f.path().join("lab");
    std::fs::create_dir_all(repo.join("clients/data-designer")).unwrap();
    std::fs::write(repo.join("clients/data-designer/install.sh"), "#!/bin/sh\n").unwrap();
    let b = f.adapter_with(
        &FakeRunner::new(),
        Opts {
            repo: Some(&repo),
            ..Opts::default()
        },
    );
    let st = b.status().await.unwrap();
    let q = st
        .problems
        .iter()
        .find(|p| p.code == "dd.not_installed")
        .unwrap();
    assert_eq!(
        q.fix.as_deref().unwrap(),
        format!(
            "{}/clients/data-designer/install.sh   # pinned data-designer 0.9.3 + the data-designer-local wrapper",
            repo.display()
        )
    );
}

#[tokio::test]
async fn test_router_down_is_degraded() {
    let f = Fx::new().await;
    let a = f.adapter_url(&standard_runner(), Opts::default(), CLOSED);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    assert!(codes(&st.problems).contains("dd.router_down"));
}

#[tokio::test]
async fn test_missing_key_file_is_a_warning() {
    let f = Fx::new().await;
    let a = f.adapter_with(
        &standard_runner(),
        Opts {
            key_file: false,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    let p = st.problems.iter().find(|p| p.code == "dd.no_key").unwrap();
    assert_eq!(p.severity, Severity::Warn);
    assert!(p.fix.as_deref().unwrap().contains("mint-key.sh"));
    assert!(!a.ctx().paths["dd_key_file"].exists());
}

#[tokio::test]
async fn test_existing_key_file_is_never_read() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let st = a.status().await.unwrap();
    assert!(!codes(&st.problems).contains("dd.no_key"));
    assert!(
        !serde_json::to_string(&st)
            .unwrap()
            .contains("sk-FAKE-not-a-real-key")
    );
}

#[tokio::test]
async fn test_missing_tool_config_dir_is_reported_not_created() {
    let f = Fx::new().await;
    let a = f.adapter_with(
        &standard_runner(),
        Opts {
            providers: false,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    assert!(codes(&st.problems).contains("dd.no_config_dir"));
    assert!(!f.path().join("home/.data-designer").exists());
}

#[tokio::test]
async fn test_no_scripts_dir_is_a_warning() {
    let f = Fx::new().await;
    let a = f.adapter_with(
        &standard_runner(),
        Opts {
            scripts: false,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    assert!(codes(&st.problems).contains("dd.no_scripts") && a.scripts().is_empty());
    assert!(a.actions().unwrap()[0].params[0].choices.is_none());
}

// ── script discovery and containment ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_only_allowlisted_script_files_are_offered() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    assert_eq!(a.scripts(), ["reviews_smoke.py", "tiny.yaml"]);
    assert_eq!(
        a.script_path("reviews_smoke.py")
            .unwrap()
            .file_name()
            .unwrap(),
        "reviews_smoke.py"
    );
}

#[tokio::test]
async fn test_script_path_refuses_anything_outside_the_list() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let long = "x".repeat(70);
    for name in [
        "../outside.py",
        "UPPER.py",
        "notes.txt",
        "subdir.py",
        "linked.py",
        "",
        "a/b.py",
        ".hidden.py",
        long.as_str(),
    ] {
        assert!(
            matches!(a.script_path(name), Err(AdapterError::UnknownId(_))),
            "{name}"
        );
        assert!(
            matches!(a.config_path(name), Err(AdapterError::UnknownId(_))),
            "{name}"
        );
    }
}

// ── the tool's own yaml config ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_yaml_config_is_parsed_and_api_key_is_never_surfaced() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let provs = a.providers().unwrap();
    let aliases = a.aliases().unwrap();
    assert_eq!(
        provs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["lab-router", "nvidia"]
    );
    assert_eq!(provs[0].endpoint, "http://127.0.0.1:4000/v1");
    assert!(
        provs
            .iter()
            .all(|p| !format!("{p:?}").contains("LITELLM_DD_KEY"))
    );
    assert_eq!(
        aliases.iter().map(|x| x.alias.as_str()).collect::<Vec<_>>(),
        ["lab-text", "lab-embed"]
    );
    assert_eq!(aliases[0].model, "glm-5.3-flash-next-local");
    assert_eq!(aliases[0].temperature, Some(0.7));
    assert_eq!(aliases[0].max_tokens, Some(1024.0));
    assert_eq!(aliases[1].temperature, None);
}

#[tokio::test]
async fn test_show_config_action_never_shows_key_fields() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let res = a
        .run_action("show_config", &params(json!({})))
        .await
        .unwrap();
    assert!(
        res.ok
            && res.output.contains("lab-router")
            && res.output.contains("glm-5.3-flash-next-local")
    );
    for no in ["api_key", "LITELLM_DD_KEY", "NVIDIA_API_KEY"] {
        assert!(!res.output.contains(no), "{no}");
    }
}

#[tokio::test]
async fn test_broken_yaml_is_zero_entries_never_an_error() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    std::fs::write(
        f.path().join("home/.data-designer/model_configs.yaml"),
        "{ not yaml !!!",
    )
    .unwrap();
    assert!(a.aliases().unwrap().is_empty());
    assert_eq!(a.status().await.unwrap().health, Health::Up);
}

#[tokio::test]
async fn test_yaml_toolshape_top_level_key_with_list_is_parsed() {
    let f = Fx::new().await;
    let dd = f.path().join("home/.data-designer");
    std::fs::create_dir_all(&dd).unwrap();
    std::fs::write(
        dd.join("model_providers.yaml"),
        "providers:\n- name: lab-router\n  endpoint: http://127.0.0.1:4000/v1\n  provider_type: openai\n  api_key: LITELLM_DD_KEY\n- name: nvidia\n  endpoint: https://integrate.api.nvidia.com/v1\n  provider_type: openai\n  api_key: NVIDIA_API_KEY\n",
    )
    .unwrap();
    std::fs::write(
        dd.join("model_configs.yaml"),
        "model_configs:\n- alias: lab-text\n  model: glm-5.3-flash-next-local\n  provider: lab-router\n  inference_parameters:\n    generation_type: chat-completion\n    temperature: 1.0\n    top_p: 0.95\n    max_tokens: 4096\n- alias: lab-lightning\n  model: nemotron-lightning\n  provider: lab-router\n",
    )
    .unwrap();
    let a = f.adapter_with(
        &standard_runner(),
        Opts {
            providers: false,
            ..Opts::default()
        },
    );
    let provs = a.providers().unwrap();
    let aliases = a.aliases().unwrap();
    assert_eq!(
        provs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["lab-router", "nvidia"]
    );
    assert!(provs.iter().all(|p| !format!("{p:?}").contains("api_key")));
    assert_eq!(
        aliases.iter().map(|x| x.alias.as_str()).collect::<Vec<_>>(),
        ["lab-text", "lab-lightning"]
    );
    assert_eq!(aliases[0].model, "glm-5.3-flash-next-local");
}

// ── config files ────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_config_files_specs_and_read() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let specs: HashMap<String, _> = a
        .config_files()
        .unwrap()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
    let mut ids: Vec<&String> = specs.keys().collect();
    ids.sort();
    assert_eq!(ids, ["reviews_smoke.py", "tiny.yaml"]);
    assert_eq!(specs["tiny.yaml"].format, "yaml");
    assert_eq!(specs["reviews_smoke.py"].format, "text");
    assert!(specs["reviews_smoke.py"].editable && specs["reviews_smoke.py"].authoritative);
    assert!(specs["reviews_smoke.py"].description.contains("EXECUTES"));
    let cf = a.read_config("reviews_smoke.py").await.unwrap();
    assert!(cf.content.contains("load_config_builder") && !cf.sha256.is_empty() && cf.mtime > 0.0);
}

#[tokio::test]
async fn test_validate_config_runs_the_cli_on_a_temp_copy() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    let res = a
        .validate_config("reviews_smoke.py", "# new content\n")
        .await
        .unwrap();
    let calls = run.calls();
    let call = calls
        .iter()
        .find(|c| c.get(1).map(String::as_str) == Some("validate"))
        .unwrap();
    let tmp = f.path().join("state/datadesigner/tmp");
    assert_eq!(call[0], LAUNCHER);
    assert!(call[2].starts_with(tmp.to_str().unwrap()) && call[2].ends_with(".py"));
    assert!(res.ok && res.authoritative && res.note.as_deref().unwrap().contains("EXECUTES"));
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

#[tokio::test]
async fn test_validate_config_reports_the_cli_errors_redacted() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(
        &[LAUNCHER, "validate"],
        ok_rc(
            "",
            1,
            "Config error at line 3\napi_key=sk-abcdefghijklmnop\n",
        ),
    );
    let a = f.adapter(&run);
    let res = a.validate_config("reviews_smoke.py", "bad").await.unwrap();
    assert!(!res.ok && res.authoritative);
    let msgs: Vec<&str> = res.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(msgs.contains(&"Config error at line 3") && msgs.len() <= 10);
    assert!(
        !serde_json::to_string(&res)
            .unwrap()
            .contains("sk-abcdefghijklmnop")
    );
}

#[tokio::test]
async fn test_validate_unknown_id_is_keyerror() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    assert!(matches!(
        a.validate_config("nope.py", "x").await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_post_apply_is_healthy_without_restart() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let r = a.post_apply("reviews_smoke.py", true).await.unwrap();
    assert!(r.ok && r.post_check == PostCheck::Healthy && !r.restarted);
}

#[tokio::test]
async fn test_script_executing_actions_are_disruptive_admin() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    let acts = a.actions().unwrap();
    let by: HashMap<&str, _> = acts.iter().map(|s| (s.id.as_str(), s)).collect();
    for id in ["preview", "create", "check_models"] {
        assert_eq!(by[id].danger, Danger::Disruptive, "{id}");
    }
    assert!(acts.iter().all(|s| s.role == Role::Admin));
}

// ── preview ─────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_preview_runs_the_cli_keeps_the_report_and_exports_records() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    let res = a
        .run_action(
            "preview",
            &params(json!({"script": "reviews_smoke.py", "num_records": 3})),
        )
        .await
        .unwrap();
    let calls = run.calls();
    let idx = calls
        .iter()
        .position(|c| c.get(1).map(String::as_str) == Some("preview"))
        .unwrap();
    let call = &calls[idx];
    let script = f
        .path()
        .join("home/Datasets/data-designer/scripts/reviews_smoke.py");
    assert_eq!(
        call[..5],
        argv(&[LAUNCHER, "preview", script.to_str().unwrap(), "-n", "3"])[..]
    );
    assert!(call.contains(&"--save-results".to_owned()));
    let o = call.iter().position(|x| x == "-o").unwrap();
    let run_dir = PathBuf::from(&call[o + 1]);
    assert_eq!(run_dir.parent().unwrap().file_name().unwrap(), "runs");
    assert_eq!(run.cwds()[idx].as_deref(), Some(run_dir.as_path()));
    assert!(calls.iter().find(|c| c[0] == EXPORTER).unwrap()[1].ends_with("dataset.parquet"));
    assert_eq!(mode_of(&run_dir), 0o700);
    assert_eq!(mode_of(&run_dir.join("run.json")), 0o600);
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(meta["verdict"], "succeeded");
    assert_eq!(meta["mode"], "preview");
    assert_eq!(meta["records"], 2);
    assert_eq!(meta["num_records_requested"], 3);
    assert_eq!(meta["preview_dir"], "preview_results_20261023_222454");
    assert_eq!(
        std::fs::read_to_string(run_dir.join("records.jsonl"))
            .unwrap()
            .matches('\n')
            .count(),
        2
    );
    assert!(res.output.contains("sample_records_browser.html") && res.ok);
    let ev = &a.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.kind, "dd.run");
    assert_eq!(ev.data["num_records"], 2);
    assert_eq!(ev.data["mode"], "preview");
    assert!(!serde_json::to_string(&ev.data).unwrap().contains("Books"));
}

#[tokio::test]
async fn test_preview_cli_failure_is_data() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(
        &[LAUNCHER, "preview"],
        ok_rc("", 1, "Config error: unknown model alias"),
    );
    let a = f.adapter(&run);
    let res = a
        .run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("unknown model alias"));
    let rid = a.recent_events(100).await.unwrap()[0].data["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let meta = run_json(&a, &rid);
    assert_eq!(meta["verdict"], "failed");
    assert!(
        meta["error"]
            .as_str()
            .unwrap()
            .contains("unknown model alias")
    );
}

#[tokio::test]
async fn test_preview_without_convertible_output_is_an_error() {
    let f = Fx::new().await;
    let run = standard_runner();
    run.on(&[EXPORTER], ok_rc("", 3, "boom"));
    let a = f.adapter(&run);
    let res = a
        .run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    assert!(!res.ok);
    let rid = a.recent_events(100).await.unwrap()[0].data["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(run_json(&a, &rid)["verdict"], "error");
}

// ── report / check-models ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_report_shows_a_markdown_table_of_the_first_records() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    a.run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    let rep = a
        .run_action("report", &params(json!({"run_id": ""})))
        .await
        .unwrap();
    assert!(rep.ok);
    for needle in [
        "| product_category | review |",
        "| --- | --- |",
        "Books",
        "records.jsonl",
    ] {
        assert!(rep.output.contains(needle), "{needle}: {}", rep.output);
    }
    let rep2 = a
        .run_action(
            "report",
            &params(json!({"run_id": "20990101T000000-000-nope"})),
        )
        .await
        .unwrap();
    assert!(!rep2.ok && rep2.output == "unknown run id");
}

#[tokio::test]
async fn test_check_models_passes_the_script_through() {
    let f = Fx::new().await;
    let run = standard_runner();
    let a = f.adapter(&run);
    let res = a
        .run_action("check_models", &params(json!({"script": "tiny.yaml"})))
        .await
        .unwrap();
    let calls = run.calls();
    let call = calls
        .iter()
        .find(|c| c.get(1).map(String::as_str) == Some("check-models"))
        .unwrap();
    assert!(call[0] == LAUNCHER && call[2].ends_with("tiny.yaml"));
    assert!(res.ok && res.output.contains("lab-router: ok"));
}

// ── records / files (API support) ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_records_page_paginates_and_caps_cell_size() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    a.run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    let rid = a.recent_events(100).await.unwrap()[0].data["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = a.records_page(&rid, 1, 0).await.unwrap();
    assert_eq!(page["run_id"], rid.as_str());
    assert_eq!(page["total"], 2);
    assert_eq!(page["columns"], json!(["product_category", "review"]));
    assert_eq!(page["rows"][0]["product_category"], "Books");
    assert_eq!(page["truncated"], false);
    let page2 = a.records_page(&rid, 50, 1).await.unwrap();
    assert_eq!(page2["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page2["rows"][0]["product_category"], "Electronics");
    assert_eq!(
        a.records_page(&rid, 50, 9).await.unwrap()["rows"],
        json!([])
    );
    let (rid2, run_dir2) = a.new_run_dir("reviews_smoke.py").unwrap();
    std::fs::write(
        run_dir2.join("run.json"),
        json!({"run_id": rid2, "records": 1, "verdict": "succeeded"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        run_dir2.join("records.jsonl"),
        json!({"c": "x".repeat(5000)}).to_string() + "\n",
    )
    .unwrap();
    let page4 = a.records_page(&rid2, 10, 0).await.unwrap();
    assert_eq!(page4["rows"][0]["c"].as_str().unwrap().len(), CELL_CAP);
    assert_eq!(page4["truncated"], true);
    assert!(matches!(
        a.records_page("20990101T000000-000-nope", 10, 0).await,
        Err(AdapterError::UnknownId(_))
    ));
    assert!(matches!(
        a.records_page("../escape", 10, 0).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_run_files_lists_relpaths_and_sizes() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    a.run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    let rid = a.recent_events(100).await.unwrap()[0].data["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = a.run_files(&rid).await.unwrap();
    let files = out["files"].as_array().unwrap();
    let rels: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    for want in [
        "records.jsonl",
        "run.json",
        "preview_results_20261023_222454/dataset.parquet",
        "preview_results_20261023_222454/sample_records/sample_records_browser.html",
    ] {
        assert!(rels.contains(&want), "{want}: {rels:?}");
    }
    let parquet = files
        .iter()
        .find(|f| f["path"].as_str().unwrap().ends_with("dataset.parquet"))
        .unwrap();
    assert_eq!(
        parquet["bytes"].as_u64().unwrap(),
        std::fs::metadata(fxp("preview-artifacts/dataset.parquet"))
            .unwrap()
            .len()
    );
    assert!(matches!(
        a.run_files("20990101T000000-000-nope").await,
        Err(AdapterError::UnknownId(_))
    ));
}

// ── events / metrics ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_metrics_and_runs_by_verdict() {
    let f = Fx::new().await;
    let a = f.adapter(&standard_runner());
    a.run_action("preview", &params(json!({"script": "reviews_smoke.py"})))
        .await
        .unwrap();
    let p = pts(&a.metrics().await.unwrap());
    assert_eq!(p["dd.scripts"], 2.0);
    assert_eq!(p["dd.runs"], 1.0);
    assert_eq!(p["dd.live_running"], 0.0);
    assert_eq!(p["dd.last_records"], 2.0);
    assert_eq!(p["dd.runs_by_verdict{verdict=succeeded}"], 1.0);
    assert_eq!(p["dd.runs_by_verdict{verdict=failed}"], 0.0);
}

#[tokio::test]
async fn test_a_running_run_left_by_a_previous_process_reads_as_interrupted() {
    let f = Fx::new().await;
    let gate = Gate::new();
    let run = standard_runner();
    run.on(&[LAUNCHER, "create"], gated(create_default(), &gate));
    let old = f.adapter(&run);
    old.run_action(
        "create",
        &params(json!({"script": "reviews_smoke.py", "num_records": 2})),
    )
    .await
    .unwrap();
    let new = f.adapter(&standard_runner());
    let ev = &new.recent_events(100).await.unwrap()[0];
    assert_eq!(ev.data["verdict"], "interrupted");
    assert_eq!(
        pts(&new.metrics().await.unwrap())["dd.runs_by_verdict{verdict=interrupted}"],
        1.0
    );
    gate.set();
    finish(&old).await;
}
