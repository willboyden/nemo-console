//! Port of backend/tests/core/test_followups.py: repo root, action timeout, param warnings, absent config file,
//! urls env merge.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
mod support_b;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{Env, FakeAdapter, apply_body, mode_of};
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::config::{DEFAULT_URLS, Settings, default_repo_root_from};
use nemo_console::domain::models::{ConfigFile, SystemId};
use nemo_console::services::common::sha256_hex;
use serde_json::{Map, Value, json};
use support_b::{HookAdapter, fakes, state_with};

fn empty_sha() -> String {
    sha256_hex(b"")
}

fn env_settings(pairs: &[(&str, &str)]) -> Settings {
    let env: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Settings::from_sources(None, &env, Map::new()).unwrap()
}

// (1) repo root ───────────────────────────────────────────────────────────────────────────────────

#[test]
fn test_repo_root_env_override() {
    let tmp = tempfile::tempdir().unwrap();
    let s = env_settings(&[("NEMO_CONSOLE_REPO_ROOT", &tmp.path().to_string_lossy())]);
    assert_eq!(s.repo_root, tmp.path());
}

#[test]
fn test_repo_root_falls_back_to_cwd_standalone() {
    let tmp = tempfile::tempdir().unwrap();
    let standalone = tmp.path().join("site-packages").join("nemo_console");
    std::fs::create_dir_all(&standalone).unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let w = work.clone();
    let got = default_repo_root_from(&standalone.join("config.py"), move || {
        std::fs::canonicalize(&w)
    });
    assert_eq!(got, std::fs::canonicalize(&work).unwrap());
}

#[test]
fn test_repo_root_walks_up_to_compose_files() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    std::fs::create_dir_all(t.join("inference").join("dynamo")).unwrap();
    std::fs::write(
        t.join("inference/dynamo/docker-compose.yml"),
        "services: {}\n",
    )
    .unwrap();
    let deep = t.join("clients").join("x").join("src").join("nemo_console");
    std::fs::create_dir_all(&deep).unwrap();
    let got = default_repo_root_from(&deep.join("config.py"), || {
        Ok(PathBuf::from("/nonexistent-cwd"))
    });
    assert_eq!(got, std::fs::canonicalize(t).unwrap());
}

#[tokio::test]
async fn test_missing_repo_root_is_a_problem_not_a_crash() {
    let env = Env::new(None);
    let mut init = Map::new();
    init.insert(
        "state_dir".to_owned(),
        json!(env.tmp().join("st9").to_string_lossy()),
    );
    init.insert(
        "repo_root".to_owned(),
        json!(env.tmp().join("nowhere").to_string_lossy()),
    );
    init.insert("background_tasks".to_owned(), json!(false));
    let cfg = Settings::from_sources(None, &[], init).unwrap();
    let state = state_with(&env, &cfg, fakes(&env), env.runner.clone(), &env.environ);
    let st = state.status.status(SystemId::Relay, true).await;
    assert_eq!(st.problems[0].code, "console.repo_root.missing");
    assert!(
        st.problems[0]
            .fix
            .as_deref()
            .unwrap_or("")
            .contains("NEMO_CONSOLE_REPO_ROOT")
    );
    assert_eq!(
        cfg.resolved_paths()["dynamo_compose"].file_name().unwrap(),
        "docker-compose.yml"
    ); // still builds paths
}

// (2) action timeout ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_action_timeout_default_and_per_action_override() {
    assert!((env_settings(&[]).action_timeout_s - 900.0).abs() < f64::EPSILON);
    let mut env = Env::with_settings(None, json!({"action_timeouts": {"dynamo.restart": 0.05}}));
    let mut slow = HookAdapter::new(Arc::clone(env.a(SystemId::Dynamo)));
    slow.run_action = Some(Box::new(|_id| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Err(AdapterError::failed("AssertionError", ""))
        })
    }));
    let mut adapters = fakes(&env);
    adapters.insert(SystemId::Dynamo, Arc::new(slow));
    let (settings, environ) = (env.settings.clone(), env.environ.clone());
    env.state = state_with(&env, &settings, adapters, env.runner.clone(), &environ);
    let r = env
        .post(
            "/api/v1/systems/dynamo/actions/restart",
            json!({"confirm": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    let body = r.json();
    assert_eq!(body["ok"], false);
    assert!(
        body["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
}

// (3) param warnings ──────────────────────────────────────────────────────────────────────────────

fn with_dynamo(env: &mut Env, a: HookAdapter) {
    let mut adapters = fakes(env);
    adapters.insert(SystemId::Dynamo, Arc::new(a));
    let (settings, environ) = (env.settings.clone(), env.environ.clone());
    env.state = state_with(env, &settings, adapters, env.runner.clone(), &environ);
}

#[tokio::test]
async fn test_preview_returns_adapter_warnings() {
    let mut env = Env::new(None);
    let mut a = HookAdapter::new(Arc::clone(env.a(SystemId::Dynamo)));
    a.warnings = Some(Box::new(|v: &Map<String, Value>| {
        let mut keys: Vec<String> = v.keys().map(|k| format!("'{k}'")).collect();
        keys.sort();
        Ok(vec![format!("careful [{}]", keys.join(", "))]) // Python: f"careful {sorted(v)}"
    }));
    with_dynamo(&mut env, a);
    let r = env
        .post(
            "/api/v1/systems/dynamo/params/preview",
            json!({"values": {"n": 2}}),
        )
        .send()
        .await;
    assert_eq!(r.json()["warnings"], json!(["careful ['n']"]));
    let r = env
        .post(
            "/api/v1/systems/switchyard/params/preview",
            json!({"values": {"n": 2}}),
        )
        .send()
        .await;
    assert_eq!(r.json()["warnings"], json!([])); // adapter without the hook
}

#[tokio::test]
async fn test_preview_warning_hook_crash_is_ignored() {
    let mut env = Env::new(None);
    let mut a = HookAdapter::new(Arc::clone(env.a(SystemId::Dynamo)));
    a.warnings = Some(Box::new(|_v: &Map<String, Value>| {
        Err(AdapterError::failed("RuntimeError", "x"))
    }));
    with_dynamo(&mut env, a);
    let r = env
        .post(
            "/api/v1/systems/dynamo/params/preview",
            json!({"values": {"n": 2}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["warnings"], json!([]));
}

#[tokio::test]
async fn test_preview_refused_by_the_adapter_is_422_not_500() {
    // Dynamo raises ParamError (a ValueError) when the repo compose is missing: a fresh clone's Params tab got a 500.
    let mut env = Env::new(None);
    let mut a = HookAdapter::new(Arc::clone(env.a(SystemId::Dynamo)));
    a.preview = Some(Box::new(|_v| {
        Box::pin(async {
            Err(AdapterError::Invalid(
                "cannot read the repo compose: FileNotFoundError".to_owned(),
            ))
        })
    }));
    with_dynamo(&mut env, a);
    let r = env
        .post(
            "/api/v1/systems/dynamo/params/preview",
            json!({"values": {"n": 2}}),
        )
        .send()
        .await;
    assert_eq!(r.status, 422);
    assert_eq!(
        r.json()["error"],
        json!({"code": "params_invalid", "message": "cannot read the repo compose: FileNotFoundError"})
    );
}

// (4) config file that does not exist yet ─────────────────────────────────────────────────────────

/// Python `PlaceholderAdapter`: a switchyard fake whose file does not exist yet and reads as empty.
fn placeholder(env: &mut Env) -> (Arc<FakeAdapter>, PathBuf) {
    let target = env
        .tmp()
        .join("newstate")
        .join("dynamo")
        .join("routes.toml");
    let inner = FakeAdapter::new(SystemId::Switchyard, Some(target.clone()));
    let mut a = HookAdapter::new(Arc::clone(&inner));
    a.read_config = Some(Box::new(|f: &FakeAdapter| {
        let p = f.cfg_file.clone().unwrap();
        let content = std::fs::read_to_string(&p).unwrap_or_default();
        Ok(ConfigFile {
            spec: f.config_files()?.remove(0),
            content,
            sha256: "x".to_owned(),
            mtime: 0.0,
        })
    }));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = fakes(env);
    adapters.insert(SystemId::Switchyard, Arc::new(a));
    let mut init = Map::new();
    init.insert(
        "state_dir".to_owned(),
        json!(env.tmp().join("st5").to_string_lossy()),
    );
    init.insert(
        "frontend_dir".to_owned(),
        json!(env.tmp().join("front").to_string_lossy()),
    );
    init.insert("background_tasks".to_owned(), json!(false));
    init.insert("trust_loopback".to_owned(), json!(true));
    let cfg = Settings::from_sources(None, &[], init).unwrap();
    let environ = env.environ.clone();
    env.state = state_with(env, &cfg, adapters, env.runner.clone(), &environ);
    (inner, target)
}

#[tokio::test]
async fn test_absent_file_reads_as_empty_and_apply_creates_it_private() {
    let mut env = Env::new(None);
    let (a, target) = placeholder(&mut env);
    let url = "/api/v1/systems/switchyard/config/routes";
    let r = env.get(url).send().await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["sha256"], empty_sha());
    assert_eq!(r.json()["content"], "");
    let stale = env
        .post(
            &format!("{url}/apply"),
            apply_body("a = 1\n", &"1".repeat(64)),
        )
        .send()
        .await;
    assert_eq!(stale.status, 409);
    assert!(!target.exists());
    let ok = env
        .post(&format!("{url}/apply"), apply_body("a = 1\n", &empty_sha()))
        .send()
        .await;
    assert_eq!(ok.status, 200, "{}", ok.text());
    assert_eq!(ok.json()["ok"], true, "{}", ok.text());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "a = 1\n");
    assert_eq!(mode_of(&target), 0o600);
    assert_eq!(mode_of(target.parent().unwrap()), 0o700);
    assert_eq!(*a.post_calls.lock().unwrap(), vec![true]);
}

#[tokio::test]
async fn test_failed_first_apply_removes_the_new_file() {
    let mut env = Env::new(None);
    let (_, target) = placeholder(&mut env);
    let r = env
        .post(
            "/api/v1/systems/switchyard/config/routes/apply",
            apply_body("a = 'BAD'\n", &empty_sha()),
        )
        .send()
        .await;
    assert_eq!(r.json()["rolled_back"], true, "{}", r.text());
    assert!(!target.exists());
}

// (5) env urls merge ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_env_url_override_merges_into_defaults() {
    let urls = env_settings(&[("NEMO_CONSOLE_URLS__DYNAMO", "http://127.0.0.1:9999")]).urls;
    assert_eq!(urls["dynamo"], "http://127.0.0.1:9999");
    let rest: Vec<(&str, &str)> = urls
        .iter()
        .filter(|(k, _)| *k != "dynamo")
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let want: Vec<(&str, &str)> = DEFAULT_URLS
        .iter()
        .filter(|(k, _)| *k != "dynamo")
        .copied()
        .collect();
    assert_eq!(rest, want);
    let s = env_settings(&[
        ("NEMO_CONSOLE_URLS__DYNAMO", "http://127.0.0.1:9999"),
        ("NEMO_CONSOLE_PLAYGROUND_KEY_ENV__DYNAMO", "DYN_KEY"),
    ]);
    let got: Vec<(&str, &str)> = s
        .playground_key_env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("switchyard", "LITELLM_API_KEY"),
            ("relay", "LITELLM_API_KEY"),
            ("dynamo", "DYN_KEY")
        ]
    );
}
