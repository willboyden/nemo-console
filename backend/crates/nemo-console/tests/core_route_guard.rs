//! Audit finding 1: role enforcement is STRUCTURAL. The route table's role is checked by the request pipeline for every
//! route before its handler runs (`api::route_guard`), not only by each handler's own `dep_admin` / `dep_viewer` call.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::Request;
use common::Env;
use http_body_util::BodyExt as _;
use nemo_console::api::{self, H, RoleDep, RouteDef};
use nemo_console::domain::models::Role;
use serde_json::Value;

/// A concrete path for a route template: valid ids everywhere (so only the role can refuse).
fn concrete(template: &str) -> String {
    template
        .replace("{system_id}", "switchyard")
        .replace("{file_id}", "routes")
        .replace("{action}", "restart")
        .replace("{run_id}", "r1")
}

async fn call(env: &Env, method: &str, path: &str, key: Option<&str>, body: bool) -> (u16, Value) {
    // not from loopback: the test env trusts loopback callers as admin (`trust_loopback`)
    let mut r = env.req(method, path).client("192.0.2.10");
    if let Some(k) = key {
        r = r.token(k);
    }
    if body {
        r = r.header("content-type", "application/json").raw(b"{}");
    }
    let resp = r.send().await;
    let v = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    (resp.status, v)
}

/// Every non-public route: 401 without a key; every admin route: 403 for a viewer (the error envelope of Python's
/// `viewer` / `admin` dependencies); never 422 / 404 / 200.
#[tokio::test]
async fn every_guarded_route_refuses_the_wrong_role() {
    let env = Env::new(None);
    let (_, viewer) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let mut checked = 0;
    for def in api::ROUTES {
        if matches!(def.role, RoleDep::Public | RoleDep::None) {
            continue;
        }
        let path = concrete(def.path);
        for m in def.methods {
            let (status, v) = call(&env, m, &path, None, def.body).await;
            assert_eq!(status, 401, "{m} {path} without a key: {v}");
            assert_eq!(v["error"]["code"], "unauthorized", "{m} {path}");
            if def.role == RoleDep::Admin {
                let (status, v) = call(&env, m, &path, Some(&viewer), def.body).await;
                assert_eq!(status, 403, "{m} {path} as a viewer: {v}");
                assert_eq!(v["error"]["code"], "forbidden", "{m} {path}");
                assert_eq!(
                    v["error"]["message"],
                    "this key has the viewer role (read-only)"
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 25, "only {checked} guarded routes");
}

/// The order of Python's dependencies is kept: an unknown system is a 404 before the role is looked at.
#[tokio::test]
async fn the_adapter_dependency_comes_before_the_role() {
    let env = Env::new(None);
    let (_, viewer) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (status, v) = call(
        &env,
        "POST",
        "/api/v1/systems/bogus/config/routes/apply",
        Some(&viewer),
        true,
    )
    .await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (404, Some("not_found"))
    );
    let (status, _) = call(
        &env,
        "POST",
        "/api/v1/systems/switchyard/config/BAD!/apply",
        Some(&viewer),
        true,
    )
    .await;
    assert_eq!(status, 404);
}

/// The pipeline itself enforces the role: an admin route whose handler does NO role check of its own (the public
/// health handler) still refuses a viewer. Without `route_guard` in `api::handle_in` this answers 200.
#[tokio::test]
async fn the_pipeline_enforces_the_table_role_without_the_handler() {
    let env = Env::new(None);
    let (_, viewer) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, admin) = env.state.auth.mint(Role::Admin, "").unwrap();
    let table: &'static [RouteDef] = Box::leak(Box::new([RouteDef {
        methods: &["GET"],
        path: "/api/v1/test/admin-only",
        label: Some("/test/admin-only"),
        role: RoleDep::Admin,
        body: false,
        handler: H::Health,
    }]));
    let compiled = api::compile_routes(table);
    for (key, want) in [(viewer, 403), (admin, 200)] {
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/test/admin-only")
            .header("host", common::BASE_HOST)
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap();
        let resp = api::handle_in(
            table,
            &compiled,
            env.state.clone(),
            Some("192.0.2.10".into()),
            req,
        )
        .await;
        let status = resp.status().as_u16();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status, want, "{}", String::from_utf8_lossy(&body));
    }
}
