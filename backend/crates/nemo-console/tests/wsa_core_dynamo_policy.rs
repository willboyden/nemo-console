//! workstream A's share of backend/tests/core/test_dynamo_policy.py: the Dynamo card policy comes from Settings and reaches
//! the registry-built adapter; an injected adapter keeps its own. Also the CardPolicy half of
//! `test_defaults_describe_the_lab_and_lists_replace` (PORTING.md section 9: the core test covers the Settings half).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::Env;
use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, AdapterRef};
use nemo_console::adapters::dynamo::{CardPolicy, DynamoAdapter, DynamoNames};
use nemo_console::config::{Settings, settings_for_test};
use nemo_console::container::{Overrides, build_container};
use nemo_console::domain::models::{Health, SystemId};
use nemo_console::services::environ::Environ;
use serde_json::{Map, Value, json};

fn closed_urls() -> Value {
    let mut urls = Map::new();
    for k in [
        "switchyard",
        "relay",
        "dynamo",
        "dynamo_worker_metrics",
        "litellm",
        "dcgm",
        "prometheus",
        "guardrails",
    ] {
        urls.insert(k.to_owned(), json!("http://127.0.0.1:9"));
    }
    Value::Object(urls)
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn test_card_policy_defaults_equal_the_settings_defaults() {
    let s = Settings::from_sources(None, &[], Map::new()).unwrap();
    let from_settings = CardPolicy {
        start: s.dynamo_start.clone(),
        restore: s.default_model_start.clone(),
        exclusive: s.dynamo_exclusive_engines.clone(),
        coexisting: s.dynamo_coexisting_engines.clone(),
        ignored: s.dynamo_ignored_containers.clone(),
        dark: s.dynamo_dark_while_held.clone(),
        ..CardPolicy::default()
    };
    assert_eq!(CardPolicy::default(), from_settings);
    assert_eq!(
        CardPolicy::default(),
        CardPolicy::from_settings(&s, &DynamoNames::default())
    );
}

#[tokio::test]
async fn test_settings_reach_the_registry_built_dynamo_adapter() {
    let env = Env::new(None);
    let cfg = settings_for_test(json!({
        "state_dir": env.tmp.path().join("state-p").to_string_lossy(),
        "frontend_dir": env.tmp.path().join("front").to_string_lossy(),
        "background_tasks": false, "dynamo_opt_in": false, "dynamo_start": "my/start",
        "default_model_start": "my/restore", "dynamo_exclusive_engines": ["x-1"],
        "dynamo_coexisting_engines": ["y-1"], "dynamo_ignored_containers": ["z-1"],
        "dynamo_dark_while_held": ["alias-a"], "urls": closed_urls(),
    }));
    let fakes = || -> HashMap<SystemId, AdapterRef> {
        env.adapters
            .iter()
            .filter(|(k, _)| **k != SystemId::Dynamo)
            .map(|(k, v)| (*k, Arc::clone(v) as AdapterRef))
            .collect()
    };
    let ov = |adapters| Overrides {
        adapters,
        runner: Some(env.runner.clone()),
        http: Some(env.http.clone()),
        registry: None,
        environ: Some(Environ::fixed(HashMap::new())),
    };
    let c = build_container(cfg.clone(), ov(fakes())).unwrap();
    let dynr = c.adapter(SystemId::Dynamo).unwrap();
    assert_eq!(dynr.label(), "NVIDIA Dynamo");
    // the policy the container built (behaviour: every text it drives) ...
    let acts = dynr.actions().unwrap();
    let up = acts.iter().find(|a| a.id == "up").unwrap();
    assert!(
        up.description.contains("my/start")
            && up.description.contains("(x-1)")
            && up.description.contains("alias-a")
    );
    assert!(
        acts.iter()
            .find(|a| a.id == "down")
            .unwrap()
            .description
            .contains("my/restore")
    );
    let st = dynr.status().await.unwrap(); // opt-in off: nothing runs -> down, with the configured start fix
    assert_eq!(st.health, Health::Down);
    assert!(
        st.problems
            .iter()
            .all(|p| p.fix.as_deref() == Some("my/start"))
    );
    // ... and the value itself, built the way the registry builds it
    let ctx = AdapterContext {
        runner: env.runner.clone(),
        http: env.http.clone(),
        urls: IndexMap::new(),
        paths: cfg.resolved_paths(),
        secret_env: IndexMap::new(),
        state_dir: cfg.state_dir.clone(),
        environ: Environ::fixed(HashMap::new()),
    };
    let built = DynamoAdapter::from_settings(&ctx, &cfg);
    assert_eq!(
        built.policy,
        CardPolicy {
            opt_in: false,
            start: "my/start".into(),
            restore: "my/restore".into(),
            exclusive: owned(&["x-1"]),
            coexisting: owned(&["y-1"]),
            ignored: owned(&["z-1"]),
            dark: owned(&["alias-a"]),
            ..CardPolicy::default()
        }
    );
    // an injected adapter keeps its own policy
    let mut mine = DynamoAdapter::new(ctx);
    mine.policy = CardPolicy {
        start: "injected".into(),
        ..CardPolicy::default()
    };
    let mine = Arc::new(mine);
    let mut adapters = fakes();
    adapters.insert(SystemId::Dynamo, Arc::clone(&mine) as AdapterRef);
    let c2 = build_container(cfg, ov(adapters)).unwrap();
    let got = c2.adapter(SystemId::Dynamo).unwrap();
    assert!(std::ptr::addr_eq(Arc::as_ptr(got), Arc::as_ptr(&mine)));
    assert_eq!(mine.policy.start, "injected");
}
