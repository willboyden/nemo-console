//! Port of backend/tests/core/test_audit2.py (second independent audit: F1 audit JSON validity, F2 quadratic regex,
//! F3 raw-path traversal, F6 viewer redaction, F7 credential-name allowlist, rollback semaphore).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Env;
use nemo_console::domain::models::Role;
use nemo_console::services::common::{find_secret, redact};
use serde_json::{Value, json};

// F1 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_token_key_in_body_does_not_poison_audit() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, vk) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, ak) = env.state.auth.mint(Role::Admin, "").unwrap();
    let big: serde_json::Map<String, Value> = (0..100)
        .map(|i| (format!("k{i}"), json!("x".repeat(100))))
        .collect();
    let url = "/api/v1/systems/switchyard/actions/nosuch";
    let a = |r: common::TestReq| r.client("10.0.0.9").token(&ak);
    a(env.post(url, json!({"token": "x", "api_key": "y", "password": "z"})))
        .send()
        .await;
    a(env.post(url, Value::Object(big))).send().await;
    let r = a(env.get("/api/v1/audit?limit=500")).send().await;
    assert_eq!(r.status, 200, "{}", r.text());
    let rows = r.json();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|x| x["params"].is_object()));
    assert_eq!(rows[1]["params"]["token"], "[redacted]");
    // viewer can trigger it too, but is a denied count
    env.post(url, json!({"token": "x"}))
        .client("10.0.0.9")
        .token(&vk)
        .send()
        .await;
    assert_eq!(a(env.get("/api/v1/audit")).send().await.status, 200);
}

#[tokio::test]
async fn test_poisoned_row_does_not_break_list() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.state
        .store
        .execute(
            "INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
            rusqlite::params![1.0, "a", "admin", "POST", "/api/v1/x", Option::<String>::None, 200, "{\"token\": [redacted]"],
        )
        .unwrap();
    let r = env.get("/api/v1/audit").send().await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()[0]["params"], json!({"_unparseable": true}));
}

// F2 ------------------------------------------------------------------------------------------------
#[test]
fn test_redact_and_find_secret_are_not_quadratic() {
    for text in [
        "a_".repeat(32_768),
        "a".repeat(65_536),
        format!("token={}", "a_".repeat(40_000)),
        format!("-----BEGIN PRIVATE KEY-----{}", "x".repeat(65_536)),
    ] {
        let t0 = Instant::now();
        let _ = redact(&text);
        let _ = find_secret(&text);
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "{}: {:?}",
            &text[..12],
            t0.elapsed()
        );
    }
}

#[tokio::test]
async fn test_audit_write_of_pathological_body_is_fast() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, ak) = env.state.auth.mint(Role::Admin, "").unwrap();
    let body = json!({"v": "a_".repeat(30_000)});
    let t0 = Instant::now();
    env.post("/api/v1/systems/switchyard/actions/nosuch", body)
        .client("10.0.0.9")
        .token(&ak)
        .send()
        .await;
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}

// F3 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_static_rejects_dotdot_before_resolving() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let front = env.settings.frontend_dir.clone();
    for d in ["css", "js", "dev", "tests"] {
        std::fs::create_dir_all(front.join(d)).unwrap();
    }
    std::fs::write(front.join("index.html"), "shell").unwrap();
    std::fs::write(front.join("css/a.css"), "ok").unwrap();
    std::fs::write(front.join("dev/mock-server.mjs"), "DEV-SECRET").unwrap();
    std::fs::write(front.join("package.json"), "PKG-SECRET").unwrap();
    std::fs::write(front.join("tests/api.test.mjs"), "TEST-SECRET").unwrap();
    assert_eq!(
        env.get("/css/a.css").client("10.0.0.9").send().await.status,
        200
    );
    // raw paths, exactly as a client may send them (no normalisation on the way)
    for raw in [
        "/css/../dev/mock-server.mjs",
        "/js/../package.json",
        "/js/../tests/api.test.mjs",
        "/css/%2e%2e/dev/mock-server.mjs",
        "/css/..%2fdev/mock-server.mjs",
        "/css/%2E%2E%2Fdev/mock-server.mjs",
        "/css//a.css",
        "/css\\..\\dev\\mock-server.mjs",
        "/css/%252e%252e/dev/mock-server.mjs",
        "/css/./a.css",
    ] {
        let r = env.get(raw).client("10.0.0.9").send().await;
        assert_eq!(r.status, 404, "{raw}");
        assert!(!r.text().contains("SECRET"), "{raw}");
    }
}

// F6 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_viewer_gets_redacted_config_admin_gets_raw() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    std::fs::write(
        &env.cfg_file,
        "api_key = \"literalsecretvalue123\"\nname = \"ok\"\n",
    )
    .unwrap();
    let (_, vk) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, ak) = env.state.auth.mint(Role::Admin, "").unwrap();
    let url = "/api/v1/systems/switchyard/config/routes";
    let v = env
        .get(url)
        .client("10.0.0.9")
        .token(&vk)
        .send()
        .await
        .json();
    assert!(!v.to_string().contains("literalsecretvalue123"));
    assert!(v["content"].as_str().unwrap().contains("name = \"ok\""));
    let a = env
        .get(url)
        .client("10.0.0.9")
        .token(&ak)
        .send()
        .await
        .json();
    assert!(
        a["content"]
            .as_str()
            .unwrap()
            .contains("literalsecretvalue123")
    );
    assert_eq!(v["sha256"], a["sha256"]); // CAS hash still of the raw file
}

// F7 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_playground_key_env_must_be_allowlisted() {
    let env =
        Env::new(Some("http://127.0.0.1:9")).with_env(&[("SOME_PRIVATE_THING", "topsecretvalue")]);
    for bad in [
        "SOME_PRIVATE_THING",
        "PATH",
        "HOME",
        "AWS_SECRET_ACCESS_KEY",
        "GITHUB_TOKEN",
    ] {
        let r = env
            .put(
                "/api/v1/settings",
                json!({"playground_key_env": {"switchyard": bad}}),
            )
            .send()
            .await;
        assert_eq!(r.status, 422, "{bad}");
    }
    for good in [
        "LITELLM_MASTER_KEY",
        "NEMO_CONSOLE_PLAYGROUND_X",
        "MY_SERVICE_API_KEY",
    ] {
        let r = env
            .put(
                "/api/v1/settings",
                json!({"playground_key_env": {"switchyard": good}}),
            )
            .send()
            .await;
        assert_eq!(r.status, 200, "{good}");
    }
    assert!(
        !env.get("/api/v1/settings")
            .send()
            .await
            .text()
            .contains("topsecretvalue")
    );
}

#[tokio::test]
async fn test_env_supplied_disallowed_key_name_is_ignored() {
    let env = Env::new(Some("http://127.0.0.1:9")).with_env(&[("PATH_LEN_PROBE", "x")]);
    env.state
        .store
        .execute(
            "INSERT INTO settings(k, v) VALUES ('overrides', ?)",
            [json!({"playground_key_env": {"switchyard": "HOME"}}).to_string()],
        )
        .unwrap();
    assert_eq!(
        env.state.settings.effective_key_env()["switchyard"],
        "LITELLM_API_KEY"
    );
}

#[tokio::test]
async fn test_url_userinfo_redacted_in_audit() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.put(
        "/api/v1/settings",
        json!({"urls": {"dynamo": "http://user:pw123456@127.0.0.1:1"}}),
    )
    .send()
    .await;
    env.post(
        "/api/v1/systems/switchyard/actions/nosuch",
        json!({"note": "see https://bob:hunter22@example.com/x and ok"}),
    )
    .send()
    .await;
    let rows = env.get("/api/v1/audit").send().await.text();
    assert!(
        !rows.contains("pw123456") && !rows.contains("hunter22") && !rows.contains("bob"),
        "{rows}"
    );
}

// rollback shares the validate semaphore ------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_rollback_validation_takes_the_semaphore() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let permits = Arc::clone(&env.state.validate_sem)
        .acquire_many_owned(2)
        .await
        .unwrap();
    env.state
        .store
        .execute(
            "INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
            rusqlite::params!["bb000001", "switchyard", "routes", 1.0, "t", "0".repeat(64), "", b"a = 2\n".to_vec()],
        )
        .unwrap();
    let req = env.post(
        "/api/v1/systems/switchyard/config/routes/rollback",
        json!({"revision_id": "bb000001", "confirm": true}),
    );
    let t = tokio::spawn(req.send());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!t.is_finished());
    drop(permits);
    let r = tokio::time::timeout(Duration::from_secs(3), t)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, 200);
}
