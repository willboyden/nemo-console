//! Replays tests/fixtures/api_golden.json (generated from the real Python app by parity/gen/gen_api_golden.py) through
//! the Rust pipeline: status + body must match. Pins FastAPI's order (JSON parse -> dependencies -> validation),
//! pydantic's texts and locations, the error envelope and the 404/405 behaviour of the catch-all route.
mod common;

use common::Env;
use nemo_console::domain::models::Role;
use serde_json::Value;

/// Cases whose Python answer depends on the real adapters (system health) rather than the core: status only.
const STATUS_ONLY: &[&str] = &["no_key_health"];

/// The version the Python reference reported when the golden was generated.
const GOLDEN_VERSION: &str = "0.3.1";

#[tokio::test]
async fn test_api_golden_replays_python_answers() {
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/api_golden.json")).expect("golden fixture");
    let closed = "http://127.0.0.1:9";
    let urls: serde_json::Map<String, Value> = [
        "switchyard",
        "relay",
        "dynamo",
        "dynamo_worker_metrics",
        "litellm",
        "dcgm",
        "prometheus",
        "guardrails",
    ]
    .iter()
    .map(|k| ((*k).to_owned(), Value::String(closed.to_owned())))
    .collect();
    let env = Env::with_settings(
        None,
        serde_json::json!({"trust_loopback": false, "urls": urls}),
    );
    let (_, admin) = env.state.auth.mint(Role::Admin, "").unwrap();
    let (_, viewer) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let mut failures = Vec::new();
    for case in golden["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut r = env.req(
            case["method"].as_str().unwrap(),
            case["path"].as_str().unwrap(),
        );
        match case["role"].as_str().unwrap() {
            "admin" => r = r.token(&admin),
            "viewer" => r = r.token(&viewer),
            _ => {}
        }
        if let Some(ct) = case["content_type"].as_str() {
            r = r.header("content-type", ct);
        }
        if let Some(b) = case["body"].as_str() {
            let bytes = if name == "json_bad_utf8" {
                b"{\"content\": \"\xff\"}".to_vec()
            } else {
                b.as_bytes().to_vec()
            };
            r = r.raw(&bytes);
        }
        let resp = r.send().await;
        let want_status = case["status"].as_u64().unwrap();
        if u64::from(resp.status) != want_status {
            failures.push(format!(
                "{name}: status {} != {want_status} ({})",
                resp.status,
                resp.text()
            ));
            continue;
        }
        if STATUS_ONLY.contains(&name) {
            continue;
        }
        let mut want = case["response"].clone();
        let mut got: Value = serde_json::from_slice(&resp.body)
            .unwrap_or_else(|_| serde_json::json!({"_text": resp.text()}));
        // the golden was recorded from the Python reference (0.3.1); each release reports its own version
        if let Some(o) = want.as_object_mut()
            && o.get("version").and_then(Value::as_str) == Some(GOLDEN_VERSION)
        {
            o.insert(
                "version".to_owned(),
                Value::from(nemo_console::config::VERSION),
            );
        }
        // volatile: the admin settings view shows the (temp) state dir
        for v in [&mut want, &mut got] {
            if let Some(o) = v.as_object_mut()
                && o.contains_key("state_dir")
            {
                o.insert("state_dir".to_owned(), Value::String("<state>".to_owned()));
            }
        }
        let want = &want;
        if &got != want {
            failures.push(format!("{name}:\n   got  {got}\n   want {want}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} golden mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
