//! The series route `GET /systems/{id}/metrics/series` (addendum 9.13, Rust-only, PORTING.md D-36) and the role
//! change of the Data Designer run records (admin since addendum 9.13, PORTING.md D-37). Hermetic: points are fed to
//! the poller with `record()` or written to the `rollup` table directly; no adapter is polled.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use common::Env;
use nemo_console::domain::models::{MetricPoint, Role, SystemId, now_ts};
use nemo_console::services::metrics::series_labels;
use serde_json::{Value, json};

const URL: &str = "/api/v1/systems/switchyard/metrics/series";

fn point(key: &str, value: f64, labels: &[(&str, &str)]) -> MetricPoint {
    let mut p = MetricPoint::new(key, value);
    for (k, v) in labels {
        p.labels.insert((*k).to_owned(), (*v).to_owned());
    }
    p
}

/// Write one 1-minute rollup row the way the poller's flush does.
fn rollup(env: &Env, system: &str, series: &str, bucket: i64, avg: f64, max: f64, n: i64) {
    env.state
        .store
        .execute(
            "INSERT INTO rollup(system, series, bucket, avg, max, n) VALUES (?,?,?,?,?,?)",
            rusqlite::params![system, series, bucket, avg, max, n],
        )
        .unwrap();
}

async fn series(env: &Env, query: &str) -> Value {
    let r = env.get(&format!("{URL}?{query}")).send().await;
    assert_eq!(r.status, 200, "{query}: {}", r.text());
    r.json()
}

fn points(s: &Value) -> Vec<[f64; 3]> {
    s["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            [
                p[0].as_f64().unwrap(),
                p[1].as_f64().unwrap(),
                p[2].as_f64().unwrap(),
            ]
        })
        .collect()
}

/// The start of the minute `back` minutes before now (epoch minutes).
fn minute(back: i64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let m = (now_ts() / 60.0).floor() as i64;
    m - back
}

#[test]
fn test_series_labels_parse_the_stored_names() {
    assert_eq!(Value::Object(series_labels("sy.tokens")), json!({}));
    assert_eq!(
        Value::Object(series_labels("sy.tokens{kind=prompt,model=x}")),
        json!({"kind": "prompt", "model": "x"})
    );
    // the contract's quoted form, a value with a comma (series_name does not escape) and one with `=`
    assert_eq!(
        Value::Object(series_labels(r#"sy.model.calls{model="x",q="p50"}"#)),
        json!({"model": "x", "q": "p50"})
    );
    assert_eq!(
        Value::Object(series_labels("k{a=1,2,b=c=d}")),
        json!({"a": "1,2", "b": "c=d"})
    );
    // keys come back in the stored (sorted) order
    let keys: Vec<String> = series_labels("k{a=1,b=2,c=3}").keys().cloned().collect();
    assert_eq!(keys, ["a", "b", "c"]);
}

#[tokio::test]
async fn test_15m_reads_the_ring_and_split_0_sums_label_variants() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let p = &env.state.metrics;
    let base = now_ts() - 100.0;
    for i in 0..3u32 {
        p.record(
            SystemId::Switchyard,
            base + f64::from(i) * 2.0,
            &[
                point("sy.tokens", 1.0, &[("kind", "prompt")]),
                point("sy.tokens", 2.0 + f64::from(i), &[("kind", "completion")]),
                point("sy.requests_total", f64::from(i), &[]),
            ],
        );
    }
    // an old point outside the 15-minute window and another system's series are not read
    p.record(
        SystemId::Switchyard,
        now_ts() - 2000.0,
        &[point("sy.requests_total", 99.0, &[])],
    );
    p.record(
        SystemId::Relay,
        base,
        &[point("sy.requests_total", 50.0, &[])],
    );
    let r = series(&env, "window=15m&keys=sy.tokens,sy.requests_total").await;
    assert_eq!(r["window"], "15m");
    assert!((r["step_s"].as_f64().unwrap() - p.interval()).abs() < 1e-9);
    assert_eq!(r["truncated"], false);
    let (from, to) = (r["from"].as_f64().unwrap(), r["to"].as_f64().unwrap());
    assert!((to - from - 900.0).abs() < 1e-6 && (to - now_ts()).abs() < 5.0);
    let s = r["series"].as_array().unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0]["key"], "sy.tokens");
    assert_eq!(s[0]["label"], Value::Null);
    let t = |i: u32| base + f64::from(i) * 2.0;
    // raw points: max = the value itself; variants summed per timestamp
    assert_eq!(
        points(&s[0]),
        [[t(0), 3.0, 3.0], [t(1), 4.0, 4.0], [t(2), 5.0, 5.0]]
    );
    assert_eq!(s[1]["key"], "sy.requests_total");
    assert_eq!(
        points(&s[1]),
        [[t(0), 0.0, 0.0], [t(1), 1.0, 1.0], [t(2), 2.0, 2.0]]
    );
}

#[tokio::test]
async fn test_split_1_gives_one_series_per_label_variant() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let p = &env.state.metrics;
    let ts = now_ts() - 30.0;
    p.record(
        SystemId::Switchyard,
        ts,
        &[
            point("sy.tokens", 1.0, &[("kind", "prompt")]),
            point("sy.tokens", 2.0, &[("kind", "completion")]),
            point("sy.tokens", 7.0, &[]),
            point("sy.tokensx", 5.0, &[]), // another key with the same prefix is not a variant
        ],
    );
    let r = series(&env, "window=15m&keys=sy.tokens&split=1").await;
    let s = r["series"].as_array().unwrap();
    assert_eq!(s.len(), 3);
    // ordered by key then label (the stored name): the bare series first, then the variants
    assert_eq!(s[0]["label"], json!({}));
    assert_eq!(points(&s[0]), [[ts, 7.0, 7.0]]);
    assert_eq!(s[1]["label"], json!({"kind": "completion"}));
    assert_eq!(points(&s[1]), [[ts, 2.0, 2.0]]);
    assert_eq!(s[2]["label"], json!({"kind": "prompt"}));
    assert!(s.iter().all(|x| x["key"] == "sy.tokens"));
    assert_eq!(r["truncated"], false);
    // a labelled key matches only itself
    let one = series(&env, "window=15m&keys=sy.tokens{kind=prompt}&split=1").await;
    let s = one["series"].as_array().unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0]["key"], "sy.tokens{kind=prompt}");
    assert_eq!(s[0]["label"], json!({"kind": "prompt"}));
    assert_eq!(points(&s[0]), [[ts, 1.0, 1.0]]);
    // split=0 over the same data: everything summed, label null
    let sum = series(&env, "window=15m&keys=sy.tokens").await;
    assert_eq!(points(&sum["series"][0]), [[ts, 10.0, 10.0]]);
}

#[tokio::test]
async fn test_split_1_caps_32_series_per_key_and_says_truncated() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let ts = now_ts() - 10.0;
    let ids: Vec<String> = (0..40).map(|i| format!("{i:02}")).collect();
    let pts: Vec<MetricPoint> = ids
        .iter()
        .map(|id| point("dcgm.gpu.util", 1.0, &[("gpu", id.as_str())]))
        .collect();
    env.state.metrics.record(SystemId::Dcgm, ts, &pts);
    env.state
        .metrics
        .record(SystemId::Dcgm, ts, &[point("dcgm.up", 1.0, &[])]);
    let r = env
        .get("/api/v1/systems/dcgm/metrics/series?window=15m&keys=dcgm.gpu.util,dcgm.up&split=1")
        .send()
        .await
        .json();
    assert_eq!(r["truncated"], true);
    let s = r["series"].as_array().unwrap();
    assert_eq!(s.len(), 33); // 32 variants of the first key + the second key
    let gpus: Vec<&str> = s[..32]
        .iter()
        .map(|x| x["label"]["gpu"].as_str().unwrap())
        .collect();
    assert_eq!(
        gpus,
        ids[..32].iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(s[32]["key"], "dcgm.up");
    // without split nothing is dropped: the 40 variants are summed
    let r = env
        .get("/api/v1/systems/dcgm/metrics/series?window=15m&keys=dcgm.gpu.util")
        .send()
        .await
        .json();
    assert_eq!(r["truncated"], false);
    assert_eq!(points(&r["series"][0]), [[ts, 40.0, 40.0]]);
}

#[tokio::test]
async fn test_1h_and_24h_read_the_minute_rollups() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (b1, b2, b3) = (minute(10), minute(180), minute(2000));
    rollup(&env, "switchyard", "sy.lat", b1, 1.5, 3.0, 4);
    rollup(&env, "switchyard", "sy.lat{q=p50}", b1, 0.5, 1.0, 4);
    rollup(&env, "switchyard", "sy.lat", b2, 2.0, 2.5, 1);
    rollup(&env, "switchyard", "sy.lat", b3, 9.0, 9.0, 1); // older than 24 h
    rollup(&env, "relay", "sy.lat", b1, 100.0, 100.0, 1); // another system
    #[allow(clippy::cast_precision_loss)]
    let ts = |b: i64| (b * 60) as f64;
    let h = series(&env, "keys=sy.lat").await; // the default window is 1h
    assert_eq!(h["window"], "1h");
    assert_eq!(h["step_s"], json!(60.0));
    assert_eq!(h["from"].as_f64().unwrap() % 60.0, 0.0);
    assert_eq!(points(&h["series"][0]), [[ts(b1), 2.0, 4.0]]); // variants summed
    let d = series(&env, "window=24h&keys=sy.lat&split=1").await;
    let s = d["series"].as_array().unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0]["label"], json!({}));
    assert_eq!(
        points(&s[0]),
        [[ts(b2), 2.0, 2.5], [ts(b1), 1.5, 3.0]] // time order
    );
    assert_eq!(s[1]["label"], json!({"q": "p50"}));
    assert_eq!(points(&s[1]), [[ts(b1), 0.5, 1.0]]);
}

#[tokio::test]
async fn test_1h_reads_points_still_pending_through_the_flush() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let p = &env.state.metrics;
    // inside ONE past minute bucket: the rollup is avg 2 and max 4 of 0..4
    let base = (now_ts() / 60.0).floor() * 60.0 - 120.0;
    for i in 0..5u32 {
        p.record(
            SystemId::Switchyard,
            base + f64::from(i) * 2.0,
            &[point("sy.requests_total", f64::from(i), &[])],
        );
    }
    let r = series(&env, "window=1h&keys=sy.requests_total").await;
    assert_eq!(points(&r["series"][0]), [[base, 2.0, 4.0]]);
}

#[tokio::test]
async fn test_7d_regroups_into_10_minute_buckets_weighted_by_n() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    #[allow(clippy::cast_possible_truncation)]
    let g = (now_ts() / 600.0).floor() as i64 - 5; // a 10-minute group about an hour ago
    let m = g * 10;
    // group g: avg (1*1 + 4*3) / 4 = 3.25, max 7
    rollup(&env, "switchyard", "sy.w", m + 1, 1.0, 5.0, 1);
    rollup(&env, "switchyard", "sy.w", m + 2, 4.0, 7.0, 3);
    // group g+1: avg (10*2 + 4*1) / 3 = 8, max 10 (minutes 0 and 9 of the group)
    rollup(&env, "switchyard", "sy.w", m + 10, 10.0, 10.0, 2);
    rollup(&env, "switchyard", "sy.w", m + 19, 4.0, 6.0, 1);
    // a label variant in group g
    rollup(&env, "switchyard", "sy.w{gpu=0}", m + 3, 2.0, 2.0, 2);
    // older than 7 days: left out
    rollup(&env, "switchyard", "sy.w", minute(8 * 1440), 50.0, 50.0, 1);
    #[allow(clippy::cast_precision_loss)]
    let ts = |grp: i64| (grp * 600) as f64;
    let r = series(&env, "window=7d&keys=sy.w&split=1").await;
    assert_eq!(r["window"], "7d");
    assert_eq!(r["step_s"], json!(600.0));
    assert_eq!(r["from"].as_f64().unwrap() % 600.0, 0.0);
    let s = r["series"].as_array().unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(points(&s[0]), [[ts(g), 3.25, 7.0], [ts(g + 1), 8.0, 10.0]]);
    assert_eq!(s[1]["label"], json!({"gpu": "0"}));
    assert_eq!(points(&s[1]), [[ts(g), 2.0, 2.0]]);
    // split=0: per 10-minute bucket, the variants' averages and maxes summed
    let r = series(&env, "window=7d&keys=sy.w").await;
    assert_eq!(
        points(&r["series"][0]),
        [[ts(g), 5.25, 9.0], [ts(g + 1), 8.0, 10.0]]
    );
}

#[tokio::test]
async fn test_a_key_without_data_is_an_empty_series() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    for w in ["15m", "1h", "24h", "7d"] {
        for split in ["0", "1"] {
            let r = series(
                &env,
                &format!("window={w}&keys=sy.none,sy.nada{{a=b}}&split={split}"),
            )
            .await;
            assert_eq!(
                r["series"],
                json!([
                    {"key": "sy.none", "label": null, "points": []},
                    {"key": "sy.nada{a=b}", "label": null, "points": []},
                ]),
                "{w} split={split}"
            );
            assert_eq!(r["truncated"], false);
        }
    }
}

#[tokio::test]
async fn test_every_bad_query_is_422_invalid_request() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let thirteen: Vec<String> = (0..13).map(|i| format!("sy.k{i}")).collect();
    let thirteen = format!("keys={}", thirteen.join(","));
    let long = format!("keys={}", "a".repeat(65));
    let bad = [
        "window=2h&keys=sy.x",
        "window=15M&keys=sy.x",
        "window=&keys=sy.x",
        "window=30d&keys=sy.x",
        "",
        "keys=",
        "keys=sy.a,,sy.b",
        "keys=sy.a,",
        ",keys=sy.a",
        "keys=Bad Key",
        "keys=sy.a{b",
        "keys=sy.a{}",
        long.as_str(),
        thirteen.as_str(),
        "keys=sy.x&split=2",
        "keys=sy.x&split=true",
        "keys=sy.x&split=",
        "keys=sy.x&split=01",
    ];
    for q in bad {
        let r = env.get(&format!("{URL}?{q}")).send().await;
        assert_eq!(r.status, 422, "{q}: {}", r.text());
        assert_eq!(r.code(), "invalid_request", "{q}");
    }
    // 12 keys is the limit, and a labelled key keeps its commas
    let twelve: Vec<String> = (0..12).map(|i| format!("sy.k{i}")).collect();
    let r = series(&env, &format!("keys={}", twelve.join(","))).await;
    assert_eq!(r["series"].as_array().unwrap().len(), 12);
    let r = series(&env, "keys=sy.t{a=1,b=2},sy.u").await;
    assert_eq!(r["series"][0]["key"], "sy.t{a=1,b=2}");
    assert_eq!(r["series"][1]["key"], "sy.u");
    // the window list is named in the message
    let r = env.get(&format!("{URL}?window=2h&keys=sy.x")).send().await;
    assert!(
        r.text().contains("['15m', '1h', '24h', '7d']"),
        "{}",
        r.text()
    );
    // the history route is unchanged: still no 7d
    assert_eq!(
        env.get("/api/v1/systems/switchyard/metrics/history?window=7d&key=sy.x")
            .send()
            .await
            .status,
        422
    );
    // unknown system: 404 before anything else
    assert_eq!(
        env.get("/api/v1/systems/bogus/metrics/series?keys=sy.x")
            .send()
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn test_a_viewer_reads_the_series_route_on_every_system() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, vk) = env.state.auth.mint(Role::Viewer, "").unwrap();
    for sid in SystemId::ALL {
        let r = env
            .get(&format!(
                "/api/v1/systems/{}/metrics/series?keys=x.up",
                sid.as_str()
            ))
            .client("10.0.0.9")
            .token(&vk)
            .send()
            .await;
        assert_eq!(r.status, 200, "{}", sid.as_str());
    }
    let r = env
        .get(&format!("{URL}?keys=x.up"))
        .client("10.0.0.9")
        .send()
        .await;
    assert_eq!(r.status, 401);
}

#[tokio::test]
async fn test_run_records_are_admin_only() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Datadesigner).knobs().records =
        Some(json!({"rows": [{"review": "model-written"}], "total": 1}));
    let (_, vk) = env.state.auth.mint(Role::Viewer, "").unwrap();
    let (_, ak) = env.state.auth.mint(Role::Admin, "").unwrap();
    let path = "/api/v1/systems/datadesigner/runs/20260101T000000-000-x/records";
    let viewer = env.get(path).client("10.0.0.9").token(&vk).send().await;
    assert_eq!(viewer.status, 403);
    assert_eq!(viewer.code(), "forbidden");
    assert!(!viewer.text().contains("model-written"));
    let admin = env.get(path).client("10.0.0.9").token(&ak).send().await;
    assert_eq!(admin.status, 200);
    assert_eq!(admin.json()["rows"][0]["review"], "model-written");
    assert_eq!(env.get(path).send().await.status, 200); // loopback trust = admin
    assert_eq!(env.get(path).client("10.0.0.9").send().await.status, 401);
    // viewers keep the artifact list (paths and sizes)
    let files = env
        .get("/api/v1/systems/datadesigner/runs/20260101T000000-000-x/files")
        .client("10.0.0.9")
        .token(&vk)
        .send()
        .await;
    assert_ne!(files.status, 403);
}
