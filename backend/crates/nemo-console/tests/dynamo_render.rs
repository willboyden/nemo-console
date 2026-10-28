//! Port of backend/tests/dynamo/test_render.py: override rendering / validation (pure functions, golden output) and
//! the compose-merge proof (offline + the real `docker compose config`, read-only, scrubbed environment).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod dynamo_support;
mod switchyard_support;

use dynamo_support::{fx, s};
use nemo_console::adapters::base::AdapterError;
use nemo_console::adapters::dynamo::{
    catalog, load_base_commands, param_warnings, parse_values_header, render_override,
    validate_values,
};
use nemo_console::services::pyyaml::{PyValue, safe_load_value as safe_load};
use serde_json::{Map, Value, json};

fn base() -> (Vec<String>, Vec<String>) {
    load_base_commands(&fx("base_compose.yml"), "dynamo-worker", "dynamo-frontend").unwrap()
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn golden_values() -> Map<String, Value> {
    obj(json!({
        "max_model_len": 131072, "gpu_memory_utilization": 0.6, "num_speculative_tokens": 3,
        "enable_prefix_caching": false, "router_mode": "kv", "reasoning_parser": "none",
    }))
}

fn render(values: Value) -> (String, Vec<String>) {
    let (w, f) = base();
    render_override(
        &obj(values),
        &w,
        Some(&f),
        "dynamo-worker",
        "dynamo-frontend",
    )
    .unwrap()
}

fn worker_cmd(text: &str) -> Vec<String> {
    let doc = safe_load(text).unwrap();
    match doc
        .get("services")
        .and_then(|s| s.get("dynamo-worker"))
        .and_then(|w| w.get("command"))
    {
        Some(PyValue::List(a)) => a.iter().map(PyValue::py_str).collect(),
        other => panic!("{other:?}"),
    }
}

fn at(cmd: &[String], flag: &str) -> String {
    cmd[cmd.iter().position(|c| c == flag).unwrap() + 1].clone()
}

#[test]
fn test_golden_override_matches_fixture() {
    let (w, f) = base();
    let (text, warns) = render_override(
        &golden_values(),
        &w,
        Some(&f),
        "dynamo-worker",
        "dynamo-frontend",
    )
    .unwrap();
    assert_eq!(text, fx("golden_override.yml"));
    assert_eq!(warns.len(), 2);
}

#[test]
fn test_base_interpolations_stay_live_not_escaped() {
    // regression: escaping `$` froze `${MAX_NUM_SEQS:-64}` into literal text inside the container
    let (text, _) = render(json!({"max_model_len": 4096}));
    assert!(text.contains("\"${MAX_NUM_SEQS:-64}\"") && !text.contains("$$"));
}

#[test]
fn test_only_supplied_keys_change_and_unmanaged_flags_survive() {
    let (text, _) = render(json!({"max_num_seqs": 8}));
    let cmd = worker_cmd(&text);
    assert_eq!(at(&cmd, "--max-num-seqs"), "8");
    assert_eq!(at(&cmd, "--dyn-tool-call-parser"), "qwen3_coder");
    assert!(cmd.iter().any(|c| c == "--dyn-enable-structural-tag"));
    assert!(cmd.iter().any(|c| c == "--enable-prefix-caching"));
    assert!(at(&cmd, "--model").ends_with("16b6615af3548b88e2d8e382457bc705b00479cf"));
    let doc = safe_load(&text).unwrap();
    assert!(
        doc.get("services")
            .unwrap()
            .get("dynamo-frontend")
            .is_none()
    ); // round-robin: frontend untouched
}

#[test]
fn test_speculative_off_and_count() {
    let off = worker_cmd(&render(json!({"speculative_enabled": false})).0);
    assert!(!off.iter().any(|c| c == "--speculative-config"));
    let n = worker_cmd(&render(json!({"num_speculative_tokens": 4})).0);
    assert_eq!(
        at(&n, "--speculative-config"),
        r#"{"method":"mtp","num_speculative_tokens":4}"#
    );
}

#[test]
fn test_structural_tag_off_warns_and_removes_flag() {
    let (text, warns) = render(json!({"enable_structural_tag": false}));
    assert!(
        !worker_cmd(&text)
            .iter()
            .any(|c| c == "--dyn-enable-structural-tag")
    );
    assert!(warns.iter().any(|w| w.contains("tool_choice")));
    assert!(text.contains("# WARNING:"));
}

#[test]
fn test_invalid_values_rejected() {
    for values in [
        json!({"nope": 1}),
        json!({"max_model_len": true}),
        json!({"max_model_len": 10}),
        json!({"gpu_memory_utilization": 1.5}),
        json!({"gpu_memory_utilization": "x"}),
        json!({"tool_call_parser": "rm -rf /"}),
        json!({"enable_prefix_caching": "yes"}),
        json!({"router_mode": "direct"}),
        json!({"max_num_seqs": 1.5}),
        json!({"kv_cache_memory_bytes": 5}),
    ] {
        assert!(
            matches!(
                validate_values(&obj(values.clone())),
                Err(AdapterError::Invalid(_))
            ),
            "{values}"
        );
    }
}

#[test]
fn test_kv_and_gpu_util_warning_and_header_roundtrip() {
    let w = param_warnings(&obj(
        json!({"kv_cache_memory_bytes": 1, "gpu_memory_utilization": 0.5}),
    ));
    assert!(
        w.iter()
            .any(|x| x.contains("ignores gpu_memory_utilization"))
    );
    let (w2, f) = base();
    let (text, _) = render_override(
        &golden_values(),
        &w2,
        Some(&f),
        "dynamo-worker",
        "dynamo-frontend",
    )
    .unwrap();
    assert_eq!(
        parse_values_header(&text),
        validate_values(&golden_values()).unwrap()
    );
}

#[test]
fn test_catalog_flags_come_from_the_pinned_image_help() {
    // Every catalog flag (and every enum choice) must appear in the captured `--help` of that image.
    let helps = fx("help_vllm.txt") + &fx("help_frontend.txt");
    for p in catalog() {
        if let Some(flag) = &p.flag {
            assert!(helps.contains(flag.as_str()), "{flag}");
        }
        if ["tool_call_parser", "reasoning_parser", "router_mode"].contains(&p.key.as_str()) {
            for c in p.choices.clone().unwrap_or_default() {
                assert!(c == "none" || helps.contains(c.as_str()), "{} {c}", p.key);
            }
        }
    }
    assert!(helps.contains("--kv-cache-memory-bytes") && !helps.contains("--kv-cache-memory "));
}

#[test]
fn test_compose_merge_replaces_command_proof_offline() {
    // `command` in an override REPLACES the base list (compose semantics); see the live test for the real binary.
    let (text, _) = render(golden_values().into());
    let PyValue::Dict(top) = safe_load(&text).unwrap() else {
        panic!()
    };
    assert_eq!(
        top.iter().map(|(k, _)| k.py_str()).collect::<Vec<_>>(),
        ["services"]
    );
    let PyValue::Dict(svcs) = &top[0].1 else {
        panic!()
    };
    let names: Vec<String> = svcs.iter().map(|(k, _)| k.py_str()).collect();
    assert_eq!(names, ["dynamo-worker", "dynamo-frontend"]);
    for (_, v) in svcs {
        let PyValue::Dict(m) = v else { panic!() };
        assert_eq!(
            m.iter().map(|(k, _)| k.py_str()).collect::<Vec<_>>(),
            ["command"]
        );
    }
}

#[test]
fn test_docker_compose_config_renders_override() {
    // READ-ONLY proof with the real `docker compose config` (no containers touched), environment scrubbed.
    let Some(docker) = ["/usr/local/bin/docker", "/usr/bin/docker"]
        .iter()
        .find(|p| std::path::Path::new(p).is_file())
    else {
        return; // docker CLI not available
    };
    let tmp = tempfile::tempdir().unwrap();
    let base_p = tmp.path().join("docker-compose.yml");
    std::fs::write(&base_p, fx("base_compose.yml")).unwrap();
    let ov = tmp.path().join("override.yml");
    std::fs::write(&ov, render(golden_values().into()).0).unwrap();
    let out = std::process::Command::new(docker)
        .args(["compose", "--project-directory"])
        .arg(tmp.path())
        .arg("-f")
        .arg(&base_p)
        .arg("-f")
        .arg(&ov)
        .arg("config")
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success()
        && ((stderr.contains("ai-lab") && stderr.contains("network"))
            || stderr.contains("is not a docker command"))
    {
        return; // external network ai-lab not resolvable here, or no compose plugin
    }
    assert!(out.status.success(), "{stderr}");
    let merged = safe_load(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let svcs = merged.get("services").unwrap();
    let wc: Vec<String> = match svcs.get("dynamo-worker").unwrap().get("command") {
        Some(PyValue::List(a)) => a.iter().map(PyValue::py_str).collect(),
        _ => panic!(),
    };
    assert_eq!(at(&wc, "--max-model-len"), "131072");
    assert_eq!(at(&wc, "--max-num-seqs"), "64"); // base ${MAX_NUM_SEQS:-64} resolved, not literal
    assert!(wc.iter().any(|c| c == "--no-enable-prefix-caching"));
    assert!(!wc.iter().any(|c| c == "--dyn-reasoning-parser"));
    let fc: Vec<String> = match svcs.get("dynamo-frontend").unwrap().get("command") {
        Some(PyValue::List(a)) => a.iter().map(PyValue::py_str).collect(),
        _ => panic!(),
    };
    assert_eq!(fc[fc.len() - 2..], s(&["--router-mode", "kv"]));
    let w = svcs.get("dynamo-worker").unwrap();
    assert!(
        w.get("volumes").is_some_and(PyValue::truthy)
            && w.get("deploy").is_some_and(PyValue::truthy)
    );
}
