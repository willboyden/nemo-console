//! Port of backend/tests/switchyard/test_live_smoke.py: read-only smoke against the REAL stack. Opt-in: NEMO_LIVE=1
//! (skipped otherwise, before anything is touched). Never runs a disruptive action; validate_config sends the
//! candidate over stdin to a throwaway --network none container.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, SystemAdapter};
use nemo_console::adapters::switchyard::SwitchyardAdapter;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::runner::SubprocessRunner;

fn repo_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("inference/switchyard/routes.toml").is_file())
        .map(Path::to_path_buf)
}

#[tokio::test]
async fn test_live_read_only() {
    if std::env::var("NEMO_LIVE").ok().as_deref() != Some("1") {
        return; // live smoke is opt-in (NEMO_LIVE=1)
    }
    let Some(repo) = repo_root() else {
        return; // not inside a lab repository checkout
    };
    let d = repo.join("inference/switchyard");
    let ctx = AdapterContext {
        runner: Arc::new(SubprocessRunner::with_environ(&[], Environ::process())),
        http: Http::new().unwrap(),
        urls: IndexMap::from([("switchyard".to_owned(), "http://127.0.0.1:4010".to_owned())]),
        paths: IndexMap::from([
            ("switchyard_dir".to_owned(), d.clone()),
            ("switchyard_routes".to_owned(), d.join("routes.toml")),
        ]),
        secret_env: IndexMap::new(),
        state_dir: std::env::temp_dir(),
        environ: Environ::process(),
    };
    let a = SwitchyardAdapter::new(ctx);
    let st = a.status().await.unwrap();
    println!("STATUS {:?} {:?} {}", st.health, st.version, st.summary);
    let m = a.metrics().await.unwrap();
    let cf = a.read_config("routes").await.unwrap();
    let v = a.validate_config("routes", &cf.content).await.unwrap();
    let bad = a.validate_config("routes", "a = [").await.unwrap();
    assert!(st.version.as_deref() == Some("0.3.0") && m.available && v.ok && !bad.ok);
}
