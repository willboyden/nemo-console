//! The Rust route table equals the Python app's (parity/routes.python.json, generated from the live FastAPI
//! app by parity/tools/dump_routes.py) on (method, path, auth role). Writes parity/routes.rust.json for the
//! parity harness's route-coverage check.
use serde_json::{Value, json};

#[test]
fn test_route_table_matches_python() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../parity");
    let rows = nemo_console::api::route_rows();
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
    let mut got: Vec<_> = rows.iter().map(key).collect();
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
