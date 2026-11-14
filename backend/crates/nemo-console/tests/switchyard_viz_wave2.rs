//! Addendum 9.14, Switchyard: the per-model latency / errors / cache / context keys, routing overhead, fallbacks,
//! stage-router decisions and client outcomes, from the `/v1/stats` and `/metrics` the adapter already reads
//! (fixtures captured from the live 0.3.0 server; the reduced documents below are SYNTHETIC cut-downs of them).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::needless_pass_by_value
)]

mod switchyard_support;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::switchyard::{SwitchyardAdapter, histogram_quantile, parse_prometheus};
use nemo_console::domain::models::MetricPoint;
use nemo_console::services::metrics::{MAX_SERIES_PER_SYSTEM, series_name};
use switchyard_support::{FakeHttp, FakeRunner, Reply, ctx, fixture, ok};

const NEW_KEYS: [&str; 8] = [
    "sy.model.latency_ms",
    "sy.model.errors_total",
    "sy.model.cache_hit_ratio",
    "sy.model.max_context_tokens",
    "sy.routing_overhead_ms",
    "sy.fallbacks_total",
    "sy.decisions_total",
    "sy.client_responses_total",
];

fn fx(name: &str) -> String {
    fixture("switchyard", name)
}

struct Fix {
    a: Arc<SwitchyardAdapter>,
    http: FakeHttp,
    _tmp: tempfile::TempDir,
}

impl Fix {
    async fn new() -> Fix {
        let runner = FakeRunner::new();
        runner.on(
            |a: &[String]| a.len() >= 2 && a[0] == "docker" && a[1] == "inspect",
            ok("running|healthy|ai-lab-switchyard:0.3.0|ai-lab-switchyard|switchyard\n"),
        );
        runner.on(
            |a: &[String]| a.len() >= 2 && a[0] == "docker" && a.iter().any(|x| x == "tail"),
            ok(&fx("routing_tail.jsonl")),
        );
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("switchyard");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("routes.toml"), fx("routes.good.toml")).unwrap();
        let http = FakeHttp::start().await;
        http.get("/v1/stats", Reply::text(200, &fx("stats.json")));
        http.get("/metrics", Reply::text(200, &fx("metrics.txt")));
        let c = ctx(
            runner,
            &[("switchyard", &http.uri())],
            &[
                ("switchyard_routes", dir.join("routes.toml")),
                ("switchyard_dir", dir.clone()),
            ],
            tmp.path(),
        );
        Fix {
            a: Arc::new(SwitchyardAdapter::new(c)),
            http,
            _tmp: tmp,
        }
    }

    async fn points(&self) -> Vec<MetricPoint> {
        let m = self.a.metrics().await.unwrap();
        assert!(m.available, "{:?}", m.error);
        m.points
    }
}

/// `key` -> {sorted labels -> value}.
fn by(points: &[MetricPoint], key: &str) -> BTreeMap<Vec<(String, String)>, f64> {
    points
        .iter()
        .filter(|p| p.key == key)
        .map(|p| {
            let mut l: Vec<(String, String)> = p
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            l.sort();
            (l, p.value)
        })
        .collect()
}

fn lab(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut l: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    l.sort();
    l
}

fn unit_of<'a>(points: &'a [MetricPoint], key: &str) -> &'a str {
    &points.iter().find(|p| p.key == key).unwrap().unit
}

const LIGHTNING: &str = "nemotron-lightning";
const QWEN27: &str = "qwen3.8-27b-1m";
const FLASH: &str = "qwen3.8-flash-next";

/// The stage-router decisions both sources report in the captured fixtures.
fn captured_decisions() -> BTreeMap<Vec<(String, String)>, f64> {
    BTreeMap::from([
        (lab(&[("source", "capable_hold"), ("target", FLASH)]), 2.0),
        (lab(&[("source", "dimensions"), ("target", FLASH)]), 1.0),
        (lab(&[("source", "fall_open"), ("target", LIGHTNING)]), 7.0),
        (
            lab(&[("source", "llm-classifier"), ("target", LIGHTNING)]),
            48.0,
        ),
        (lab(&[("source", "llm-classifier"), ("target", FLASH)]), 1.0),
    ])
}

fn captured_outcomes() -> BTreeMap<Vec<(String, String)>, f64> {
    BTreeMap::from([
        (lab(&[("outcome", "client_disconnected")]), 0.0),
        (lab(&[("outcome", "ok")]), 72.0),
        (lab(&[("outcome", "other_error")]), 1.0),
        (lab(&[("outcome", "retryable_error")]), 0.0),
    ])
}

#[tokio::test]
async fn test_new_keys_from_v1_stats_first() {
    let f = Fix::new().await;
    let pts = f.points().await;
    let lat = by(&pts, "sy.model.latency_ms");
    assert_eq!(
        lat.len(),
        6,
        "three models (not the classifier's), p50 + p99"
    );
    assert_eq!(lat[&lab(&[("model", LIGHTNING), ("q", "p50")])], 528.72);
    assert_eq!(lat[&lab(&[("model", LIGHTNING), ("q", "p99")])], 9451.1);
    assert_eq!(lat[&lab(&[("model", FLASH), ("q", "p99")])], 47093.45);
    assert_eq!(
        by(&pts, "sy.model.errors_total"),
        BTreeMap::from([
            (lab(&[("model", LIGHTNING)]), 0.0),
            (lab(&[("model", QWEN27)]), 1.0),
            (lab(&[("model", FLASH)]), 0.0),
        ])
    );
    assert_eq!(by(&pts, "sy.model.cache_hit_ratio").len(), 3);
    assert_eq!(
        by(&pts, "sy.model.max_context_tokens")[&lab(&[("model", FLASH)])],
        143_002.0
    );
    assert_eq!(
        by(&pts, "sy.routing_overhead_ms"),
        BTreeMap::from([
            (lab(&[("q", "p50")]), 26.76),
            (lab(&[("q", "p99")]), 508.79)
        ])
    );
    assert_eq!(
        by(&pts, "sy.fallbacks_total"),
        BTreeMap::from([
            (lab(&[("reason", "context_window")]), 0.0),
            (lab(&[("reason", "unavailable")]), 0.0)
        ])
    );
    assert_eq!(by(&pts, "sy.decisions_total"), captured_decisions());
    assert_eq!(by(&pts, "sy.client_responses_total"), captured_outcomes());
    for (key, unit) in [
        ("sy.model.latency_ms", "ms"),
        ("sy.model.errors_total", "1"),
        ("sy.model.cache_hit_ratio", "ratio"),
        ("sy.model.max_context_tokens", "tok"),
        ("sy.routing_overhead_ms", "ms"),
        ("sy.fallbacks_total", "1"),
        ("sy.decisions_total", "1"),
        ("sy.client_responses_total", "1"),
    ] {
        assert_eq!(unit_of(&pts, key), unit, "{key}");
    }
    // appended after every older key (the per-system series cap keeps the older ones first)
    let first_new = pts
        .iter()
        .position(|p| NEW_KEYS.contains(&p.key.as_str()))
        .unwrap();
    assert!(
        pts[first_new..]
            .iter()
            .all(|p| NEW_KEYS.contains(&p.key.as_str()))
    );
}

#[tokio::test]
async fn test_metrics_stands_in_when_the_stats_do_not_answer() {
    let f = Fix::new().await;
    f.http.get("/v1/stats", Reply::status(500));
    let pts = f.points().await;
    // per-model latency from the model-call histogram, computed here from the raw buckets
    let samples = parse_prometheus(&fx("metrics.txt"));
    let buckets = |model: &str| -> Vec<(f64, f64)> {
        samples
            .iter()
            .filter(|(n, l, _)| {
                n == "switchyard_model_call_latency_ms_bucket" && l["model"] == model
            })
            .map(|(_, l, v)| {
                let le = if l["le"] == "+Inf" {
                    f64::INFINITY
                } else {
                    l["le"].parse().unwrap()
                };
                (le, *v)
            })
            .collect()
    };
    let lat = by(&pts, "sy.model.latency_ms");
    assert_eq!(lat.len(), 6);
    for model in [LIGHTNING, QWEN27, FLASH] {
        for (q, p) in [("p50", 0.5), ("p99", 0.99)] {
            assert_eq!(
                lat[&lab(&[("model", model), ("q", q)])],
                histogram_quantile(p, &buckets(model)).unwrap(),
                "{model} {q}"
            );
        }
    }
    // only a model with errors has the counter in /metrics
    assert_eq!(
        by(&pts, "sy.model.errors_total"),
        BTreeMap::from([(lab(&[("model", QWEN27)]), 1.0)])
    );
    // routing overhead: both algorithms' buckets summed
    let mut all: Vec<(f64, f64)> = Vec::new();
    for (n, l, v) in &samples {
        if n == "switchyard_routing_overhead_ms_bucket" {
            let le = if l["le"] == "+Inf" {
                f64::INFINITY
            } else {
                l["le"].parse().unwrap()
            };
            match all.iter_mut().find(|(b, _)| *b == le) {
                Some(s) => s.1 += v,
                None => all.push((le, *v)),
            }
        }
    }
    let ro = by(&pts, "sy.routing_overhead_ms");
    assert_eq!(
        ro[&lab(&[("q", "p50")])],
        histogram_quantile(0.5, &all).unwrap()
    );
    assert_eq!(
        ro[&lab(&[("q", "p99")])],
        histogram_quantile(0.99, &all).unwrap()
    );
    assert_eq!(by(&pts, "sy.decisions_total"), captured_decisions());
    assert_eq!(by(&pts, "sy.client_responses_total"), captured_outcomes());
    // only /v1/stats reports these: absent, never 0
    for absent in [
        "sy.model.cache_hit_ratio",
        "sy.model.max_context_tokens",
        "sy.fallbacks_total",
    ] {
        assert!(!pts.iter().any(|p| p.key == absent), "{absent}");
    }
}

#[tokio::test]
async fn test_a_value_no_source_reports_is_absent() {
    let f = Fix::new().await;
    // a stats document without the blocks, a latency block that saw nothing, a /metrics without the families
    f.http.get(
        "/v1/stats",
        Reply::text(
            200,
            r#"{"total_requests": 0, "routing_overhead": {"count": 0, "p50_ms": 0.0, "p99_ms": 0.0},
                "models": {"m": {"calls": 0, "model_call_latency": {"count": 0, "p50_ms": 0.0}}}}"#,
        ),
    );
    f.http.get(
        "/metrics",
        Reply::text(
            200,
            "switchyard_total_requests{otel_scope_name=\"switchyard\"} 0\n",
        ),
    );
    let pts = f.points().await;
    for key in NEW_KEYS {
        assert!(!pts.iter().any(|p| p.key == key), "{key}");
    }
    // a model entry with an error count keeps it (0 is reported, so 0 is drawn)
    f.http.get(
        "/v1/stats",
        Reply::text(
            200,
            r#"{"models": {"m": {"errors": 0, "cache_hit_rate": true}}}"#,
        ),
    );
    let pts = f.points().await;
    assert_eq!(
        by(&pts, "sy.model.errors_total"),
        BTreeMap::from([(lab(&[("model", "m")]), 0.0)])
    );
    assert!(!pts.iter().any(|p| p.key == "sy.model.cache_hit_ratio")); // a bool is not a number
}

#[tokio::test]
async fn test_every_new_point_is_a_valid_series_and_the_system_fits_the_cap() {
    let f = Fix::new().await;
    let pts = f.points().await;
    let names: HashSet<String> = pts.iter().map(series_name).collect();
    assert_eq!(names.len(), pts.len(), "no two points share a series");
    assert!(
        names
            .iter()
            .all(|n| nemo_console::api::observe::KEY_RE.is_match(n)),
        "{names:?}"
    );
    // the captured stack (3 models, 5 decision paths): far below the cap; each extra model adds 6 series
    assert!(names.len() < MAX_SERIES_PER_SYSTEM / 4, "{}", names.len());
}
