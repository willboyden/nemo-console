//! Port of backend/tests/relay/test_relay_live_smoke.py: opt-in READ-ONLY live smoke against this workstation's real
//! gateway (NEMO_CONSOLE_LIVE=1; skipped otherwise, before anything is touched). Never restarts anything and never
//! sends a prompt.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, SystemAdapter};
use nemo_console::adapters::relay::RelayAdapter;
use nemo_console::config::{Settings, home_dir};
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::runner::SubprocessRunner;

#[tokio::test]
async fn test_live_status_metrics_events() {
    if std::env::var("NEMO_CONSOLE_LIVE").ok().as_deref() != Some("1") {
        return; // live smoke is opt-in (NEMO_CONSOLE_LIVE=1)
    }
    let home = home_dir();
    let repo = Settings::load().unwrap().repo_root;
    let paths = IndexMap::from([
        (
            "relay_routes".to_owned(),
            repo.join("clients/nemo-relay/routes.toml"),
        ),
        (
            "relay_plugins".to_owned(),
            home.join(".config/nemo-relay/plugins.toml"),
        ),
        (
            "relay_config".to_owned(),
            home.join(".config/nemo-relay/config.toml"),
        ),
        (
            "relay_atof_dir".to_owned(),
            home.join(".local/share/nemo-relay/atof"),
        ),
        (
            "relay_atif_dir".to_owned(),
            home.join(".local/share/nemo-relay/atif"),
        ),
    ]);
    let a = RelayAdapter::new(AdapterContext {
        runner: Arc::new(SubprocessRunner::with_environ(&[], Environ::process())),
        http: Http::new().unwrap(),
        urls: IndexMap::from([("relay".to_owned(), "http://127.0.0.1:4040".to_owned())]),
        paths,
        secret_env: IndexMap::new(),
        state_dir: std::env::temp_dir(),
        environ: Environ::process(),
    });
    let st = a.status().await.unwrap();
    let _m = a.metrics().await.unwrap();
    let evs = a.recent_events(20).await.unwrap();
    let cfg = a.read_config("routes").await.unwrap();
    let _v = a.validate_config("routes", &cfg.content).await.unwrap();
    assert!(matches!(st.health.as_str(), "up" | "degraded" | "down"));
    let allowed = [
        "atof_kind",
        "name",
        "size_bytes",
        "phase",
        "category",
        "uuid",
        "gateway_path",
        "status_code",
        "severity",
        "session_id",
        "turn_index",
        "model",
        "served_model",
        "target",
        "target_model",
        "algorithm",
        "outcome",
        "call_role",
        "failure_kind",
        "stop_reason",
        "event_type",
        "provider",
        "error_category",
        "error_phase",
        "latency_ms",
        "call_index",
        "upstream_status",
        "chunk_index",
        "fallback_used",
        "finish_reason",
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "reasoning_tokens",
        "cached_tokens",
    ];
    for e in evs {
        assert!(e.data.keys().all(|k| allowed.contains(&k.as_str())));
    }
}
