//! The integration row (written after the adapter merges) of backend/tests/core/test_observe.py (PORTING.md section 10): the REAL adapters built
//! with an empty `ctx.paths` (missing keys) degrade to problems and 404s, never a 500.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{Env, TestReq};
use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, AdapterRef, CommandRunner};
use nemo_console::adapters::build_adapter;
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::SystemId;

#[tokio::test]
async fn test_real_adapters_with_no_paths_give_problems_not_500() {
    let env = Env::new(None);
    let ctx = AdapterContext {
        runner: env.runner.clone(),
        http: env.http.clone(),
        urls: IndexMap::new(),
        paths: IndexMap::new(),
        secret_env: IndexMap::new(),
        state_dir: env.tmp().join("st3"),
        environ: env.environ.clone(),
    };
    let mut cfg = env.settings.clone();
    cfg.state_dir = env.tmp().join("state3");
    let sids = [
        SystemId::Switchyard,
        SystemId::Relay,
        SystemId::Dynamo,
        SystemId::Skillevaluator,
    ];
    let real: HashMap<SystemId, AdapterRef> = sids
        .iter()
        .map(|sid| (*sid, build_adapter(*sid, &ctx, &cfg).unwrap()))
        .collect();
    let ov = Overrides {
        adapters: real,
        runner: Some(env.runner.clone() as Arc<dyn CommandRunner>),
        http: Some(env.http.clone()),
        registry: None,
        environ: Some(env.environ.clone()),
    };
    let state = build_container(cfg, ov).unwrap();
    let get = |path: String| TestReq::new(Arc::clone(&state), "GET", &path).send();
    for sid in sids {
        let id = sid.as_str();
        for p in [
            "status",
            "metrics",
            "events",
            "config",
            "actions",
            "params",
            "playground",
        ] {
            let r = get(format!("/api/v1/systems/{id}/{p}")).await;
            assert!(r.status < 500, "{id} {p}: {} {}", r.status, r.text());
        }
        let st = get(format!("/api/v1/systems/{id}/status")).await.json();
        assert!(
            ["down", "unknown", "degraded", "up", "off"].contains(&st["health"].as_str().unwrap()),
            "{id}: {st}"
        );
        let files = get(format!("/api/v1/systems/{id}/config")).await.json();
        for f in files.as_array().unwrap() {
            let fid = f["id"].as_str().unwrap();
            let r = get(format!("/api/v1/systems/{id}/config/{fid}")).await;
            assert!(r.status < 500, "{id} {fid}: {}", r.text());
        }
    }
    assert_eq!(get("/api/v1/overview".to_owned()).await.status, 200);
}
