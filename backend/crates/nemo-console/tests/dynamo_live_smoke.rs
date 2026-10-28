//! Port of backend/tests/dynamo/test_live_smoke.py: READ-ONLY live smoke against the running Dynamo stack. Opt-in
//! (NEMO_LIVE=1, checked before anything is touched; Python probes :8020 at collection instead, which a hermetic run
//! must not do). Never starts, stops or recreates anything; `NEMO_CONSOLE_LIVE_DYNAMO_COMPOSE` points it at a checkout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, SystemAdapter};
use nemo_console::adapters::dynamo::DynamoAdapter;
use nemo_console::domain::models::Health;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::runner::SubprocessRunner;
use serde_json::json;

fn find_base() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NEMO_CONSOLE_LIVE_DYNAMO_COMPOSE") {
        return Some(PathBuf::from(p));
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("inference/dynamo/docker-compose.yml"))
        .find(|p| p.is_file())
}

#[tokio::test]
async fn test_live_status_metrics_preview() {
    if std::env::var("NEMO_LIVE").ok().as_deref() != Some("1") {
        return; // live smoke is opt-in (NEMO_LIVE=1)
    }
    let Some(base) = find_base().filter(|p| p.is_file()) else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let ad = DynamoAdapter::new(AdapterContext {
        runner: Arc::new(SubprocessRunner::with_environ(&[], Environ::process())),
        http: Http::new().unwrap(),
        urls: IndexMap::from([("dynamo".to_owned(), "http://127.0.0.1:8020".to_owned())]),
        paths: IndexMap::from([("dynamo_compose".to_owned(), base)]),
        secret_env: IndexMap::new(),
        state_dir: tmp.path().to_path_buf(),
        environ: Environ::process(),
    });
    let st = ad.status().await.unwrap();
    assert!(matches!(st.health, Health::Up | Health::Degraded));
    let cf = ad.compose_files(None).await.unwrap();
    assert!(cf.error.is_none() && !cf.files.is_empty());
    let snap = ad.metrics().await.unwrap();
    assert!(snap.available);
    let values = json!({"max_num_seqs": 32});
    let preview = ad
        .preview_params(values.as_object().unwrap())
        .await
        .unwrap();
    assert!(preview.contains("--max-num-seqs"));
    let v = ad.validate_config("override", &preview).await.unwrap();
    if cf.overrides().is_empty() {
        assert!(v.ok && v.authoritative);
    } else {
        assert!(!v.ok);
    }
    assert!(!ad.read_config("compose").await.unwrap().spec.editable);
}
