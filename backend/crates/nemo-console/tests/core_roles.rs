//! Port of backend/tests/core/test_roles.py. Python inspects FastAPI's dependency tree; the Rust route table
//! (`nemo_console::api::ROUTES`) declares each route's ONE role dependency directly, so the same rules are checked
//! on it, and the OpenAPI document must list exactly the routed paths.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::collections::BTreeSet;

use common::Env;
use nemo_console::api::{ROUTES, RoleDep, RouteDef};

/// The routes of the routers (the API and the root /metrics), i.e. without the OpenAPI document and the static
/// catch-all (Python `_all_routes()`).
fn all_routes() -> Vec<&'static RouteDef> {
    ROUTES
        .iter()
        .filter(|r| r.path != "/api/v1/openapi.json" && r.path != "/{path:path}")
        .collect()
}

#[test]
fn test_every_route_carries_exactly_one_role_dependency() {
    let routes = all_routes();
    assert!(routes.len() >= 25);
    for r in routes {
        // exactly one role dependency: the table has one `role` per route and it is never `None` here
        assert_ne!(
            r.role,
            RoleDep::None,
            "{}: needs exactly one role dependency",
            r.path
        );
        if r.path == "/api/v1/health" {
            assert_eq!(r.role, RoleDep::Public);
            continue;
        }
        assert_ne!(r.role, RoleDep::Public, "{} must not be public", r.path);
        let is_get = r.methods.contains(&"GET");
        if !is_get && !r.path.ends_with("/actions/{action}") {
            assert_eq!(
                r.role,
                RoleDep::Admin,
                "{}: mutating routes need admin",
                r.path
            );
        }
        if is_get && r.path != "/api/v1/audit" {
            assert_eq!(r.role, RoleDep::Viewer, "{}", r.path);
        }
    }
}

#[test]
fn test_openapi_and_app_routes_are_exactly_the_enumerated_ones() {
    let enumerated: BTreeSet<&str> = all_routes().iter().map(|r| r.path).collect();
    let doc: serde_json::Value =
        serde_json::from_str(nemo_console::api::admin::OPENAPI_JSON).unwrap();
    let documented: BTreeSet<&str> = doc["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(documented, enumerated);
    // the only routes outside the routers: openapi.json (auth by middleware) and the public static catch-all
    let extra: Vec<&RouteDef> = ROUTES
        .iter()
        .filter(|r| !enumerated.contains(r.path))
        .collect();
    let extra_paths: Vec<&str> = extra.iter().map(|r| r.path).collect();
    assert_eq!(extra_paths, ["/api/v1/openapi.json", "/{path:path}"]);
    let catch_all = extra.iter().find(|r| r.path == "/{path:path}").unwrap();
    assert_eq!(catch_all.role, RoleDep::Public);
    let health = ROUTES.iter().find(|r| r.path == "/api/v1/health").unwrap();
    assert_eq!(health.role, RoleDep::Public);
}

#[tokio::test]
async fn test_openapi_json_is_behind_auth() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    assert_eq!(
        env.get("/api/v1/openapi.json")
            .client("10.0.0.9")
            .send()
            .await
            .status,
        401
    );
    assert_eq!(env.get("/api/v1/openapi.json").send().await.status, 200);
}
