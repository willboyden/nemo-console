//! Port of backend/tests/core/test_audit_fixes.py (security-audit fixes F1, F4, F5, F6a-e).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use common::{Env, LogCapture, TestReq};
use futures::{FutureExt, StreamExt};
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::config::{Settings, settings_for_test};
use nemo_console::container::AppState;
use nemo_console::domain::models::*;
use nemo_console::services::audit::DenialLog;
use nemo_console::services::events::{Shutdown, SseLimiter, event_stream};
use serde_json::{Value, json};

const URL: &str = "/api/v1/systems/switchyard/config/routes";

/// Python `_app(env, **kw)`: a fresh app over the env's fakes with these settings (loopback trust OFF by default).
fn app(env: &Env, extra: Value) -> (Arc<AppState>, Settings) {
    let mut init = json!({
        "state_dir": env.tmp().join(format!("s{}", uuid::Uuid::new_v4().simple())).to_string_lossy(),
        "frontend_dir": env.tmp().join("front").to_string_lossy(),
        "repo_root": env.tmp().join("repo").to_string_lossy(),
        "background_tasks": false,
    });
    if let (Value::Object(base), Value::Object(more)) = (&mut init, extra) {
        base.extend(more);
    }
    let cfg = settings_for_test(init);
    (
        Env::state_for(&cfg, &env.adapters, &env.runner, &env.http, &env.environ),
        cfg,
    )
}

// F1 ------------------------------------------------------------------------------------------------
#[test]
fn test_loopback_trust_defaults_off() {
    assert!(!settings_for_test(json!({})).trust_loopback);
}

#[tokio::test]
async fn test_default_is_public_role_and_needs_a_key() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (st, _) = app(&env, json!({}));
    let h = TestReq::new(Arc::clone(&st), "GET", "/api/v1/health")
        .send()
        .await
        .json();
    assert_eq!(
        (h["role"].as_str(), h["trust"].as_str()),
        (Some("public"), Some("bearer"))
    );
    assert!(h["trust_notice"].is_null());
    assert_eq!(
        TestReq::new(Arc::clone(&st), "GET", "/api/v1/systems")
            .send()
            .await
            .status,
        401
    );
    assert_eq!(
        TestReq::new(st, "PUT", "/api/v1/settings")
            .json(json!({}))
            .send()
            .await
            .status,
        401
    );
}

#[tokio::test]
async fn test_loopback_trust_refused_when_exposure_plausible() {
    for kw in [
        json!({"allowed_hosts": ["console.lan:8793"]}),
        json!({"host": "0.0.0.0"}),
    ] {
        let env = Env::new(Some("http://127.0.0.1:9"));
        let (cap, guard) = LogCapture::start();
        let mut extra = kw.clone();
        extra["trust_loopback"] = json!(true);
        let (st, _) = app(&env, extra);
        let h = TestReq::new(Arc::clone(&st), "GET", "/api/v1/health")
            .header("host", "127.0.0.1:8793")
            .send()
            .await;
        let h = h.json();
        assert_eq!(h["trust"], "bearer", "{kw}");
        assert!(
            h["trust_notice"]
                .as_str()
                .unwrap()
                .contains("loopback trust disabled"),
            "{kw}"
        );
        assert_eq!(
            TestReq::new(st, "GET", "/api/v1/systems")
                .send()
                .await
                .status,
            401
        );
        drop(guard);
        assert!(
            cap.text().contains("loopback trust disabled"),
            "{kw}: {}",
            cap.text()
        );
    }
}

#[tokio::test]
async fn test_explicit_opt_in_still_works_on_plain_loopback() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (st, _) = app(&env, json!({"trust_loopback": true}));
    assert_eq!(
        TestReq::new(st, "GET", "/api/v1/health")
            .send()
            .await
            .json()["role"],
        "admin"
    );
}

// F4 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_denied_requests_are_not_audit_rows_but_are_counted() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, vk) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, ak) = env.state.auth.mint(Role::Admin, "").unwrap();
    for _ in 0..5 {
        env.put("/api/v1/settings", json!({}))
            .client("10.0.0.9")
            .send()
            .await; // 401
    }
    env.put("/api/v1/settings", json!({}))
        .client("10.0.0.9")
        .header("host", "evil:1")
        .send()
        .await; // 421
    let r = env
        .put("/api/v1/settings", json!({}))
        .client("10.0.0.9")
        .token(&vk)
        .send()
        .await;
    assert_eq!(r.status, 403); // viewer denied
    env.put("/api/v1/settings", json!({}))
        .no_csrf()
        .send()
        .await; // 403 csrf
    let n: i64 = env
        .state
        .store
        .with(|db| db.query_row("SELECT COUNT(*) n FROM audit", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(env.state.denials.total(401), 5);
    assert_eq!(env.state.denials.total(421), 1);
    let a = |r: TestReq| r.client("10.0.0.9").token(&ak);
    assert_eq!(
        a(env.put("/api/v1/settings", json!({})))
            .send()
            .await
            .status,
        200
    ); // authorised: one row
    let rows = a(env.get("/api/v1/audit")).send().await.json();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert!(rows[0]["actor"].as_str().unwrap().starts_with("key:"));
    let metrics = a(env.get("/metrics")).send().await.text();
    assert!(
        metrics.contains("nemo_console_denied_requests_total{status=\"401\"} 5.0"),
        "{metrics}"
    );
}

#[test]
fn test_denial_log_is_bounded_and_logs_one_line_per_interval() {
    let (cap, guard) = LogCapture::start();
    let d = DenialLog::new(0.0, 10);
    for _ in 0..500 {
        d.note(401, "PUT");
    }
    assert_eq!(d.ring_len(), 10);
    let d2 = DenialLog::new(3600.0, 100);
    let before = cap.text().len();
    for _ in 0..50 {
        d2.note(403, "POST");
    }
    drop(guard);
    assert_eq!(cap.text().len(), before, "nothing logged per request"); // nothing per request
}

// F5 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_rollback_validates_and_scans_the_target_revision() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let insert = |rid: &str, content: &[u8]| {
        env.state
            .store
            .execute(
                "INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
                rusqlite::params![rid, "switchyard", "routes", 1.0, "t", "0".repeat(64), "", content.to_vec()],
            )
            .unwrap();
    };
    for (rid, content, code) in [
        ("aa000001", "INVALID = 1\n", "validation_failed"),
        (
            "aa000002",
            "api_key = \"sk-abcdefghijklmnop1234\"\n",
            "secret_in_content",
        ),
    ] {
        insert(rid, content.as_bytes());
        let r = env
            .post(
                &format!("{URL}/rollback"),
                json!({"revision_id": rid, "confirm": true}),
            )
            .send()
            .await;
        assert_eq!(r.status, 422);
        assert_eq!(r.code(), code);
        assert!(!r.text().contains("abcdefghijklmnop"));
    }
    insert("aa000003", b"\xff\xfe");
    let r = env
        .post(
            &format!("{URL}/rollback"),
            json!({"revision_id": "aa000003", "confirm": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 422);
    assert_eq!(std::fs::read_to_string(&env.cfg_file).unwrap(), "a = 1\n");
}

// F6 ------------------------------------------------------------------------------------------------
#[tokio::test]
async fn test_settings_state_dir_hidden_from_non_admins() {
    // Python: Path.home() is the fixture's temp HOME; here the app's environment pins HOME
    let env = Env::new(Some("http://127.0.0.1:9"));
    let home = env.tmp().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env = env.with_env(&[("HOME", home.to_str().unwrap())]);
    let state_dir = home.join(".local/share/nc-test");
    let cfg = settings_for_test(json!({
        "state_dir": state_dir.to_string_lossy(), "frontend_dir": env.tmp().join("f").to_string_lossy(),
        "repo_root": env.tmp().join("repo").to_string_lossy(), "background_tasks": false,
    }));
    let st = Env::state_for(&cfg, &env.adapters, &env.runner, &env.http, &env.environ);
    let (_, vk) = st.auth.mint(Role::Viewer, "").unwrap();
    let (_, ak) = st.auth.mint(Role::Admin, "").unwrap();
    let v = TestReq::new(Arc::clone(&st), "GET", "/api/v1/settings")
        .client("10.0.0.9")
        .token(&vk)
        .send()
        .await;
    let v = v.json();
    assert_eq!(v["state_dir"], "~/.local/share/nc-test");
    assert!(!v.to_string().contains(home.to_str().unwrap()));
    let a = TestReq::new(st, "GET", "/api/v1/settings")
        .client("10.0.0.9")
        .token(&ak)
        .send()
        .await
        .json();
    assert_eq!(a["state_dir"], state_dir.to_string_lossy().as_ref());
    // state dir outside $HOME
    let (_, vk2) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let r = env
        .get("/api/v1/settings")
        .client("10.0.0.9")
        .token(&vk2)
        .send()
        .await
        .json();
    assert_eq!(r["state_dir"], "<state dir>");
}

#[tokio::test]
async fn test_nul_in_static_path_is_404_not_500() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    std::fs::create_dir_all(env.settings.frontend_dir.join("js")).unwrap();
    std::fs::write(env.settings.frontend_dir.join("index.html"), "x").unwrap();
    for p in ["/js/a%00.js", "/%00", "/js/%00/x", "/index.html%00"] {
        let r = env.get(p).client("10.0.0.9").send().await;
        assert_eq!(r.status, 404, "{p}");
        assert!(r.headers.contains_key("content-security-policy"));
    }
}

#[tokio::test]
async fn test_playground_requests_ask_for_identity_encoding() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/chat/completions"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"ok": 1})))
        .mount(&server)
        .await;
    let env = Env::new(Some("http://127.0.0.1:9"))
        .with_env(&[("LITELLM_API_KEY", "sk-FAKEFAKEFAKE1234567890abcdef")]);
    env.a(SystemId::Switchyard).knobs().playground_url =
        Some(format!("{}/v1/chat/completions", server.uri()));
    env.post(
        "/api/v1/systems/switchyard/playground/chat",
        json!({"model": "m1", "messages": [{"role": "user", "content": "x"}], "stream": false}),
    )
    .send()
    .await;
    let calls = server.received_requests().await.unwrap();
    assert_eq!(
        calls[0]
            .headers
            .get("accept-encoding")
            .unwrap()
            .to_str()
            .unwrap(),
        "identity"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_validations_are_bounded() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (r2, p2) = (Arc::clone(&running), Arc::clone(&peak));
    env.a(SystemId::Switchyard).knobs().validate_hook =
        Some(Arc::new(move |_f: &str, _c: &str| {
            let (running, peak) = (Arc::clone(&r2), Arc::clone(&p2));
            async move {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                ValidationResult {
                    ok: true,
                    validator: "v".to_owned(),
                    authoritative: true,
                    diagnostics: vec![],
                    note: None,
                }
            }
            .boxed()
        }));
    let reqs = (0..8).map(|_| {
        env.post(&format!("{URL}/validate"), json!({"content": "a = 1"}))
            .send()
    });
    let rs = futures::future::join_all(reqs.map(tokio::spawn)).await;
    assert!(rs.iter().all(|r| r.as_ref().unwrap().status == 200));
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "peak {}",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sse_slots_do_not_leak_on_aborted_streams() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().follow_block = true;
    let st = Arc::clone(&env.state);
    let aborted = |delay: u64| {
        let st = Arc::clone(&st);
        async move {
            let req = Request::builder()
                .method("GET")
                .uri("/api/v1/systems/switchyard/events/stream")
                .header("host", common::BASE_HOST)
                .body(Body::empty())
                .unwrap();
            let fut = async move {
                let resp = nemo_console::api::handle(st, Some("127.0.0.1".to_owned()), req).await;
                let mut body = resp.into_body().into_data_stream();
                if delay > 0 {
                    let _ = tokio::time::timeout(Duration::from_millis(delay), body.next()).await;
                }
                // the client disconnects: the body is dropped here
            };
            let _ = tokio::time::timeout(Duration::from_secs(3), fut).await;
        }
    };
    futures::future::join_all((0..20).map(|i| aborted(if i % 2 == 1 { 0 } else { 20 }))).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(env.state.sse.active(), 0);
}

#[tokio::test]
async fn test_generator_that_never_starts_holds_no_slot() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let a: Arc<dyn SystemAdapter> =
        Arc::clone(env.a(SystemId::Switchyard)) as Arc<dyn SystemAdapter>;
    let lim = Arc::new(SseLimiter::new(1));
    let stream = event_stream(
        Arc::clone(&a),
        Arc::clone(&lim),
        1.0,
        1.0,
        Shutdown::default(),
    );
    drop(stream);
    assert_eq!(lim.active(), 0);
    let full = Arc::new(SseLimiter::new(1));
    assert!(full.acquire());
    let t0 = Instant::now();
    let chunks: Vec<bytes::Bytes> =
        event_stream(a, Arc::clone(&full), 1.0, 1.0, Shutdown::default())
            .collect()
            .await;
    // over the cap: error event, no release
    assert!(String::from_utf8_lossy(&chunks[0]).contains("too many"));
    assert_eq!(full.active(), 1);
    assert!(t0.elapsed() < Duration::from_secs(2));
}
