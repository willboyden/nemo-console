//! The background machinery end to end in-process (the parity harness's `lifecycle` scenario compares it with Python
//! once the adapters are ported): the lifespan starts the metrics poller, the poller fills the ring and the minute
//! rollups, PUT /settings changes its interval live, and shutdown flushes the pending rollups to SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use std::time::Duration;

use common::Env;
use nemo_console::domain::models::SystemId;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_lifespan_poller_fills_ring_and_rollups_and_shutdown_flushes() {
    let env = Env::with_settings(
        None,
        json!({"background_tasks": true, "poll_interval_s": 0.1}),
    );
    let st = env.state.clone();
    let poller = nemo_console::server::startup(&st);
    assert!(poller.is_some(), "background_tasks=true starts the poller");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let ring = st
        .metrics
        .history(SystemId::Dynamo, "15m", "fake.requests_total", None);
    assert!(ring.len() >= 3, "the ring holds several ticks: {ring:?}");
    assert!(ring.iter().all(|p| p[1] == 5.0));
    let summed = st
        .metrics
        .history(SystemId::Dynamo, "15m", "fake.tokens", None);
    assert!(
        summed.iter().all(|p| p[1] == 5.0),
        "label variants are summed (2 + 3): {summed:?}"
    );
    // the 1h window flushes the pending minute bucket and reads the rollup table
    let hour = st
        .metrics
        .history(SystemId::Dynamo, "1h", "fake.requests_total", None);
    assert!(
        !hour.is_empty() && hour.iter().all(|p| p[1] == 5.0 && p[0] % 60.0 == 0.0),
        "{hour:?}"
    );
    // the interval is live: PUT /settings changes it for the running loop
    let r = env
        .put("/api/v1/settings", json!({"poll_interval_s": 0.5}))
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!((st.metrics.interval() - 0.5).abs() < f64::EPSILON);
    nemo_console::server::shutdown(&st, poller).await;
    let rows: i64 = st
        .store
        .with(|db| {
            db.query_row(
                "SELECT COUNT(*) FROM rollup WHERE system='dynamo'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert!(
        rows >= 3,
        "shutdown flushed the rollups of every series: {rows}"
    );
}

#[tokio::test]
async fn test_lifespan_without_background_tasks_starts_no_poller_but_writes_the_bootstrap_key() {
    let env = Env::new(None);
    let st = env.state;
    assert!(nemo_console::server::startup(&st).is_none());
    let key = st.cfg.state_dir.join("bootstrap-admin.key");
    assert!(
        key.is_file(),
        "the lifespan writes the first-start admin key"
    );
    assert!(nemo_console::server::startup(&st).is_none());
    assert!(
        key.is_file(),
        "a second start keeps it (keys exist: no new bootstrap key)"
    );
}
