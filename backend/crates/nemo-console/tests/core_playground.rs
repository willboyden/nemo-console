//! Port of backend/tests/core/test_playground.py: the chat proxy clamps, streams, resolves the credential by env
//! NAME server-side, fails closed and never persists prompt text. Upstreams are wiremock servers (never :4010).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use common::{Env, FAKE_SECRET};
use nemo_console::domain::models::{Role, SystemId};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT: &str = "/api/v1/systems/switchyard/playground/chat";

fn msg() -> Value {
    json!([{"role": "user", "content": "hello"}])
}

async fn upstream(resp: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(resp)
        .mount(&server)
        .await;
    server
}

fn point(env: &Env, sid: SystemId, server: &MockServer) {
    env.a(sid).knobs().playground_url = Some(format!("{}/v1/chat/completions", server.uri()));
}

#[tokio::test]
async fn test_spec_and_no_playground_system() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Relay).knobs().no_playground = true;
    assert_eq!(
        env.get("/api/v1/systems/switchyard/playground")
            .send()
            .await
            .json()["default_model"],
        "m1"
    );
    assert_eq!(
        env.get("/api/v1/systems/relay/playground")
            .send()
            .await
            .json(),
        Value::Null
    );
    let r = env
        .post(
            "/api/v1/systems/relay/playground/chat",
            json!({"model": "m", "messages": msg()}),
        )
        .send()
        .await;
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn test_max_tokens_clamped_and_credential_from_env_name() {
    let server = upstream(
        ResponseTemplate::new(200)
            .set_body_json(json!({"ok": true}))
            .insert_header("x-switchyard-route", "auto-local")
            .insert_header("x-switchyard-tier", "capable"),
    )
    .await;
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    point(&env, SystemId::Switchyard, &server);
    let r = env
        .post(
            CHAT,
            json!({"model": "m1", "messages": msg(), "max_tokens": 999_999, "stream": false}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json(), json!({"ok": true}));
    assert_eq!(r.header("x-served-by-route").unwrap(), "auto-local");
    assert_eq!(r.header("x-served-by-tier").unwrap(), "capable");
    env.post(
        CHAT,
        json!({"model": "m1", "messages": msg(), "stream": false}),
    )
    .send()
    .await;
    let calls = server.received_requests().await.unwrap();
    let sent: Vec<Value> = calls
        .iter()
        .map(|c| serde_json::from_slice(&c.body).unwrap())
        .collect();
    assert_eq!(
        (
            sent[0]["max_tokens"].as_i64(),
            sent[1]["max_tokens"].as_i64()
        ),
        (Some(4096), Some(1024))
    );
    assert_eq!(
        calls[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {FAKE_SECRET}")
    );
    assert!(!Value::Array(sent).to_string().contains(FAKE_SECRET)); // never in the body
}

#[tokio::test]
async fn test_stream_passthrough() {
    let sse =
        format!("data: {{\"a\":1}}\n\ndata: {{\"leak\":\"{FAKE_SECRET}\"}}\n\ndata: [DONE]\n\n");
    let server =
        upstream(ResponseTemplate::new(200).set_body_raw(sse.into_bytes(), "text/event-stream"))
            .await;
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    point(&env, SystemId::Switchyard, &server);
    let r = env
        .post(
            CHAT,
            json!({"model": "m1", "messages": msg(), "stream": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("text/event-stream")
    );
    let t = r.text();
    assert!(t.contains("data: {\"a\":1}") && t.contains("[DONE]") && !t.contains(FAKE_SECRET));
}

#[tokio::test]
async fn test_missing_credential_is_503_naming_the_variable() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env
        .post(CHAT, json!({"model": "m1", "messages": msg()}))
        .send()
        .await;
    assert_eq!(r.status, 503);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("LITELLM_API_KEY")
    );
}

#[tokio::test]
async fn test_fails_closed_never_direct() {
    // the fake adapter's default playground URL is a closed port: the connection is refused
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    let r = env
        .post(
            CHAT,
            json!({"model": "m1", "messages": msg(), "stream": false}),
        )
        .send()
        .await;
    assert_eq!(r.status, 502);
    assert_eq!(r.code(), "upstream_unreachable");
}

#[tokio::test]
async fn test_request_validation() {
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    for body in [
        json!({"model": "bad model!", "messages": msg()}),
        json!({"model": "m1", "messages": []}),
        json!({"model": "m1", "messages": [{"role": "root", "content": "x"}]}),
        json!({"model": "m1", "messages": msg(), "max_tokens": 0}),
        json!({"model": "m1", "messages": msg(), "extra": 1}),
    ] {
        assert_eq!(
            env.post(CHAT, body.clone()).send().await.status,
            422,
            "{body}"
        );
    }
}

#[tokio::test]
async fn test_prompt_text_is_never_persisted_or_audited() {
    let server = upstream(ResponseTemplate::new(200).set_body_json(json!({"ok": true}))).await;
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    point(&env, SystemId::Switchyard, &server);
    env.post(
        CHAT,
        json!({"model": "m1", "messages": [{"role": "user", "content": "PRIVATE-PROMPT-TEXT"}], "stream": false}),
    )
    .send()
    .await;
    let audit = env.get("/api/v1/audit").send().await.text();
    assert!(
        !audit.contains("PRIVATE-PROMPT-TEXT") && audit.contains("1 messages"),
        "{audit}"
    );
    for f in std::fs::read_dir(&env.settings.state_dir).unwrap() {
        let p = f.unwrap().path();
        if p.is_file() {
            let data = std::fs::read(&p).unwrap();
            assert!(
                !data.windows(19).any(|w| w == b"PRIVATE-PROMPT-TEXT"),
                "{}",
                p.display()
            );
        }
    }
}

#[tokio::test]
async fn test_viewer_cannot_chat() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, key) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let r = env
        .post(CHAT, json!({"model": "m1", "messages": msg()}))
        .client("10.0.0.2")
        .token(&key)
        .send()
        .await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn test_relay_playground_sends_the_key_named_for_relay() {
    // 2026-10-17: the Relay gateway's /v1/* answers 401 without a bearer. The default names LITELLM_API_KEY for relay
    // too, resolved server-side like Switchyard's; a missing variable is a clear 503, never an anonymous call.
    let server = upstream(ResponseTemplate::new(200).set_body_json(json!({"ok": true}))).await;
    let env = Env::new(Some("http://127.0.0.1:9"));
    point(&env, SystemId::Relay, &server);
    let url = "/api/v1/systems/relay/playground/chat";
    let body = json!({"model": "m1", "messages": msg(), "stream": false});
    let r = env.post(url, body.clone()).send().await;
    assert_eq!(r.status, 503);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("LITELLM_API_KEY")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    let env = env.with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    let r = env.post(url, body).send().await;
    assert_eq!(r.status, 200);
    let calls = server.received_requests().await.unwrap();
    assert_eq!(
        calls[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {FAKE_SECRET}")
    );
}
