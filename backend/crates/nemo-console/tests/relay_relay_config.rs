//! Port of backend/tests/relay/test_relay_config.py.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod relay_support;
mod switchyard_support;

use std::path::Path;

use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::relay::{base_url_problem, validate_routes_semantics};
use nemo_console::domain::models::{Diagnostic, Severity};
use nemo_console::services::common::sha256_hex;
use relay_support::{Router, fx, make_adapter};

#[tokio::test]
async fn test_config_files_contract() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let specs = m.a.config_files().unwrap();
    let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["routes", "plugins", "config"]);
    let routes = &specs[0];
    assert!(routes.editable && !routes.authoritative && routes.format == "toml");
    assert!(routes.apply_effect.contains("restart"));
    for s in &specs[1..] {
        let why = s.why_readonly.clone().unwrap();
        assert!(!s.editable && why.contains("human decision") && why.contains("diff"));
    }
    assert!(
        specs
            .iter()
            .all(|s| !s.display_path.is_empty() && !s.display_path.starts_with("/home/"))
    );
}

#[tokio::test]
async fn test_config_path_and_read() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    assert!(matches!(
        m.a.config_path("nope"),
        Err(AdapterError::UnknownId(_))
    ));
    assert!(matches!(
        m.a.read_config("nope").await,
        Err(AdapterError::UnknownId(_))
    ));
    let cf = m.a.read_config("routes").await.unwrap();
    assert_eq!(cf.sha256, sha256_hex(fx("routes.toml").as_bytes()));
    assert!(cf.content == fx("routes.toml") && cf.spec.editable);
    let pl = m.a.read_config("plugins").await.unwrap();
    assert!(pl.content.contains("ed25519:") && !pl.spec.editable);
}

fn is_permission(r: &Result<nemo_console::domain::models::ConfigFile, AdapterError>) -> bool {
    matches!(r, Err(e) if e.py_class() == "PermissionError")
}

#[tokio::test]
async fn test_read_config_refuses_symlink_oversize_and_redacts() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let target = m.tmp.path().join("elsewhere.toml");
    std::fs::write(&target, "x = 1\n").unwrap();
    let p = m.a.config_path("config").unwrap();
    std::fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink(&target, &p).unwrap();
    assert!(is_permission(&m.a.read_config("config").await));
    std::fs::remove_file(&p).unwrap();
    std::fs::write(&p, "a".repeat(300 * 1024)).unwrap();
    assert!(is_permission(&m.a.read_config("config").await));
    std::fs::write(
        &p,
        "api_key = \"sk-abcdefghijklmnopqrstuvwxyz012345\"\ntoken = \"hunter2hunter2\"\nauth = \"Bearer abcdefghijkl123\"\n",
    )
    .unwrap();
    let cf = m.a.read_config("config").await.unwrap();
    assert!(!cf.content.contains("sk-abcdef") && !cf.content.contains("hunter2"));
    assert!(!cf.content.contains("abcdefghijkl123"));
    assert_eq!(cf.content.matches("[REDACTED]").count(), 3);
}

#[tokio::test]
async fn test_repo_routes_toml_validates_clean_but_non_authoritative() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let v =
        m.a.validate_config("routes", &fx("routes.toml"))
            .await
            .unwrap();
    assert!(v.ok, "{:?}", v.diagnostics);
    assert!(!v.authoritative);
    let note = v.note.unwrap();
    assert!(
        note.contains("switchyard-server --dry-run")
            && note.contains("NOT")
            && note.contains("restart")
    );
}

fn errs(text: &str) -> Vec<String> {
    validate_routes_semantics(text)
        .unwrap()
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect()
}

fn any(v: &[String], s: &str) -> bool {
    v.iter().any(|m| m.contains(s))
}

#[test]
fn test_semantic_rejections() {
    let base = fx("routes.toml");
    assert!(any(
        &errs(&base.replacen(
            "type = \"stage_router\"",
            "type = \"stage_router\"\ncapable_system_prompt = \"x\"",
            1
        )),
        "capable_system_prompt"
    ));
    assert!(any(
        &errs(&base.replacen("target = \"qwen-27b-1m\"", "target = \"missing\"", 1)),
        "not defined under [targets]"
    ));
    assert!(any(
        &errs(&base.replacen(
            "api_key_env = \"LITELLM_MASTER_KEY\"",
            "api_key_env = \"sk-live-1234567890abcdefgh\"",
            1
        )),
        "api_key_env"
    ));
    assert!(any(
        &errs(&base.replacen(
            "api_key_env = \"LITELLM_MASTER_KEY\"",
            "api_key = \"abc\"",
            1
        )),
        "literal API keys"
    ));
    assert!(any(
        &errs(&base.replace("schema_version = 1", "schema_version = 2")),
        "schema_version"
    ));
    assert!(any(
        &errs(&base.replacen(
            "confidence_threshold = 0.5",
            "confidence_threshold = 1.5",
            1
        )),
        "confidence_threshold"
    ));
    assert!(any(
        &errs(&base.replace("format = \"openai_chat\"", "format = \"grpc\"")),
        "format"
    ));
    assert!(any(
        &errs(&base.replace("id = \"direct-27b\"", "id = \"direct-lightning\"")),
        "duplicates"
    ));
}

#[test]
fn test_two_wire_formats_for_one_model_id_is_an_error() {
    // the judge alias exists precisely because the 0.3.0 plugin refuses this
    let bad = fx("routes.toml").replace(
        "id = \"nemotron-lightning-judge\"",
        "id = \"nemotron-lightning\"",
    );
    assert!(any(&errs(&bad), "two wire formats"));
}

#[test]
fn test_syntax_error_reports_line_and_unknown_field_is_a_warning() {
    let d: Vec<Diagnostic> =
        validate_routes_semantics("schema_version = 1\n[routes.x\nid = \"a\"\n").unwrap();
    assert!(d[0].severity == Severity::Error && d[0].line.is_some());
    let warn = validate_routes_semantics(&fx("routes.toml").replacen(
        "context_window = 262144",
        "context_window = 262144\nwarp = 9",
        1,
    ))
    .unwrap();
    assert!(
        warn.iter().any(|x| x.severity == Severity::Warn
            && x.message.contains("warp")
            && x.line.is_some())
    );
    assert!(!warn.iter().any(|x| x.severity == Severity::Error));
}

#[tokio::test]
async fn test_validate_flags_literal_secret_and_readonly_files() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let v =
        m.a.validate_config(
            "routes",
            &(fx("routes.toml") + "\n# sk-abcdefghijklmnopqrstuvwxyz012345\n"),
        )
        .await
        .unwrap();
    assert!(!v.ok);
    let v2 =
        m.a.validate_config("plugins", "version = 1\n")
            .await
            .unwrap();
    assert!(v2.ok && !v2.authoritative && v2.note.as_deref().unwrap().contains("PASS"));
    assert!(!m.a.validate_config("config", "= broken").await.unwrap().ok);
    assert!(matches!(
        m.a.validate_config("nope", "").await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[test]
fn test_base_url_local_targets_allowed() {
    for url in [
        "http://127.0.0.1:4000",
        "http://127.0.0.1:4000/v1",
        "http://litellm:4000/v1",
        "http://localhost:4000",
        "http://[::1]:4000/v1",
        "http://127.5.5.5",
        "http://127.0.0.1.:4000",
        "http://LiteLLM./v1",
        "https://localhost",
    ] {
        assert_eq!(base_url_problem(url), None, "{url}");
    }
}

#[test]
fn test_base_url_offbox_or_tricky_rejected() {
    for url in [
        "http://evil.com",
        "http://evil.com.",
        "http://127.0.0.1@evil.com",
        "http://127.0.0.1:4000@evil.com/v1",
        "http://user:pw@127.0.0.1:4000",
        "http://[::1]@evil.com",
        "http://[2001:db8::1]:4000",
        "http://[::ffff:8.8.8.8]",
        "http://10.0.0.5:4000",
        "http://172.16.1.7:14001/v1",
        "http://127.0.0.1.evil.com",
        "http://localhost.evil.com",
        "http://2130706433",
        "http://0x7f.1",
        "http://evil.com\\@127.0.0.1",
        "http://127.0.0.1 @evil.com",
        "http://:4000",
        "http://litellm:99999",
        "ftp://litellm",
        "http://[::1",
        "http://exa mple",
    ] {
        assert!(base_url_problem(url).is_some(), "{url}");
    }
}

#[test]
fn test_offbox_base_url_is_an_error_diagnostic() {
    let bad = fx("routes.toml").replacen(
        "base_url = \"http://127.0.0.1:4000/v1\"",
        "base_url = \"http://127.0.0.1@evil.com/v1\"",
        1,
    );
    assert!(any(&errs(&bad), "userinfo"));
    let off = fx("routes.toml").replacen(
        "base_url = \"http://127.0.0.1:4000\"",
        "base_url = \"https://api.example.com\"",
        1,
    );
    assert!(any(&errs(&off), "off-box"));
}

#[test]
fn test_repo_switchyard_server_routes_toml_validates_clean() {
    let real = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("inference/switchyard/routes.toml"))
        .find(|p| p.is_file());
    let Some(real) = real else {
        return; // no inference/switchyard/routes.toml in any parent directory (console cloned outside the lab repo)
    };
    let text = std::fs::read_to_string(real).unwrap();
    assert!(!errs(&text).iter().any(|m| m.contains("base_url")));
}
