//! Port of backend/tests/core/test_security.py: Host/Origin/CSRF checks, auth, security headers on every answer,
//! body caps, the static allowlist and its revalidation headers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use common::{Env, assert_secure, env_with_router, mode_of};
use http_body_util::BodyExt;
use nemo_console::adapters::base::{AdapterError, EventStream, SystemAdapter};
use nemo_console::config::{NO_USER_ENV, user_env_file_in};
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::*;
use nemo_console::services::environ::Environ;
use serde_json::{Map, Value, json};

#[test]
fn test_user_env_file_is_disabled_in_tests() {
    // Python: the autouse fixture sets NEMO_CONSOLE_NO_USER_ENV=1 and a temp HOME, so user_env_file() is None.
    // Rust tests never read the process environment (settings come from `settings_for_test`); the switch itself:
    let home = "/tmp/x/home";
    assert_eq!(
        user_env_file_in(&Environ::from_pairs(&[(NO_USER_ENV, "1"), ("HOME", home)])),
        None
    );
    assert_eq!(
        user_env_file_in(&Environ::from_pairs(&[("HOME", home)])),
        Some(Path::new(home).join(".config/nemo-console/env"))
    );
    assert!(home.ends_with("home"));
}

#[tokio::test]
async fn test_bad_host_is_421() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env
        .get("/api/v1/health")
        .header("host", "evil.example:8793")
        .send()
        .await;
    assert_eq!(r.status, 421);
    assert_eq!(r.code(), "misdirected_request");
    assert_secure(&r);
}

#[tokio::test]
async fn test_cross_origin_post_is_403() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env
        .put("/api/v1/settings", json!({}))
        .header("origin", "http://evil.example")
        .send()
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(r.code(), "cross_origin");
    assert_secure(&r);
    let r = env
        .put("/api/v1/settings", json!({}))
        .header("origin", "null")
        .send()
        .await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn test_same_origin_is_allowed() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env
        .put("/api/v1/settings", json!({}))
        .header("origin", "http://127.0.0.1:8793")
        .send()
        .await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn test_missing_csrf_header_is_403_even_with_bearer() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env
        .put("/api/v1/settings", json!({}))
        .no_csrf()
        .send()
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(r.code(), "csrf_header_required");
    let (_, key) = env.state.auth.mint(Role::Admin, "").unwrap();
    let r = env
        .put("/api/v1/settings", json!({}))
        .no_csrf()
        .token(&key)
        .send()
        .await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn test_get_needs_no_csrf_header() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    assert_eq!(
        env.get("/api/v1/systems").no_csrf().send().await.status,
        200
    );
}

#[tokio::test]
async fn test_headers_on_401_and_404_and_405() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r401 = env.get("/api/v1/systems").client("10.1.2.3").send().await;
    assert_eq!(r401.status, 401);
    assert_eq!(r401.code(), "unauthorized");
    assert_secure(&r401);
    assert_eq!(r401.header("cache-control").unwrap(), "no-store");
    let r404 = env.get("/api/v1/systems/nope/status").send().await;
    let r404b = env.get("/api/v1/definitely-not-a-route").send().await;
    let r405 = env.req("POST", "/api/v1/systems").send().await;
    assert_eq!((r404.status, r404b.status, r405.status), (404, 404, 405));
    for r in [&r404, &r404b, &r405] {
        assert_secure(r);
        assert!(r.json().get("error").is_some());
    }
}

/// A relay adapter whose status() panics with a secret in the message (Python: `container.topology.get = boom`).
struct Boom;

#[async_trait]
impl SystemAdapter for Boom {
    fn id(&self) -> SystemId {
        SystemId::Relay
    }
    fn label(&self) -> String {
        "Boom".to_owned()
    }
    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        #[allow(clippy::panic)]
        {
            panic!("boom secret sk-FAKEFAKEFAKE1234567890")
        }
    }
    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        Err(AdapterError::NotImplemented)
    }
    async fn recent_events(&self, _limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(vec![])
    }
    fn follow_events(self: Arc<Self>, _since_ts: Option<f64>) -> EventStream {
        Box::pin(futures::stream::empty())
    }
    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        Ok(vec![])
    }
    fn config_path(&self, file_id: &str) -> Result<std::path::PathBuf, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }
    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }
    async fn validate_config(
        &self,
        file_id: &str,
        _c: &str,
    ) -> Result<ValidationResult, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }
    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }
    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        Ok(vec![])
    }
    async fn run_action(
        &self,
        id: &str,
        _p: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        Err(AdapterError::UnknownId(id.to_owned()))
    }
}

#[tokio::test]
async fn test_headers_on_validation_errors_and_500() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env
        .post(
            "/api/v1/systems/switchyard/config/routes/validate",
            json!({"content": 5}),
        )
        .send()
        .await;
    assert_eq!(r.status, 422);
    assert_secure(&r);
    // an unhandled failure inside a handler (Python: a raising topology service) is a 500 with the headers and
    // never the exception text
    let mut adapters: std::collections::HashMap<SystemId, Arc<dyn SystemAdapter>> = env
        .adapters
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v) as Arc<dyn SystemAdapter>))
        .collect();
    adapters.insert(SystemId::Relay, Arc::new(Boom));
    let mut settings = env.settings.clone();
    settings.state_dir = env.tmp().join("state-boom");
    let state = build_container(
        settings,
        Overrides {
            adapters,
            environ: Some(env.environ.clone()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let r = common::TestReq::new(state, "GET", "/api/v1/topology")
        .send()
        .await;
    assert_eq!(r.status, 500);
    assert!(!r.text().contains("sk-"));
    assert_secure(&r);
}

#[tokio::test]
async fn test_no_swagger_or_redoc() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    for p in ["/docs", "/redoc", "/docs/oauth2-redirect"] {
        let r = env.get(p).send().await;
        assert_eq!(r.status, 404, "{p}");
        assert_secure(&r);
    }
}

#[tokio::test]
async fn test_x_forwarded_for_disables_loopback_trust() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    assert_eq!(env.get("/api/v1/systems").send().await.status, 200);
    assert_eq!(
        env.get("/api/v1/health").send().await.json()["trust"],
        "loopback-admin"
    );
    let ff = |r: common::TestReq| r.header("x-forwarded-for", "8.8.8.8");
    assert_eq!(ff(env.get("/api/v1/systems")).send().await.status, 401);
    assert_eq!(
        ff(env.get("/api/v1/health")).send().await.json()["trust"],
        "bearer"
    );
}

#[tokio::test]
async fn test_bearer_keys_roles_and_wrong_key() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, viewer_key) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, admin_key) = env.state.auth.mint(Role::Admin, "").unwrap();
    let v = |r: common::TestReq| r.client("10.0.0.9").token(&viewer_key);
    assert_eq!(v(env.get("/api/v1/systems")).send().await.status, 200);
    let r = v(env.put("/api/v1/settings", json!({}))).send().await;
    assert_eq!(r.status, 403);
    assert_eq!(r.code(), "forbidden");
    assert_eq!(v(env.get("/api/v1/audit")).send().await.status, 403);
    assert_eq!(
        v(env.get("/api/v1/health")).send().await.json()["trust"],
        "bearer"
    );
    let a = |r: common::TestReq| r.client("10.0.0.9").token(&admin_key);
    assert_eq!(
        a(env.put("/api/v1/settings", json!({})))
            .send()
            .await
            .status,
        200
    );
    assert_eq!(a(env.get("/api/v1/audit")).send().await.status, 200);
    assert_eq!(
        env.get("/api/v1/systems")
            .client("10.0.0.9")
            .token("nck_wrong")
            .send()
            .await
            .status,
        401
    );
    // a bad bearer must not silently fall back to loopback trust
    assert_eq!(
        env.get("/api/v1/systems")
            .token("nck_wrong")
            .send()
            .await
            .status,
        401
    );
}

#[tokio::test]
async fn test_keys_are_stored_hashed_and_bootstrap_file_is_0600() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let path = env
        .state
        .auth
        .ensure_bootstrap_key()
        .unwrap()
        .expect("first start writes the key");
    assert_eq!(mode_of(&path), 0o600);
    let key = std::fs::read_to_string(&path).unwrap().trim().to_owned();
    assert!(env.state.auth.authenticate(&key).is_some());
    let db = std::fs::read(env.settings.state_dir.join("console.db")).unwrap();
    assert!(!db.windows(key.len()).any(|w| w == key.as_bytes()));
    assert!(env.state.auth.ensure_bootstrap_key().unwrap().is_none()); // only once
    assert_eq!(mode_of(&env.settings.state_dir), 0o700);
    assert_eq!(mode_of(&env.settings.state_dir.join("console.db")), 0o600);
}

#[tokio::test]
async fn test_body_cap_content_length_and_chunked() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let mut big = b"{".to_vec();
    big.extend(std::iter::repeat_n(b' ', 1024 * 1024 + 10));
    big.push(b'}');
    let r = env
        .req("PUT", "/api/v1/settings")
        .header("content-type", "application/json")
        .raw(&big)
        .send()
        .await;
    assert_eq!(r.status, 413);
    assert_secure(&r);
    // chunked: no Content-Length, 20 x 100 kB
    let chunks: Vec<Result<bytes::Bytes, std::io::Error>> = (0..20)
        .map(|_| Ok(bytes::Bytes::from(vec![b' '; 100_000])))
        .collect();
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings")
        .header("host", common::BASE_HOST)
        .header("x-console", "1")
        .header("content-type", "application/json")
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap();
    let resp =
        nemo_console::api::handle(Arc::clone(&env.state), Some("127.0.0.1".to_owned()), req).await;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let r = common::TestResp {
        status,
        headers,
        body,
    };
    assert_eq!(r.status, 413);
    assert_secure(&r);
}

#[tokio::test]
async fn test_static_allowlist_and_traversal() {
    let (env, _router) = env_with_router().await;
    let front = env.settings.frontend_dir.clone();
    std::fs::create_dir_all(front.join("js")).unwrap();
    std::fs::create_dir(front.join("dev")).unwrap();
    std::fs::write(front.join("index.html"), "<html>ok</html>").unwrap();
    std::fs::write(front.join("js/app.js"), "x").unwrap();
    std::fs::write(front.join("dev/mock.js"), "secret-dev").unwrap();
    std::fs::write(front.join("package.json"), "{}").unwrap();
    let outside = env.tmp().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    std::os::unix::fs::symlink(&outside, front.join("js/link.js")).unwrap();
    // the static shell needs no auth
    for (path, code) in [
        ("/", 200),
        ("/index.html", 200),
        ("/js/app.js", 200),
        ("/dev/mock.js", 404),
        ("/package.json", 404),
        ("/docs", 404),
        ("/redoc", 404),
        ("/js/link.js", 404),
    ] {
        let r = env.get(path).client("10.0.0.9").send().await;
        assert_eq!(r.status, code, "{path}");
        assert_secure(&r);
    }
    assert!(
        !env.get("/dev/mock.js")
            .client("10.0.0.9")
            .send()
            .await
            .text()
            .contains("secret-dev")
    );
}

#[tokio::test]
async fn test_static_files_are_revalidated_on_every_load() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let front = env.settings.frontend_dir.clone();
    std::fs::create_dir_all(front.join("js")).unwrap();
    std::fs::write(front.join("index.html"), "<html>ok</html>").unwrap();
    std::fs::write(front.join("js/format.js"), "export const a = 1;").unwrap();
    let mut etag = String::new();
    for path in ["/", "/index.html", "/js/format.js"] {
        let r = env.get(path).send().await;
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.header("cache-control").unwrap(), "no-cache", "{path}");
        etag = r.header("etag").unwrap();
        let again = env
            .get(path)
            .header("if-none-match", &format!("W/{etag}, \"other\""))
            .send()
            .await;
        assert_eq!(again.status, 304, "{path}");
        assert!(again.body.is_empty());
        assert_eq!(again.header("etag").unwrap(), etag);
        assert_eq!(again.header("cache-control").unwrap(), "no-cache");
        assert_secure(&again);
    }
    // changed: new size -> new etag
    std::fs::write(
        front.join("js/format.js"),
        "export const a = 1, fmtUnit = 2;",
    )
    .unwrap();
    let r = env
        .get("/js/format.js")
        .header("if-none-match", &etag)
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!(r.text().contains("fmtUnit"));
    assert_eq!(
        env.get("/api/v1/health")
            .send()
            .await
            .header("cache-control")
            .unwrap(),
        "no-store"
    );
}
