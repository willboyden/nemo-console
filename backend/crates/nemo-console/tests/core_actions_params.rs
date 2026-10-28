//! Port of backend/tests/core/test_actions_params.py: danger levels, param validation against the ActionSpec,
//! roles, redaction, failures as data, one mutation per system, the params endpoints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::sync::Arc;

use common::Env;
use nemo_console::adapters::base::AdapterError;
use nemo_console::domain::models::{Role, SystemId};
use serde_json::{Map, Value, json};

const A: &str = "/api/v1/systems/dynamo/actions";

fn names(env: &Env) -> Vec<String> {
    env.a(SystemId::Dynamo)
        .actions_run
        .lock()
        .unwrap()
        .iter()
        .map(|(a, _)| a.clone())
        .collect()
}

#[tokio::test]
async fn test_confirm_enforced_for_disruptive_and_destructive() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    for action in ["restart", "reset"] {
        let r = env
            .post(&format!("{A}/{action}"), json!({"params": {}}))
            .send()
            .await;
        assert_eq!(r.status, 400);
        assert_eq!(r.code(), "confirm_required");
        let r = env
            .post(
                &format!("{A}/{action}"),
                json!({"params": {}, "confirm": false}),
            )
            .send()
            .await;
        assert_eq!(r.status, 400);
    }
    assert!(names(&env).is_empty());
    let r = env
        .post(
            &format!("{A}/restart"),
            json!({"params": {}, "confirm": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], true);
    assert_eq!(
        env.post(&format!("{A}/safe_admin"), json!({}))
            .send()
            .await
            .status,
        200
    );
    assert_eq!(names(&env), ["restart", "safe_admin"]);
}

#[tokio::test]
async fn test_param_validation_against_action_spec() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    for params in [
        json!({"timeout": 999}),
        json!({"timeout": "5"}),
        json!({"timeout": true}),
        json!({"nope": 1}),
        json!({"timeout": 0}),
    ] {
        let r = env
            .post(
                &format!("{A}/restart"),
                json!({"params": params, "confirm": true}),
            )
            .send()
            .await;
        assert_eq!(r.status, 422, "{params}");
        assert_eq!(r.code(), "invalid_param");
    }
    let ok = env
        .post(
            &format!("{A}/restart"),
            json!({"params": {"timeout": 30}, "confirm": true}),
        )
        .send()
        .await;
    assert_eq!(ok.status, 200);
    let run = env.a(SystemId::Dynamo).actions_run.lock().unwrap().clone();
    let want: Map<String, Value> = json!({"timeout": 30}).as_object().unwrap().clone();
    assert_eq!(run, vec![("restart".to_owned(), want)]);
}

#[tokio::test]
async fn test_unknown_action_and_bad_id() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    assert_eq!(
        env.post(&format!("{A}/nope"), json!({}))
            .send()
            .await
            .status,
        404
    );
    assert_eq!(
        env.post(&format!("{A}/Bad%20Id"), json!({}))
            .send()
            .await
            .status,
        404
    );
    assert_eq!(
        env.post("/api/v1/systems/zzz/actions/restart", json!({}))
            .send()
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn test_viewer_may_only_run_viewer_role_actions() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, key) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let v = |r: common::TestReq| r.client("10.0.0.2").token(&key);
    assert_eq!(
        v(env.post(&format!("{A}/status"), json!({})))
            .send()
            .await
            .status,
        200
    );
    assert_eq!(
        v(env.post(&format!("{A}/safe_admin"), json!({})))
            .send()
            .await
            .status,
        403
    );
    assert_eq!(
        v(env.post(&format!("{A}/restart"), json!({"confirm": true})))
            .send()
            .await
            .status,
        403
    );
    assert_eq!(
        v(env.get(A)).send().await.json().as_array().unwrap().len(),
        4
    );
}

#[tokio::test]
async fn test_action_output_is_redacted_and_failures_are_data() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Dynamo).knobs().action_output =
        Some("key=sk-FAKEFAKEFAKE1234567890abcdef and Bearer abcdefghijklmnop".to_owned());
    let body = env
        .post(&format!("{A}/safe_admin"), json!({}))
        .send()
        .await
        .json();
    let out = body["output"].as_str().unwrap();
    assert!(!out.contains("sk-FAKE") && !out.contains("abcdefghijklmnop"));
    env.a(SystemId::Dynamo).knobs().run_action_hook = Some(Arc::new(|_a: &str| {
        Err(AdapterError::failed("RuntimeError", "kaboom"))
    }));
    let r = env.post(&format!("{A}/safe_admin"), json!({})).send().await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], false);
    assert_eq!(r.json()["problems"][0]["code"], "console.action.failed");
}

#[tokio::test]
async fn test_second_mutation_while_busy_is_409() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let lock = env.state.lock(SystemId::Dynamo);
    let _held = lock.lock().await;
    let r = env.post(&format!("{A}/safe_admin"), json!({})).send().await;
    assert_eq!(r.status, 409);
    assert_eq!(r.code(), "busy");
}

#[tokio::test]
async fn test_params_catalog_preview_put() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let base = "/api/v1/systems/dynamo/params";
    let got = env.get(base).send().await.json();
    assert_eq!(got["catalog"].as_array().unwrap().len(), 3);
    assert_eq!(got["values"], json!({}));
    let r = env
        .post(
            &format!("{base}/preview"),
            json!({"values": {"gpu_util": 0.5}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!(r.json()["rendered"].as_str().unwrap().contains("0.5"));
    for bad in [
        json!({"gpu_util": 2.0}),
        json!({"mode": "z"}),
        json!({"n": 1.5}),
        json!({"unknown": 1}),
        json!({"n": true}),
    ] {
        let r = env
            .post(&format!("{base}/preview"), json!({"values": bad}))
            .send()
            .await;
        assert_eq!(r.status, 422, "{bad}");
    }
    assert_eq!(
        env.put(base, json!({"values": {"n": 3}, "confirm": false}))
            .send()
            .await
            .status,
        400
    );
    let r = env
        .put(base, json!({"values": {"n": 3}, "confirm": true}))
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], true);
    assert_eq!(env.get(base).send().await.json()["values"], json!({"n": 3}));
    let applied = env
        .a(SystemId::Dynamo)
        .params_applied
        .lock()
        .unwrap()
        .clone();
    assert_eq!(applied, vec![json!({"n": 3}).as_object().unwrap().clone()]);
}

#[tokio::test]
async fn test_params_unsupported_is_empty_catalog() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Relay).knobs().empty_catalog = true;
    assert_eq!(
        env.get("/api/v1/systems/relay/params").send().await.json(),
        json!({"catalog": [], "values": {}})
    );
    let r = env
        .post(
            "/api/v1/systems/relay/params/preview",
            json!({"values": {}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 404);
}
