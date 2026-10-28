//! Port of backend/tests/core/test_redaction.py: a fake secret must not appear in ANY response, audit row, log line
//! or database file; the redaction patterns; JSON stays parseable after redaction.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use common::{Env, FAKE_SECRET, LogCapture};
use nemo_console::adapters::base::{AdapterError, CommandRunner, RunOpts};
use nemo_console::domain::models::SystemId;
use nemo_console::services::common::{KNOWN, find_secret, redact, redact_obj};
use nemo_console::services::environ::Environ;
use nemo_console::services::pyfmt::{JsonOpts, json_dumps_with};
use nemo_console::services::runner::SubprocessRunner;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[tokio::test]
async fn test_secret_never_leaks_anywhere() {
    let (cap, guard) = LogCapture::start();
    KNOWN.add(FAKE_SECRET);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"echo": format!("your key is {FAKE_SECRET}")})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("bad key {FAKE_SECRET}")))
        .mount(&server)
        .await;
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    env.a(SystemId::Switchyard).knobs().playground_url =
        Some(format!("{}/v1/chat/completions", server.uri()));
    let chat = "/api/v1/systems/switchyard/playground/chat";
    let mut seen: Vec<String> = Vec::new();
    let r = env
        .post(
            chat,
            json!({"model": "m1", "messages": [{"role": "user", "content": format!("my key is {FAKE_SECRET}")}],
                   "stream": false}),
        )
        .send()
        .await;
    seen.push(format!("{}{:?}", r.text(), r.headers));
    let r = env
        .post(chat, json!({"model": "m1", "messages": [{"role": "user", "content": "hi"}], "stream": false}))
        .send()
        .await;
    assert_eq!(r.status, 502);
    seen.push(r.text());
    for p in [
        "/api/v1/settings",
        "/api/v1/audit?limit=500",
        "/api/v1/overview",
        "/api/v1/systems/switchyard/status",
        "/api/v1/systems/switchyard/events",
        "/api/v1/health",
    ] {
        seen.push(env.get(p).send().await.text());
    }
    // a VALUE, not a name
    let r = env
        .put(
            "/api/v1/settings",
            json!({"playground_key_env": {"switchyard": FAKE_SECRET}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 422);
    seen.push(r.text());
    let r = env
        .put(
            "/api/v1/settings",
            json!({"urls": {"switchyard": format!("http://user:{FAKE_SECRET}@127.0.0.1:4010")}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 422);
    seen.push(r.text());
    seen.push(env.get("/api/v1/audit?limit=500").send().await.text());
    drop(guard);
    let calls = server.received_requests().await.unwrap();
    // it WAS used upstream
    assert_eq!(
        calls[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {FAKE_SECRET}")
    );
    let blob = seen.join("\n") + &cap.text();
    assert!(!blob.contains(FAKE_SECRET));
    let state = &env.settings.state_dir;
    assert!(!contains(
        &std::fs::read(state.join("console.db")).unwrap(),
        FAKE_SECRET
    ));
    if let Ok(wal) = std::fs::read(state.join("console.db-wal")) {
        assert!(!contains(&wal, FAKE_SECRET));
    }
}

#[tokio::test]
async fn test_status_problem_text_is_redacted() {
    // the fake adapter puts 'Bearer abcdefgh12345678' in a Problem; adapters are supposed to redact, the core must too
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env.get("/api/v1/systems/relay/status").send().await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["health"], "up");
    assert!(!r.text().contains("abcdefgh12345678") && r.text().contains("leaked"));
}

#[tokio::test]
async fn test_key_endpoint_returns_only_status_never_value() {
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    let s = env.get("/api/v1/settings").send().await.json();
    assert_eq!(
        s["tokens"]["switchyard"],
        format!("[set, {} chars]", FAKE_SECRET.chars().count())
    );
    assert!(!s.to_string().contains(FAKE_SECRET));
    let env = env.with_env(&[]);
    assert_eq!(
        env.get("/api/v1/settings").send().await.json()["tokens"]["switchyard"],
        "[unset]"
    );
}

#[tokio::test]
async fn test_adapter_crash_message_is_redacted() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Relay).knobs().status_error = Some(AdapterError::failed(
        "RuntimeError",
        format!("connect failed with token {FAKE_SECRET}"),
    ));
    KNOWN.add(FAKE_SECRET);
    env.state.status.clear();
    let r = env.get("/api/v1/systems/relay/status").send().await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["health"], "unknown");
    assert!(!r.text().contains(FAKE_SECRET));
    assert_eq!(r.json()["problems"][0]["code"], "console.adapter.error");
}

#[test]
fn test_redact_patterns() {
    let text = "Authorization: Bearer abcdefghijklmnop\napi_key = \"supersecretvalue\"\nhf_abcdefghijklmnopqrstuv \
                eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklm password: hunter2222 ok=1 max_tokens=4096";
    let out = redact(text);
    for leaked in [
        "abcdefghijklmnop",
        "supersecretvalue",
        "hf_abc",
        "eyJhbGci",
        "hunter2222",
    ] {
        assert!(!out.contains(leaked), "{leaked} in {out}");
    }
    assert!(out.contains("max_tokens=4096") && out.contains("ok=1"));
    let obj = redact_obj(
        &json!({"api_key": "x", "nested": {"Authorization": "y", "max_tokens": 5, "note": "Bearer zzzzzzzzzzzz"}}),
    );
    assert_eq!(obj["api_key"], "[redacted]");
    assert_eq!(obj["nested"]["Authorization"], "[redacted]");
    assert_eq!(obj["nested"]["max_tokens"], 5);
    assert!(!obj.to_string().contains("zzzz"));
}

#[test]
fn test_find_secret_allows_env_names() {
    assert!(
        find_secret("api_key_env = \"LITELLM_MASTER_KEY\"\nmodel = \"qwen3.6-35b-a3b\"\n")
            .is_none()
    );
    assert!(find_secret("api_key = \"LITELLM_MASTER_KEY\"").is_none());
    assert!(find_secret("api_key = \"abcdefghijk\"").is_some());
}

#[tokio::test]
async fn test_runner_output_is_redacted_by_the_real_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("bin");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(
        d.join("docker"),
        format!("#!/bin/sh\necho 'env FOO={FAKE_SECRET} Bearer abcdefghijklmnop'\necho err-token=abcdefghijk1 >&2\n"),
    )
    .unwrap();
    common::set_mode(&d.join("docker"), 0o755);
    KNOWN.add(FAKE_SECRET);
    let dir = d.to_string_lossy().into_owned();
    let runner = SubprocessRunner::with_environ(
        std::slice::from_ref(&dir),
        Environ::from_pairs(&[("PATH", &dir)]),
    );
    let res = runner
        .run(&["docker".to_owned(), "ps".to_owned()], RunOpts::default())
        .await
        .unwrap();
    assert_eq!(res.returncode, 0);
    assert!(!res.stdout.contains(FAKE_SECRET) && !res.stdout.contains("abcdefghijklmnop"));
    assert!(!res.stderr.contains("abcdefghijk1"));
}

// ── redaction must not corrupt JSON it is asked to scrub ──────────────────────────────────────────
// `json.dumps([{"Name": "/c", "Config": {"Env": entries}}], indent=4)`
fn inspect_like(entries: &[&str]) -> String {
    json_dumps_with(
        &json!([{"Name": "/c", "Config": {"Env": entries}}]),
        JsonOpts {
            indent: Some(4),
            ..JsonOpts::default()
        },
    )
}

#[test]
fn test_redaction_keeps_docker_inspect_json_parseable() {
    for entry in [
        "HF_TOKEN=", // empty value: nothing to hide, must survive intact
        "LITELLM_API_KEY=",
        "HF_TOKEN=hf_abcdefghijklmnopqrst", // prefixed secret inside a JSON string
        "SOME_PASSWORD=hunter2hunter2",     // plain secret inside a JSON string
        "MAX_JOBS=4",                       // not a secret at all
    ] {
        let out = redact(&inspect_like(&[entry, "TORCH_CUDA_ARCH_LIST=12.0"]));
        let doc: Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{entry}: not JSON ({e}): {out}"));
        let env = doc[0]["Config"]["Env"].as_array().unwrap();
        assert_eq!(env[env.len() - 1], "TORCH_CUDA_ARCH_LIST=12.0", "{entry}"); // neighbours untouched
        assert_eq!(env.len(), 2, "{entry}");
    }
}

#[test]
fn test_redaction_still_hides_secret_values_inside_json() {
    let out = redact(&inspect_like(&[
        "HF_TOKEN=hf_abcdefghijklmnopqrst",
        "SOME_PASSWORD=hunter2hunter2",
    ]));
    assert!(!out.contains("hf_abcdefghijklmnopqrst") && !out.contains("hunter2hunter2"));
    assert!(out.matches("[redacted]").count() >= 2);
}

#[test]
fn test_redaction_still_hides_non_json_forms() {
    for text in [
        "token=\"abcdefgh1234\"", // quoted
        "token='abcdefgh1234'",
        "token=\"abcdefgh1234", // UNTERMINATED quote: must still be hidden, not skipped
        "token=abcdefgh1234",   // bare
        "api_key: abcdefgh1234",
        "password=ab\"cdefgh1234", // a quote INSIDE the secret must not cut the redaction short
    ] {
        let out = redact(text);
        assert!(
            !out.contains("abcdefgh1234") && !out.contains("cdefgh1234"),
            "{out}"
        );
    }
}
