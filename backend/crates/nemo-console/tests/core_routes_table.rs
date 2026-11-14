//! The Rust route table equals the Python app's (parity/routes.python.json, generated from the live FastAPI
//! app by parity/tools/dump_routes.py) on (method, path, auth role), apart from the routes added after the port
//! (listed below, each with its PORTING.md deviation) and the routes whose role changed after the port (also below).
//! Writes parity/routes.rust.json for the parity harness's route-coverage check; a Rust-only row carries
//! `"rust_only": "<deviation>"` there, a row with a changed role `"auth_deviation": {"python": <role>, "deviation":
//! "<deviation>"}`.
use serde_json::{Value, json};

/// Routes that exist only in the Rust backend: (method, path, auth, PORTING.md deviation).
const RUST_ONLY: [(&str, &str, &str, &str); 12] = [
    (
        "GET",
        "/api/v1/systems/{system_id}/analytics",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/capture",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/metrics/series",
        "viewer",
        "D-36",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/sessions",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/sessions/{session_key}",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/traces",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/traces/index",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/traces/{trace_id}",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/traces/{trace_id}/spans/{span_id}/content",
        "admin",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/trajectories",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/trajectories/{trajectory_id}",
        "viewer",
        "D-34",
    ),
    (
        "GET",
        "/api/v1/systems/{system_id}/trajectories/{trajectory_id}/steps/{step_id}/content",
        "admin",
        "D-34",
    ),
];

/// Routes whose role changed after the port: (method, path, Python role, Rust role, PORTING.md deviation).
const ROLE_CHANGED: [(&str, &str, &str, &str, &str); 1] = [(
    "GET",
    "/api/v1/systems/{system_id}/runs/{run_id}/records",
    "viewer",
    "admin",
    "D-37",
)];

fn role_changed(r: &Value) -> Option<(&'static str, &'static str, &'static str)> {
    ROLE_CHANGED
        .iter()
        .find(|(m, p, _, _, _)| r["method"] == *m && r["path"] == *p)
        .map(|(_, _, py, rust, d)| (*py, *rust, *d))
}

fn rust_only(r: &Value) -> Option<&'static str> {
    RUST_ONLY
        .iter()
        .find(|(m, p, _, _)| r["method"] == *m && r["path"] == *p)
        .map(|(_, _, _, d)| *d)
}

#[test]
fn test_route_table_matches_python() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../parity");
    let mut rows = nemo_console::api::route_rows();
    for r in &mut rows {
        if let Some(d) = rust_only(r) {
            r["rust_only"] = json!(d);
        }
        if let Some((py, rust, d)) = role_changed(r) {
            assert_eq!(r["auth"], rust, "{d}: {} {}", r["method"], r["path"]);
            r["auth_deviation"] = json!({"python": py, "deviation": d});
        }
    }
    let doc = json!({"generated_by": "cargo test --test core_routes_table", "routes": rows});
    std::fs::write(
        dir.join("routes.rust.json"),
        serde_json::to_string_pretty(&doc).unwrap() + "\n",
    )
    .unwrap();
    let py: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("routes.python.json")).unwrap())
            .unwrap();
    let key = |r: &Value| {
        (
            r["method"].as_str().unwrap_or("").to_owned(),
            r["path"].as_str().unwrap_or("").to_owned(),
            r["auth"].clone(),
        )
    };
    let mut want: Vec<_> = py["routes"].as_array().unwrap().iter().map(key).collect();
    // every Rust-only route exists, with its role, and is not in the Python table
    for (m, p, auth, d) in RUST_ONLY {
        assert!(
            rows.iter()
                .any(|r| r["method"] == m && r["path"] == p && r["auth"] == auth),
            "{d}: {m} {p} ({auth}) is not in the Rust route table"
        );
        assert!(
            !py["routes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["method"] == m && r["path"] == p),
            "{m} {p} is in the Python table: not Rust-only"
        );
    }
    // a changed role is compared as the Python role it replaced, and Python must still have exactly that role
    for (m, p, py_auth, _, d) in ROLE_CHANGED {
        assert!(
            py["routes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["method"] == m && r["path"] == p && r["auth"] == py_auth),
            "{d}: {m} {p} is not {py_auth} in the Python table"
        );
    }
    let mut got: Vec<_> = rows
        .iter()
        .filter(|r| rust_only(r).is_none())
        .map(|r| match role_changed(r) {
            Some((py_auth, _, _)) => {
                let (m, p, _) = key(r);
                (m, p, json!(py_auth))
            }
            None => key(r),
        })
        .collect();
    want.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    got.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    assert_eq!(got, want);
    // body models: exactly the Python routes with a request body
    for r in py["routes"].as_array().unwrap() {
        let has_body = !r["body_model"].is_null();
        let rust = rows
            .iter()
            .find(|x| x["method"] == r["method"] && x["path"] == r["path"])
            .unwrap();
        assert_eq!(
            rust["body"].as_bool().unwrap(),
            has_body,
            "{} {}",
            r["method"],
            r["path"]
        );
    }
}
