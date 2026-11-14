//! Addendum 9.14, Dynamo: windowed TTFT / ITL / phase quantiles (the requests that finished since the previous
//! poll, from histogram bucket deltas), preemptions, capacity and spec-decode acceptance per draft position. Driven
//! by the CAPTURED frontend and worker scrapes; later polls add SYNTHETIC observations to their buckets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::needless_pass_by_value
)]

mod dynamo_support;
mod switchyard_support;

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use dynamo_support::{Fix, fx, is_docker, worker_doc};
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::dynamo::{
    DynamoAdapter, HIST_MAX_BUCKETS, HIST_MAX_SETS, HistCounts, HistWindows, SPEC_MAX_POSITIONS,
    bucket_quantile, hist_counts, hist_delta, parse_prom, plain_points_9_14,
};
use nemo_console::domain::models::MetricPoint;
use nemo_console::services::metrics::{MAX_SERIES_PER_SYSTEM, series_name};
use switchyard_support::{Reply, ok};

const TTFT: &str = "dynamo_frontend_time_to_first_token_seconds";
const ITL: &str = "dynamo_frontend_inter_token_latency_seconds";
const QUEUE: &str = "vllm:request_queue_time_seconds";

const WINDOWED_KEYS: [&str; 3] = ["dyn.ttft_window_ms", "dyn.itl_window_ms", "dyn.phase_ms"];

/// Add `n` observations of a value just above `above` to `family` in a Prometheus text: every `_bucket` line with
/// `le` > `above` (cumulative buckets, `+Inf` included) and the `_count` line grow by `n`. SYNTHETIC.
fn bump(text: &str, family: &str, above: f64, n: f64) -> String {
    let bucket = format!("{family}_bucket{{");
    let count = format!("{family}_count");
    let mut out = String::new();
    for line in text.lines() {
        let grow = if line.starts_with(&bucket) {
            let le = line
                .split("le=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            let le: f64 = if le == "+Inf" {
                f64::INFINITY
            } else {
                le.parse().unwrap()
            };
            le > above
        } else {
            line.starts_with(&count)
        };
        if grow {
            let (head, val) = line.rsplit_once(' ').unwrap();
            let v: f64 = val.parse().unwrap();
            out.push_str(&format!("{head} {}\n", v + n));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
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

fn windowed(points: &[MetricPoint]) -> Vec<&MetricPoint> {
    points
        .iter()
        .filter(|p| WINDOWED_KEYS.contains(&p.key.as_str()))
        .collect()
}

/// A live frontend and worker whose `/metrics` texts a test changes between polls.
struct Live {
    f: Fix,
    a: Arc<DynamoAdapter>,
    worker: Arc<Mutex<String>>,
}

impl Live {
    async fn new() -> Live {
        let f = Fix::new().await;
        dynamo_support::wire(&f.runner, vec![worker_doc()], "");
        let worker = Arc::new(Mutex::new(fx("worker_metrics.prom")));
        let w = Arc::clone(&worker);
        let health = fx("worker_health.json");
        f.runner.on_fn(is_docker("exec"), move |_a: &[String]| {
            ok(&format!(
                "{}\n---METRICS---\n{}",
                health.trim(),
                w.lock().unwrap()
            ))
        });
        dynamo_support::http_live(&f.mock, true);
        let a = f.make();
        Live { f, a, worker }
    }

    fn frontend(&self, text: &str) {
        self.f.mock.get("/metrics", Reply::text(200, text));
    }

    fn worker(&self, text: &str) {
        *self.worker.lock().unwrap() = text.to_owned();
    }

    /// The poller's read (moves the windows).
    async fn poll(&self) -> Vec<MetricPoint> {
        let snap = self.a.poll_metrics().await.unwrap();
        assert!(snap.available, "{:?}", snap.error);
        snap.points
    }

    /// A UI / API read (`metrics()`): compares with the previous poll, changes nothing.
    async fn peek(&self) -> Vec<MetricPoint> {
        let snap = self.a.metrics().await.unwrap();
        assert!(snap.available, "{:?}", snap.error);
        snap.points
    }
}

// ── the window arithmetic ───────────────────────────────────────────────────────────────────────────

fn counts(text: &str, family: &str) -> HistCounts {
    hist_counts(&parse_prom(text), family)
}

#[test]
fn test_hist_delta_sums_label_sets_and_sees_resets() {
    let fe = fx("frontend_metrics.prom");
    let before = counts(&fe, TTFT);
    assert_eq!(before.len(), 1, "one model label set");
    let after = counts(&bump(&fe, TTFT, 0.1, 3.0), TTFT);
    let d = hist_delta(&before, &after).unwrap();
    // three observations, all in (0.1, 0.22]
    assert_eq!(d.iter().map(|x| x.1).fold(0.0, f64::max), 3.0);
    let p50 = bucket_quantile(d, 0.5).unwrap();
    assert!((p50 - 0.16).abs() < 1e-12, "{p50}");
    // nothing new: zero deltas, no quantile
    let none = hist_delta(&after, &after).unwrap();
    assert!(none.iter().all(|x| x.1 == 0.0));
    assert_eq!(bucket_quantile(none, 0.5), None);
    // a bucket that went down is a reset
    assert_eq!(hist_delta(&after, &before), None);
    // a label set only one side has adds nothing (a worker restarted under a new id)
    let renamed = counts(&fe.replace("Qwen3.8-27B-NVFP4\",le", "Other\",le"), TTFT);
    assert!(hist_delta(&before, &renamed).unwrap().is_empty());
}

#[test]
fn test_windows_track_the_previous_poll() {
    let fe = fx("frontend_metrics.prom");
    let mut w = HistWindows::default();
    let c0 = counts(&fe, TTFT);
    let c1 = counts(&bump(&fe, TTFT, 0.1, 2.0), TTFT);
    let c2 = counts(&bump(&bump(&fe, TTFT, 0.1, 2.0), TTFT, 1.0, 4.0), TTFT);
    // a read before any poll has nothing to compare with
    assert_eq!(w.observe(TTFT, Some(c0.clone()), false), None);
    // first poll: no window yet
    assert_eq!(w.observe(TTFT, Some(c0.clone()), true), None);
    // any number of other reads compare with that poll and move nothing
    for _ in 0..40 {
        let peek = w.observe(TTFT, Some(c2.clone()), false).unwrap();
        assert_eq!(peek.iter().map(|x| x.1).fold(0.0, f64::max), 6.0);
    }
    assert_eq!(w.observe(TTFT, None, false), None); // an absent family on a read keeps the baseline
    // the next poll: still everything since the previous poll
    let d = w.observe(TTFT, Some(c1.clone()), true).unwrap();
    assert_eq!(bucket_quantile(d, 1.0).unwrap(), 0.22);
    let d = w.observe(TTFT, Some(c2.clone()), true).unwrap();
    assert_eq!(d.iter().map(|x| x.1).fold(0.0, f64::max), 4.0);
    // nothing new since then: no quantile
    let d = w.observe(TTFT, Some(c2.clone()), true).unwrap();
    assert_eq!(bucket_quantile(d, 0.5), None);
    // a counter reset: no window this poll, the reset read is the new baseline (a read alone does not adopt it)
    assert_eq!(w.observe(TTFT, Some(c0.clone()), false), None);
    assert_eq!(w.observe(TTFT, Some(c0), true), None);
    let d = w.observe(TTFT, Some(c1), true).unwrap();
    assert_eq!(d.iter().map(|x| x.1).fold(0.0, f64::max), 2.0);
    // the family disappears on a poll: its baseline is dropped, so the next poll starts afresh
    assert_eq!(w.observe(TTFT, None, true), None);
    assert_eq!(w.observe(TTFT, Some(c2), true), None);
    assert_eq!(w.observe(TTFT, Some(HistCounts::new()), true), None);
}

#[test]
fn test_hostile_histograms_are_bounded_fast_and_nan_bounds_ignored() {
    // 20,000 distinct bounds in one label set: linear, and only HIST_MAX_BUCKETS kept
    let mut text = String::new();
    for i in 0..20_000 {
        text.push_str(&format!("{TTFT}_bucket{{model=\"m\",le=\"{i}.5\"}} {i}\n"));
    }
    let samples = parse_prom(&text);
    let t = std::time::Instant::now();
    let c = hist_counts(&samples, TTFT);
    let d = hist_delta(&c, &c).unwrap();
    // linear: a few ms even in a debug build (the quadratic search took about 1.4 s)
    assert!(t.elapsed().as_secs_f64() < 0.5, "{:?}", t.elapsed());
    assert_eq!(c["model=m"].len(), HIST_MAX_BUCKETS);
    assert_eq!(d.len(), HIST_MAX_BUCKETS);
    // 1,000 label sets: only HIST_MAX_SETS kept
    let mut text = String::new();
    for i in 0..1000 {
        text.push_str(&format!("{TTFT}_bucket{{model=\"m{i}\",le=\"1\"}} 1\n"));
    }
    assert_eq!(counts(&text, TTFT).len(), HIST_MAX_SETS);
    // a NaN / -Inf bound or a NaN count is not a bucket, so it can never look like a reset
    let base = format!(
        "{TTFT}_bucket{{model=\"m\",le=\"0.5\"}} 1\n{TTFT}_bucket{{model=\"m\",le=\"+Inf\"}} 1\n"
    );
    let junk = |n: u32| {
        format!(
            "{TTFT}_bucket{{model=\"m\",le=\"0.5\"}} {n}\n{TTFT}_bucket{{model=\"m\",le=\"+Inf\"}} {n}\n\
             {TTFT}_bucket{{model=\"m\",le=\"NaN\"}} {}\n{TTFT}_bucket{{model=\"m\",le=\"-Inf\"}} 0\n\
             {TTFT}_bucket{{model=\"m\",le=\"1\"}} NaN\n",
            100 - n
        )
    };
    assert_eq!(counts(&junk(1), TTFT)["model=m"].len(), 2);
    let mut w = HistWindows::default();
    assert_eq!(w.observe(TTFT, Some(counts(&base, TTFT)), true), None);
    let d = w.observe(TTFT, Some(counts(&junk(3), TTFT)), true).unwrap();
    assert_eq!(bucket_quantile(d, 0.5).unwrap(), 0.25);
}

// ── the adapter ─────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_windowed_quantiles_across_polls() {
    let live = Live::new().await;
    let (fe0, wk0) = (fx("frontend_metrics.prom"), fx("worker_metrics.prom"));
    // first poll: the since-start keys are there, the windowed ones not yet
    let p1 = live.poll().await;
    assert!(by(&p1, "dyn.ttft_ms").len() == 2 && windowed(&p1).is_empty());
    // second poll: 3 requests finished with TTFT in (0.1, 0.22] s, 1 with queue time in (0.3, 0.5] s
    let fe1 = bump(&bump(&fe0, TTFT, 0.1, 3.0), ITL, 0.013, 10.0);
    let wk1 = bump(&wk0, QUEUE, 0.3, 1.0);
    let wk1 = bump(&wk1, "vllm:request_prefill_time_seconds", 0.5, 1.0);
    let wk1 = bump(&wk1, "vllm:request_decode_time_seconds", 2.0, 1.0);
    let wk1 = bump(&wk1, "vllm:request_inference_time_seconds", 2.0, 1.0);
    live.frontend(&fe1);
    live.worker(&wk1);
    let p2 = live.poll().await;
    let ttft = by(&p2, "dyn.ttft_window_ms");
    assert_eq!(
        ttft.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from([
            lab(&[("q", "p50")]),
            lab(&[("q", "p95")]),
            lab(&[("q", "p99")])
        ])
    );
    assert!((ttft[&lab(&[("q", "p50")])] - 160.0).abs() < 1e-9);
    assert!(ttft[&lab(&[("q", "p99")])] <= 220.0 + 1e-9);
    // the since-start TTFT still includes the first request (0.302 s): its p95 is above anything in the window
    assert!(by(&p2, "dyn.ttft_ms")[&lab(&[("q", "p95")])] > 220.0);
    let itl = by(&p2, "dyn.itl_window_ms");
    assert!(itl[&lab(&[("q", "p50")])] > 13.0 && itl[&lab(&[("q", "p50")])] <= 24.0);
    let phase = by(&p2, "dyn.phase_ms");
    assert_eq!(phase.len(), 8, "4 phases x p50/p95: {phase:?}");
    assert!((phase[&lab(&[("phase", "queue"), ("q", "p50")])] - 400.0).abs() < 1e-9);
    for ph in ["queue", "prefill", "decode", "inference"] {
        let (p50, p95) = (
            phase[&lab(&[("phase", ph), ("q", "p50")])],
            phase[&lab(&[("phase", ph), ("q", "p95")])],
        );
        assert!(p50 > 0.0 && p50 <= p95, "{ph}");
    }
    assert!(
        p2.iter()
            .filter(|p| WINDOWED_KEYS.contains(&p.key.as_str()))
            .all(|p| p.unit == "ms")
    );
    // third poll, nothing finished: no windowed point (never 0)
    let p3 = live.poll().await;
    assert!(windowed(&p3).is_empty(), "{:?}", windowed(&p3));
    // fourth poll: the frontend restarted (counters back to the capture): a reset, skipped
    live.frontend(&fe0);
    let p4 = live.poll().await;
    assert!(by(&p4, "dyn.ttft_window_ms").is_empty());
    // fifth poll: one more request after the restart, measured from the reset read
    live.frontend(&bump(&fe0, TTFT, 1.0, 1.0));
    let p5 = live.poll().await;
    let t = by(&p5, "dyn.ttft_window_ms");
    let p50 = t[&lab(&[("q", "p50")])];
    assert!(p50 > 1000.0 && p50 <= 2200.0, "{p50}");
    // the worker stops being read: its windows start afresh
    live.f.runner.clear_rules();
    dynamo_support::wire(&live.f.runner, vec![], "");
    let p6 = live.poll().await;
    assert!(by(&p6, "dyn.phase_ms").is_empty());
}

#[tokio::test]
async fn test_a_frontend_outage_restarts_the_windows() {
    let live = Live::new().await;
    let fe0 = fx("frontend_metrics.prom");
    live.poll().await;
    live.f.mock.get("/metrics", Reply::Close);
    assert!(!live.a.poll_metrics().await.unwrap().available);
    // back with more requests: the first read after the gap is a new baseline, not a window over the outage
    live.frontend(&bump(&fe0, TTFT, 0.1, 5.0));
    assert!(by(&live.poll().await, "dyn.ttft_window_ms").is_empty());
    live.frontend(&bump(&bump(&fe0, TTFT, 0.1, 5.0), TTFT, 0.1, 1.0));
    assert_eq!(by(&live.poll().await, "dyn.ttft_window_ms").len(), 3);
}

#[tokio::test]
async fn test_capacity_preemptions_and_acceptance_per_position() {
    let live = Live::new().await;
    let pts = live.poll().await;
    assert_eq!(
        by(&pts, "dyn.capacity"),
        BTreeMap::from([
            (lab(&[("kind", "context_length")]), 262_144.0),
            (lab(&[("kind", "kv_blocks")]), 359.0),
            (lab(&[("kind", "max_batched_tokens")]), 8192.0),
            (lab(&[("kind", "max_num_seqs")]), 64.0),
        ])
    );
    assert_eq!(
        by(&pts, "dyn.preemptions_total"),
        BTreeMap::from([(vec![], 0.0)])
    );
    // 15 drafts: position 0 accepted 15 times, position 1 11 times
    assert_eq!(
        by(&pts, "dyn.spec_accept_pos"),
        BTreeMap::from([
            (lab(&[("pos", "0")]), 1.0),
            (lab(&[("pos", "1")]), 11.0 / 15.0)
        ])
    );
    let unit = |k: &str| pts.iter().find(|p| p.key == k).unwrap().unit.clone();
    assert_eq!(unit("dyn.spec_accept_pos"), "ratio");
    assert_eq!(unit("dyn.preemptions_total"), "1");
    // valid series names, unique, well under the cap
    let names: HashSet<String> = pts.iter().map(series_name).collect();
    assert_eq!(names.len(), pts.len());
    assert!(
        names
            .iter()
            .all(|n| nemo_console::api::observe::KEY_RE.is_match(n)),
        "{names:?}"
    );
    assert!(names.len() < MAX_SERIES_PER_SYSTEM / 4, "{}", names.len());
}

#[tokio::test]
async fn test_without_a_worker_or_families_the_new_keys_are_absent() {
    let f = Fix::new().await;
    dynamo_support::wire(&f.runner, vec![], "");
    f.mock.get("/metrics", Reply::text(200, "# nothing\n"));
    let a = f.make();
    for _ in 0..2 {
        let snap = a.metrics().await.unwrap();
        let keys: HashSet<&str> = snap.points.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, HashSet::from(["dyn.up", "dyn.worker_up"]));
    }
    // the frontend alone: capacity yes, worker keys no
    dynamo_support::http_live(&f.mock, true);
    let snap = a.metrics().await.unwrap();
    let keys: HashSet<&str> = snap.points.iter().map(|p| p.key.as_str()).collect();
    assert!(keys.contains("dyn.capacity"));
    for absent in [
        "dyn.preemptions_total",
        "dyn.spec_accept_pos",
        "dyn.phase_ms",
    ] {
        assert!(!keys.contains(absent), "{absent}");
    }
}

#[tokio::test]
async fn test_ui_reads_do_not_move_the_pollers_window() {
    let live = Live::new().await;
    let fe0 = fx("frontend_metrics.prom");
    live.poll().await;
    live.frontend(&bump(&fe0, TTFT, 0.1, 3.0));
    // the UI reads many times between two polls: each sees the 3 new requests ...
    for _ in 0..20 {
        let t = by(&live.peek().await, "dyn.ttft_window_ms");
        assert!((t[&lab(&[("q", "p50")])] - 160.0).abs() < 1e-9);
    }
    // ... and the poller still gets them all
    let t = by(&live.poll().await, "dyn.ttft_window_ms");
    assert!((t[&lab(&[("q", "p50")])] - 160.0).abs() < 1e-9);
    // a failed UI read does not reset the poller's windows either
    live.f.mock.get("/metrics", Reply::Close);
    assert!(!live.a.metrics().await.unwrap().available);
    live.frontend(&bump(&bump(&fe0, TTFT, 0.1, 3.0), TTFT, 1.0, 2.0));
    let t = by(&live.poll().await, "dyn.ttft_window_ms");
    assert!(t[&lab(&[("q", "p50")])] > 1000.0);
}

#[test]
fn test_spec_positions_are_strict_and_capped() {
    let drafts = "vllm:spec_decode_num_drafts_total{engine=\"0\"} 10\n";
    let mut text = drafts.to_owned();
    for pos in ["0", "1", "07", "x", "-1", "1e1", "100", "", " 2", "NaN"] {
        text.push_str(&format!(
            "vllm:spec_decode_num_accepted_tokens_per_pos_total{{position=\"{pos}\"}} 5\n"
        ));
    }
    text.push_str("vllm:spec_decode_num_accepted_tokens_per_pos_total{position=\"3\"} NaN\n");
    text.push_str(&format!(
        "vllm:spec_decode_num_accepted_tokens_per_pos_total{{position=\"{}\"}} 5\n",
        "9".repeat(100_000)
    ));
    let wm = parse_prom(&text);
    let pts = plain_points_9_14(&[], Some(&wm));
    assert_eq!(
        by(&pts, "dyn.spec_accept_pos"),
        BTreeMap::from([
            (lab(&[("pos", "0")]), 0.5),
            (lab(&[("pos", "1")]), 0.5),
            (lab(&[("pos", "7")]), 0.5),
        ])
    );
    let mut text = drafts.to_owned();
    for pos in 0..99 {
        text.push_str(&format!(
            "vllm:spec_decode_num_accepted_tokens_per_pos_total{{position=\"{pos}\"}} 1\n"
        ));
    }
    let pts = plain_points_9_14(&[], Some(&parse_prom(&text)));
    assert_eq!(by(&pts, "dyn.spec_accept_pos").len(), SPEC_MAX_POSITIONS);
    // NaN / Inf gauges and counters give no point
    let text = "vllm:num_preemptions_total{a=\"1\"} NaN\ndynamo_frontend_model_context_length{model=\"m\"} +Inf\n";
    let fe = parse_prom(text);
    let pts = plain_points_9_14(&fe, Some(&fe));
    assert!(pts.is_empty(), "{pts:?}");
}
