//! The poller's `record` guard (audit of wave 2): a non-finite value or an over-long series name never reaches the
//! ring or the rollups, and a series idle for `SERIES_IDLE_EVICT_S` leaves the ring and frees its slot of the
//! per-system cap (its rollups stay). Hermetic: points are fed with `record()`; no adapter is polled.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use common::Env;
use nemo_console::domain::models::{MetricPoint, SystemId, now_ts};
use nemo_console::services::metrics::{
    MAX_SERIES_NAME, MAX_SERIES_PER_SYSTEM, SERIES_IDLE_EVICT_S,
};

fn point(key: &str, value: f64, labels: &[(&str, &str)]) -> MetricPoint {
    let mut p = MetricPoint::new(key, value);
    for (k, v) in labels {
        p.labels.insert((*k).to_owned(), (*v).to_owned());
    }
    p
}

fn rollup_rows(env: &Env, like: &str) -> i64 {
    env.state
        .store
        .with(|db| {
            db.query_row(
                "SELECT COUNT(*) FROM rollup WHERE series LIKE ?",
                [like],
                |r| r.get(0),
            )
        })
        .unwrap()
}

#[tokio::test]
async fn test_non_finite_values_and_long_names_are_dropped() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let m = &env.state.metrics;
    let t0 = now_ts() - 600.0;
    let long = "x".repeat(MAX_SERIES_NAME);
    let fits = "y".repeat(MAX_SERIES_NAME - "sy.k{model=}".len());
    for i in 0..3u32 {
        m.record(
            SystemId::Switchyard,
            t0 + f64::from(i) * 70.0,
            &[
                point("sy.nan", f64::NAN, &[]),
                point("sy.inf", f64::INFINITY, &[]),
                point("sy.neg_inf", f64::NEG_INFINITY, &[]),
                point("sy.k", 1.0, &[("model", &long)]),
                point("sy.k", 2.0, &[("model", &fits)]),
                point("sy.ok", 3.0, &[]),
            ],
        );
    }
    m.flush();
    let st = m.record_stats();
    assert_eq!(st.dropped_non_finite, 9);
    assert_eq!(st.dropped_long_name, 3);
    for gone in ["sy.nan", "sy.inf", "sy.neg_inf"] {
        assert!(
            m.history(SystemId::Switchyard, "15m", gone, None)
                .is_empty()
        );
        assert_eq!(rollup_rows(&env, gone), 0);
    }
    assert_eq!(rollup_rows(&env, "sy.k{model=x%"), 0);
    // the longest name that fits is kept, with the finite value, in the ring and the rollups
    let kept = m.history(SystemId::Switchyard, "15m", "sy.k", None);
    assert_eq!(kept.len(), 3);
    assert!(kept.iter().all(|p| p[1] == 2.0));
    assert_eq!(rollup_rows(&env, "sy.k{%"), 3);
    assert_eq!(rollup_rows(&env, "sy.ok"), 3);
}

#[tokio::test]
async fn test_idle_series_leave_the_ring_and_free_their_cap_slots() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let m = &env.state.metrics;
    let t0 = now_ts() - 3.0 * 3600.0;
    // a full cap of short-lived label variants (models that came and went) ...
    let churn: Vec<MetricPoint> = (0..MAX_SERIES_PER_SYSTEM)
        .map(|i| point("sy.model.calls", 1.0, &[("model", &format!("gone{i:03}"))]))
        .collect();
    m.record(SystemId::Switchyard, t0, &churn);
    // ... so a new key has no slot while they are fresh
    m.record(
        SystemId::Switchyard,
        t0 + 60.0,
        &[point("sy.new", 1.0, &[])],
    );
    m.flush();
    assert_eq!(rollup_rows(&env, "sy.new"), 0);
    // once they have been idle for SERIES_IDLE_EVICT_S, they are evicted and the new key gets its slot
    let later = t0 + SERIES_IDLE_EVICT_S + 120.0;
    m.record(SystemId::Switchyard, later, &[point("sy.new", 5.0, &[])]);
    m.flush();
    assert_eq!(m.record_stats().evicted, MAX_SERIES_PER_SYSTEM);
    assert_eq!(rollup_rows(&env, "sy.new"), 1);
    // the evicted series keep their rollups (the pending minute was written when they left)
    assert_eq!(
        rollup_rows(&env, "sy.model.calls{%"),
        i64::try_from(MAX_SERIES_PER_SYSTEM).unwrap()
    );
    // a busy series is never evicted: it keeps getting points
    let mut ts = later;
    for _ in 0..40 {
        ts += 60.0;
        m.record(SystemId::Switchyard, ts, &[point("sy.new", 6.0, &[])]);
    }
    assert_eq!(m.record_stats().evicted, MAX_SERIES_PER_SYSTEM);
    // another system's series are counted on their own
    m.record(SystemId::Relay, ts, &[point("rl.x", 1.0, &[])]);
    m.flush();
    assert_eq!(rollup_rows(&env, "rl.x"), 1);
}
