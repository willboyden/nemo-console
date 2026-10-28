//! Audit finding 3: background work and shared state survive a panic, as the Python process survives an exception.
//! A panic in one tick is contained and logged with the message redacted; a poisoned store or metrics lock is
//! recovered, so one panic does not turn every later request into a 401 or an empty answer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use common::{Env, LogCapture};
use nemo_console::domain::models::{Role, SystemId};
use nemo_console::services::common::contained;
use nemo_console::services::store::StoreError;

#[tokio::test]
async fn a_panic_in_background_work_is_contained_and_logged_redacted() {
    let (cap, _guard) = LogCapture::start();
    let out = contained("unit", async {
        panic!("token Bearer sk-abcdefghijklmnopqrstuv in the message")
    })
    .await;
    let none: Option<()> = out;
    assert!(none.is_none());
    let text = cap.text();
    assert!(text.contains("unit failed"), "{text}");
    assert!(!text.contains("sk-abcdefghijklmnopqrstuv"), "{text}");
    assert_eq!(contained("ok", async { 7 }).await, Some(7));
}

#[tokio::test]
async fn the_metrics_poller_survives_a_panicking_adapter() {
    let env = Env::new(None);
    env.a(SystemId::Relay).knobs().metrics_panic = true;
    let (cap, _guard) = LogCapture::start();
    env.state.metrics.tick_contained().await; // must return, not unwind into the caller
    env.state.metrics.tick_contained().await; // and the next tick runs too
    assert!(cap.text().contains("metrics poll failed"), "{}", cap.text());
    assert!(!cap.text().contains("sk-panicsecret"), "{}", cap.text());
    // the systems polled before the panicking one in a tick were recorded
    assert!(env.state.metrics.series_for(SystemId::Switchyard) > 0);
}

#[tokio::test]
async fn a_poisoned_store_lock_does_not_lock_everyone_out() {
    let env = Env::new(None);
    let (_, admin) = env.state.auth.mint(Role::Admin, "").unwrap();
    let store = env.state.store.clone();
    let r = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<(), StoreError> = store.with(|_db| panic!("boom inside a store call"));
    }));
    assert!(r.is_err(), "the closure panicked");
    // the mutex is poisoned now; requests still authenticate through it
    let resp = env
        .get("/api/v1/systems")
        .client("192.0.2.10")
        .token(&admin)
        .send()
        .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(
        store
            .with(|db| db.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)))
            .is_ok()
    );
}
