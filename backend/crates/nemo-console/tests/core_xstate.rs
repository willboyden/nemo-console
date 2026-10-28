//! Cross-implementation persisted state (PORTING.md section 5): the Rust services read a `console.db` the PYTHON
//! services wrote (fixture from parity/gen/gen_xstate_fixture.py), and write one the Python services read back
//! (`node parity/xstate.mjs` runs the Python check on the directory this test leaves in `NC_XSTATE_OUT`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use common::Env;
use indexmap::IndexMap;
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::container::AppState;
use nemo_console::domain::models::{MetricPoint, Role, SystemId};
use nemo_console::services::common::sha256_hex;
use serde_json::{Map, Value, json};

const NOW: f64 = 1_793_000_000.0;

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xstate")
}

/// Every read the console makes of persisted state (the same reads as gen_xstate_fixture.py `reads`).
fn reads(st: &Arc<AppState>, keys: &[Value]) -> Value {
    let mut key_rows = Vec::new();
    for k in keys {
        let p = st.auth.authenticate(k["key"].as_str().unwrap());
        key_rows.push(
            json!({"label": k["label"], "principal": p.map(|p| json!([p.name, p.role.as_str()]))}),
        );
    }
    key_rows.push(
        json!({"label": "wrong", "principal": st.auth.authenticate("nck_wrong").map(|_| "?")}),
    );
    let sy: Arc<dyn SystemAdapter> = Arc::clone(st.adapter(SystemId::Switchyard).unwrap());
    let history = st.config_store.history(&sy, "routes").unwrap();
    let revisions: Vec<(String, Vec<u8>)> = st
        .store
        .with(|db| {
            let mut s = db.prepare("SELECT id, content FROM revisions ORDER BY rowid")?;
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })
        .unwrap();
    let mut rev = Map::new();
    for (id, content) in revisions {
        rev.insert(
            id,
            Value::String(base64::engine::general_purpose::STANDARD.encode(content)),
        );
    }
    let mut metrics = Map::new();
    for w in ["1h", "24h"] {
        for k in ["sy.requests_total", "sy.tokens", "sy.tokens{kind=prompt}"] {
            let pts = st.metrics.history(SystemId::Switchyard, w, k, Some(NOW));
            metrics.insert(format!("{w}:{k}"), json!(pts));
        }
    }
    let saved: Map<String, Value> = ["dynamo", "relay"]
        .iter()
        .map(|s| ((*s).to_owned(), Value::Object(st.settings.saved_params(s))))
        .collect();
    let schema: Vec<i64> = st
        .store
        .with(|db| {
            let mut s = db.prepare("SELECT version FROM schema_version ORDER BY version")?;
            s.query_map([], |r| r.get(0))?.collect()
        })
        .unwrap();
    json!({
        "keys": key_rows,
        "audit_all": st.audit.list(500, None).unwrap(),
        "audit_dynamo": st.audit.list(500, Some("dynamo")).unwrap(),
        "history": history,
        "revisions": rev,
        "metrics": metrics,
        "overrides": st.settings.overrides(),
        "effective_urls": st.settings.effective_urls(),
        "effective_key_env": st.settings.effective_key_env(),
        "effective_poll_interval": st.settings.effective_poll_interval(),
        "saved_params": saved,
        "schema_version": schema,
    })
}

/// Strict JSON comparison (int never equals float; list order matters; object key order does not).
fn diff(a: &Value, b: &Value, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}/{k}");
                match (x.get(k), y.get(k)) {
                    (Some(u), Some(v)) => diff(u, v, &p, out),
                    (u, v) => out.push(format!("{p}: {u:?} != {v:?}")),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                diff(u, v, &format!("{path}/{i}"), out);
            }
        }
        (Value::Number(x), Value::Number(y)) if x.is_f64() != y.is_f64() => {
            out.push(format!("{path}: {x} != {y} (int vs float)"));
        }
        _ if a == b => {}
        _ => out.push(format!("{path}: {a} != {b}")),
    }
}

/// An Env whose state dir is a fresh dir holding `state_db` (if any); the TempDir must outlive the Env.
fn env_on_state(state_db: Option<&Path>) -> (tempfile::TempDir, Env) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    if let Some(db) = state_db {
        std::fs::copy(db, state.join("console.db")).unwrap();
    }
    let env = Env::with_settings(None, json!({"state_dir": state.to_string_lossy()}));
    (tmp, env)
}

#[tokio::test]
async fn test_rust_reads_a_python_written_state_dir() {
    let dir = fixture_dir();
    let keys: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("keys.json")).unwrap()).unwrap();
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    let (_tmp, env) = env_on_state(Some(&dir.join("console.db")));
    let got = reads(&env.state, &keys);
    let mut problems = Vec::new();
    diff(&expected, &got, "", &mut problems);
    assert!(
        problems.is_empty(),
        "rust read of the python-written state differs:\n{}",
        problems.join("\n")
    );
}

#[tokio::test]
async fn test_rust_writes_a_state_dir_python_reads() {
    let (_tmp, env) = env_on_state(None);
    let st = Arc::clone(&env.state);
    let mut keys = Vec::new();
    for (role, label) in [(Role::Admin, "admin"), (Role::Viewer, "viewer")] {
        let (kid, key) = st.auth.mint(role, &format!("xstate {label}")).unwrap();
        keys.push(json!({"label": label, "key": key, "kid": kid}));
    }
    let big: Map<String, Value> = (0..100)
        .map(|i| (format!("k{i}"), Value::String("v".repeat(100))))
        .collect();
    let a = &st.audit;
    a.record(
        "key:abc",
        "admin",
        "PUT",
        "/api/v1/settings",
        200,
        &json!({"poll_interval_s": 7.5}),
    )
    .unwrap();
    a.record(
        "loopback",
        "admin",
        "POST",
        "/api/v1/systems/dynamo/actions/up",
        200,
        &json!({"confirm": true, "params": {"note": "héllo ✓", "api_key": "x"}}),
    )
    .unwrap();
    a.record(
        "key:abc",
        "viewer",
        "POST",
        "/api/v1/systems/dynamo/actions/x",
        403,
        &json!("[12 bytes, non-json]"),
    )
    .unwrap();
    a.record(
        "key:abc",
        "admin",
        "POST",
        "/api/v1/systems/relay/actions/x",
        422,
        &Value::Object(big),
    )
    .unwrap();
    a.record(
        "key:abc",
        "admin",
        "DELETE",
        "/api/v1/systems",
        405,
        &Value::Null,
    )
    .unwrap();
    st.store
        .execute(
            "INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
            rusqlite::params![1.5, "a", "admin", "POST", "/api/v1/x", Option::<String>::None, 200, "{\"token\": [redacted]"],
        )
        .unwrap();
    let urls: IndexMap<String, String> =
        [("dynamo".to_owned(), "http://localhost:8021/".to_owned())].into();
    let key_env: IndexMap<String, String> =
        [("switchyard".to_owned(), "MY_API_KEY".to_owned())].into();
    st.settings
        .update(Some(&urls), Some(7.5), Some(&key_env))
        .unwrap();
    let params = json!({"gpu_memory_utilization": 0.6, "max_num_seqs": 16, "router_mode": "kv", "enable_prefix_caching": false});
    st.settings
        .save_params("dynamo", params.as_object().unwrap())
        .unwrap();
    let sy: Arc<dyn SystemAdapter> = Arc::clone(st.adapter(SystemId::Switchyard).unwrap());
    let base_sha = sha256_hex(&std::fs::read(&env.cfg_file).unwrap());
    let r1 = st
        .config_store
        .apply(
            &sy,
            "routes",
            "a = 2\n# é\n",
            &base_sha,
            true,
            true,
            "key:abc",
        )
        .await
        .unwrap();
    st.config_store
        .rollback(
            &sy,
            "routes",
            r1.revision_id.as_deref().unwrap(),
            true,
            "loopback",
        )
        .await
        .unwrap();
    let base = (NOW / 60.0).floor() * 60.0 - 600.0;
    for i in 0..30u32 {
        let ts = base + f64::from(i) * 6.0;
        let v = f64::from(i % 7);
        let mut prompt = MetricPoint::new("sy.tokens", v + 0.5);
        prompt.labels.insert("kind".to_owned(), "prompt".to_owned());
        let mut completion = MetricPoint::new("sy.tokens", v + 0.5);
        completion
            .labels
            .insert("kind".to_owned(), "completion".to_owned());
        st.metrics.record(
            SystemId::Switchyard,
            ts,
            &[MetricPoint::new("sy.requests_total", v), prompt, completion],
        );
    }
    st.metrics.flush();
    st.store
        .with(|db| db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())))
        .unwrap();
    let expected = reads(&st, &keys);
    // the Rust reads of what Rust wrote must already have the Python shapes (spot checks; the Python check is xstate.mjs)
    assert_eq!(expected["audit_all"].as_array().unwrap().len(), 6);
    assert_eq!(
        expected["audit_all"][2]["params"]["_truncated"],
        json!(true)
    );
    assert_eq!(
        expected["audit_all"][0]["params"],
        json!({"_unparseable": true})
    );
    assert_eq!(expected["history"].as_array().unwrap().len(), 2);
    assert_eq!(expected["effective_poll_interval"], json!(7.5));
    assert_eq!(expected["keys"][1]["principal"][1], json!("viewer"));
    let out = match std::env::var("NC_XSTATE_OUT") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => env.tmp().join("xstate-out"),
    };
    std::fs::create_dir_all(&out).unwrap();
    std::fs::copy(st.store.path.clone(), out.join("console.db")).unwrap();
    std::fs::write(
        out.join("keys.json"),
        serde_json::to_string_pretty(&keys).unwrap(),
    )
    .unwrap();
    std::fs::write(
        out.join("expected.json"),
        serde_json::to_string_pretty(&expected).unwrap(),
    )
    .unwrap();
    std::fs::copy(&env.cfg_file, out.join("routes.toml")).unwrap();
}
