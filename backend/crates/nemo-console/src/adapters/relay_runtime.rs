//! Relay runtime (addendum 9.17): the gateway's own resource figures and the Switchyard plugin's routing rates, read
//! from Prometheus with FIXED expressions, and the gateway's runtime diagnostics, read with `nemo-relay diagnostics`
//! and mapped through a strict allowlist. Part of the Relay adapter (PORTING.md D-44); everything here is pure
//! (parsing and mapping), the adapter does the I/O.
//!
//! Security: no PromQL ever comes from a request, Prometheus labels never leave this module (the filesystem label
//! holds a home path), and of a diagnostics answer only counts, identifier-checked codes and checked timestamps are
//! kept. Event `detail` (Relay: "can contain private data that has not been sanitized"), `correlation`, `origin`,
//! ownership, instance and process ids and `context` are never read into the answer.

use std::cmp::Reverse;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use crate::adapters::base::CommandResult;
use crate::adapters::switchyard::re;
use crate::domain::models::MetricPoint;

// ── Prometheus: the new Relay metric keys ───────────────────────────────────────────────────────────

/// Per query: the timeout (seconds) and the largest answer read (bytes); at most this many series are looked at.
pub const PROM_TIMEOUT_S: f64 = 1.5;
/// The points are reused this long (one refresh at a time); while a refresh runs, points up to `PROM_STALE_S` old are
/// served instead of waiting for it.
pub const PROM_CACHE_S: f64 = 15.0;
pub const PROM_STALE_S: f64 = 60.0;
pub const PROM_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const PROM_MAX_SERIES: usize = 200;

const RES: &str = "nemo_relay_resource_";
const CPU: &str = "nemo_relay_resource_cpu_consumption_rate_logical_processors";
const RSS: &str = "nemo_relay_resource_memory_resident_kibibytes";
const ACCOUNTED: &str = "nemo_relay_resource_memory_environment_accounted_kibibytes";
const THREADS: &str = "nemo_relay_resource_thread_count_threads";
const FDS: &str = "nemo_relay_resource_process_open_file_descriptor_count_file_descriptors";
const DISK_READ: &str = "nemo_relay_resource_disk_read_throughput_bytes_per_second";
const DISK_WRITE: &str = "nemo_relay_resource_disk_write_throughput_bytes_per_second";
const FS_AVAIL: &str = "nemo_relay_resource_disk_filesystem_available_capacity_bytes";
const FS_TOTAL: &str = "nemo_relay_resource_disk_filesystem_total_capacity_bytes";
const LIMIT_EVENTS: &str = "nemo_relay_resource_limit_event_count_events";

/// The gauges, in one instant query (`__name__` tells them apart).
pub const GAUGE_QUERY: &str = concat!(
    r#"{__name__=~"nemo_relay_resource_(cpu_consumption_rate_logical_processors|memory_resident_kibibytes|"#,
    r#"memory_environment_accounted_kibibytes|thread_count_threads|"#,
    r#"process_open_file_descriptor_count_file_descriptors|disk_read_throughput_bytes_per_second|"#,
    r#"disk_write_throughput_bytes_per_second|disk_filesystem_available_capacity_bytes|"#,
    r#"disk_filesystem_total_capacity_bytes|limit_event_count_events)",job="nemo-relay"}"#
);

/// The two pressure stall rates (ms of stall per second), told apart by a fixed label `k`.
pub const PRESSURE_QUERY: &str = concat!(
    r#"label_replace(max(rate(nemo_relay_resource_cpu_pressure_some_stall_time_milliseconds{job="nemo-relay"}[5m])), "k", "cpu", "", "")"#,
    " or ",
    r#"label_replace(max(rate(nemo_relay_resource_memory_pressure_some_stall_time_milliseconds{job="nemo-relay"}[5m])), "k", "mem", "", "")"#
);

/// The Switchyard plugin's routing rates inside the gateway, told apart by a fixed label `k`.
pub const ROUTING_QUERY: &str = concat!(
    r#"label_replace(sum(rate(switchyard_routing_requests_total{job="nemo-relay"}[5m])), "k", "rps", "", "")"#,
    " or ",
    // the failure counter appears only after the first failure: 0 while requests are counted
    r#"label_replace(sum(rate(switchyard_routing_failures_total{job="nemo-relay"}[5m])) or (sum(rate(switchyard_routing_requests_total{job="nemo-relay"}[5m])) * 0), "k", "fail", "", "")"#,
    " or ",
    r#"label_replace(sum(rate(switchyard_routing_llm_tokens_total{job="nemo-relay"}[5m])), "k", "tok", "", "")"#
);

/// The routing overhead p95 (ms), with the older bucket name as the fallback.
pub const OVERHEAD_QUERY: &str = concat!(
    r#"histogram_quantile(0.95, sum by (le) (rate(switchyard_routing_overhead_milliseconds_bucket{job="nemo-relay"}[5m])))"#,
    " or ",
    r#"histogram_quantile(0.95, sum by (le) (rate(switchyard_routing_overhead_ms_bucket{job="nemo-relay"}[5m])))"#
);

/// Every expression this module sends, in the order `resource_points` takes their answers (4 of the 6 allowed).
/// The freshness of the samples is NOT read from Prometheus (the OTLP export repeats the last sample every few
/// seconds, so a sample's Prometheus timestamp is its export time): it is the capture answer's newest ATOF mark.
pub const QUERIES: [&str; 4] = [GAUGE_QUERY, PRESSURE_QUERY, ROUTING_QUERY, OVERHEAD_QUERY];

/// `url` without a trailing slash when it is http(s) to a loopback address or `localhost` and has no user info;
/// else `None`. Prometheus and the gateway URL handed to the CLI pass through it.
#[must_use]
pub fn loopback_url(raw: &str) -> Option<String> {
    let u = url::Url::parse(raw).ok()?;
    let loopback = match u.host()? {
        url::Host::Ipv4(a) => a.is_loopback(),
        url::Host::Ipv6(a) => a.is_loopback(),
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
    };
    (loopback
        && matches!(u.scheme(), "http" | "https")
        && u.username().is_empty()
        && u.password().is_none())
    .then(|| raw.trim_end_matches('/').to_owned())
}

/// One series of an instant-query answer: its labels (only ever looked at here) and its value.
pub type PromRow = (Map<String, Value>, f64);

/// The series of a Prometheus `/api/v1/query` answer (vector or scalar result), at most `PROM_MAX_SERIES`. `None`
/// when the body is not a successful answer. Values that are not numbers are dropped here; non-finite ones are
/// kept for the caller to drop.
#[must_use]
pub fn prom_rows(body: &[u8]) -> Option<Vec<PromRow>> {
    let d: Value = serde_json::from_slice(body).ok()?;
    let d = d.as_object()?;
    if d.get("status").and_then(Value::as_str) != Some("success") {
        return None;
    }
    let data = d.get("data")?.as_object()?;
    let num = |pair: &Value| -> Option<f64> {
        let s = pair.as_array()?.get(1)?.as_str()?;
        if s.len() > 64 {
            return None;
        }
        s.parse::<f64>().ok()
    };
    match data.get("result")? {
        // a scalar answer: [ts, "value"]
        v @ Value::Array(a) if a.len() == 2 && a.first().is_some_and(Value::is_number) => {
            Some(num(v).map(|x| vec![(Map::new(), x)]).unwrap_or_default())
        }
        Value::Array(rows) => Some(
            rows.iter()
                .take(PROM_MAX_SERIES)
                .filter_map(|r| {
                    let r = r.as_object()?;
                    let labels = r
                        .get("metric")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    Some((labels, num(r.get("value")?)?))
                })
                .collect(),
        ),
        _ => None,
    }
}

fn ok_value(v: f64) -> Option<f64> {
    (v.is_finite() && v >= 0.0).then_some(v)
}

fn label<'a>(labels: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    labels.get(name).and_then(Value::as_str)
}

/// The labels of a series without its metric name: pairs the available and total series of one filesystem.
fn series_key(labels: &Map<String, Value>) -> String {
    let mut pairs: Vec<(&str, &str)> = labels
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("")))
        .collect();
    pairs.sort_unstable();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}\u{1}{v}"))
        .collect::<Vec<_>>()
        .join("\u{2}")
}

fn point(key: &str, value: f64, unit: &str) -> MetricPoint {
    let mut p = MetricPoint::new(key, value);
    p.unit = unit.to_owned();
    p
}

/// The new Relay metric keys from the answers to [`QUERIES`] (`None` = that query failed: its keys are omitted). A
/// key is emitted only for a finite value >= 0; no label is ever forwarded.
#[must_use]
pub fn resource_points(answers: &[Option<Vec<PromRow>>; 4]) -> Vec<MetricPoint> {
    let mut out: Vec<MetricPoint> = Vec::new();
    let [gauges, pressure, routing, overhead] = answers;
    if let Some(rows) = gauges {
        let by_name = |name: &str| -> Vec<(&Map<String, Value>, f64)> {
            rows.iter()
                .filter(|(l, _)| label(l, "__name__") == Some(name))
                .filter_map(|(l, v)| ok_value(*v).map(|v| (l, v)))
                .collect()
        };
        // one series per name while the gateway process is the only scope; should two be seen (a restart inside
        // the lookback window), the larger one is shown rather than a sum that counts the process twice
        let max_of = |name: &str| by_name(name).iter().map(|(_, v)| *v).reduce(f64::max);
        for (name, key, scale, unit) in [
            (CPU, "relay.gw_cpu", 1.0, "cores"),
            (RSS, "relay.gw_rss", 1024.0, "B"),
            (ACCOUNTED, "relay.gw_mem_accounted", 1024.0, "B"),
            (THREADS, "relay.gw_threads", 1.0, "1"),
            (FDS, "relay.gw_fds", 1.0, "1"),
            (DISK_READ, "relay.gw_disk_read", 1.0, "B/s"),
            (DISK_WRITE, "relay.gw_disk_write", 1.0, "B/s"),
        ] {
            if let Some(v) = max_of(name).and_then(|v| ok_value(v * scale)) {
                out.push(point(key, v, unit));
            }
        }
        // the filesystem ATOF is written to: the one with the least free space, and its used share
        let avail = by_name(FS_AVAIL);
        let total = by_name(FS_TOTAL);
        if let Some((l, free)) = avail.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)) {
            out.push(point("relay.atof_fs_free", free, "B"));
            let k = series_key(l);
            if let Some((_, t)) = total.iter().find(|(tl, _)| series_key(tl) == k)
                && *t > 0.0
                && let Some(used) = ok_value(1.0 - free / t)
            {
                out.push(point("relay.atof_fs_used", used.min(1.0), "ratio"));
            }
        }
        let limit = by_name(LIMIT_EVENTS);
        if !limit.is_empty()
            && let Some(v) = ok_value(limit.iter().map(|(_, v)| *v).sum())
        {
            out.push(point("relay.gw_limit_events", v, "1"));
        }
    }
    let tagged = |rows: &Option<Vec<PromRow>>, tag: &str| -> Option<f64> {
        rows.as_ref()?
            .iter()
            .filter(|(l, _)| label(l, "k") == Some(tag))
            .filter_map(|(_, v)| ok_value(*v))
            .reduce(f64::max)
    };
    for (tag, key) in [
        ("cpu", "relay.gw_cpu_pressure"),
        ("mem", "relay.gw_mem_pressure"),
    ] {
        if let Some(v) = tagged(pressure, tag) {
            out.push(point(key, v, "ms/s"));
        }
    }
    // the quantile is a single label-less value; a labelled series is not it
    let single = |rows: &Option<Vec<PromRow>>| -> Option<f64> {
        rows.as_ref()?
            .iter()
            .filter(|(l, _)| l.is_empty())
            .filter_map(|(_, v)| ok_value(*v))
            .reduce(f64::max)
    };
    for (tag, key, unit) in [
        ("rps", "relay.route_rps", "req/s"),
        ("fail", "relay.route_failures", "req/s"),
        ("tok", "relay.route_tokens", "tok/s"),
    ] {
        if let Some(v) = tagged(routing, tag) {
            out.push(point(key, v, unit));
        }
    }
    if let Some(v) = single(overhead) {
        out.push(point("relay.route_overhead_p95", v, "ms"));
    }
    out
}

/// True when every query of this module names only Relay's own series under `job="nemo-relay"`.
#[must_use]
pub fn queries_are_fixed() -> bool {
    QUERIES.iter().all(|q| {
        q.contains(r#"job="nemo-relay""#) && (q.contains(RES) || q.contains("switchyard_routing_"))
    })
}

// ── runtime diagnostics ─────────────────────────────────────────────────────────────────────────────

/// Relay's counters are decimal strings; a saturated counter, or one above 2^53, is shown as 2^53.
pub const MAX_COUNT: u64 = 1 << 53;
pub const MAX_COMPONENTS: usize = 64;
pub const MAX_EVENTS: usize = 256;
/// Entries of a list that are looked at at all (Relay keeps at most 64 components and 256 events).
const SCAN_ITEMS: usize = 4096;
/// The CLI's output cap and per-call timeout; `--max-response-bytes` of the two calls.
pub const DIAG_MAX_OUTPUT: usize = 2 * 1024 * 1024;
pub const DIAG_TIMEOUT_S: f64 = 15.0;
/// The whole read (every page and the memory call) ends by then; what was gathered is answered (still truncated).
pub const DIAG_DEADLINE_S: f64 = 30.0;
pub const DIAG_SNAPSHOT_BYTES: &str = "1048576";
pub const DIAG_MEMORY_BYTES: &str = "262144";
/// How long an answer is reused (a failure for a shorter time, so a fixed gateway shows up soon).
pub const DIAG_CACHE_S: f64 = 30.0;
/// Snapshot pages read at most (Relay caps one page at its `max_snapshot_bytes` and starts at the OLDEST retained
/// event; `continuation` names the next page).
pub const DIAG_MAX_PAGES: usize = 4;
/// The largest `{"continuation": ...}` sent back: raw bytes, and URI-encoded bytes with `query=` (the CLI's cap is
/// 4096).
pub const MAX_QUERY_BYTES: usize = 3072;
pub const MAX_QUERY_URI_BYTES: usize = 4096;
/// Below this many bytes of the output budget no further page is asked for.
pub const MIN_PAGE_BUDGET: usize = 64 * 1024;
pub const DIAG_FAIL_CACHE_S: f64 = 5.0;

/// `coverage.loss` names that are passed on (each a `{value, saturated}` counter).
pub const LOSS_NAMES: [&str; 18] = [
    "events_evicted",
    "events_omitted",
    "components_evicted",
    "components_rejected",
    "keys_rejected",
    "observations_rejected",
    "messages_truncated",
    "details_omitted",
    "correlations_truncated",
    "correlations_omitted",
    "metadata_rejected",
    "classifications_overflowed",
    "windows_rejected",
    "queries_rejected",
    "stale_observations",
    "projection_deliveries_omitted",
    "projections_panicked",
    "projections_suppressed",
];

static KEY_RE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\A[a-z0-9_.-]{1,96}\z"));
static VERSION_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"\A[0-9]{1,6}\.[0-9]{1,6}\.[0-9]{1,6}(?:[-+.][A-Za-z0-9.+-]{1,32})?\z"));
static TS_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(
        r"\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?(?:Z|[+-][0-9]{2}:[0-9]{2})\z",
    )
});

/// A count from Relay: a decimal string (any length; above 2^53 = 2^53), a non-negative JSON integer, or a
/// `{value, saturated}` counter (saturated = 2^53). Anything else is unknown.
#[must_use]
pub fn count(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::String(s) => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let digits = s.trim_start_matches('0');
            if digits.len() > 16 {
                return Some(MAX_COUNT);
            }
            Some(digits.parse::<u64>().unwrap_or(0).min(MAX_COUNT))
        }
        Value::Number(n) => n.as_u64().map(|n| n.min(MAX_COUNT)),
        Value::Object(o) => {
            if o.get("saturated") == Some(&Value::Bool(true)) {
                Some(MAX_COUNT)
            } else {
                count(o.get("value"))
            }
        }
        _ => None,
    }
}

fn count_json(v: Option<&Value>) -> Value {
    count(v).map_or(Value::Null, |n| json!(n))
}

fn sat_add(a: u64, b: u64) -> u64 {
    a.saturating_add(b).min(MAX_COUNT)
}

/// An RFC 3339 time as Relay prints it, when it has that shape and is a real time: (the text, nanoseconds since the
/// epoch for ordering).
#[must_use]
pub fn timestamp(v: Option<&Value>) -> Option<(String, i128)> {
    let s = v?.as_str()?;
    if !TS_RE.is_match(s) {
        return None;
    }
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some((s.to_owned(), t.unix_timestamp_nanos()))
}

/// An identifier-shaped code, `null` when absent, `"other"` for anything else.
fn code(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) if KEY_RE.is_match(s) => Value::String(s.clone()),
        Some(_) => Value::String("other".to_owned()),
    }
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, k| cur.as_object()?.get(*k))
}

fn list(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(a)) => &a[..a.len().min(SCAN_ITEMS)],
        _ => &[],
    }
}

/// Why diagnostics could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The gateway runs without diagnostics (the CLI says inspection is unavailable).
    Disabled,
    Unreachable,
    /// The CLI has no `diagnostics` subcommand (Relay < 0.11).
    UnsupportedVersion,
    Error,
}

impl Reason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Disabled => "disabled",
            Reason::Unreachable => "unreachable",
            Reason::UnsupportedVersion => "unsupported_version",
            Reason::Error => "error",
        }
    }
}

/// The reason of a CLI call that gave no answer. Only fixed words of the CLI's text are looked at; none of it is
/// passed on.
#[must_use]
pub fn failure_reason(r: &CommandResult) -> Reason {
    if r.timed_out {
        return Reason::Unreachable;
    }
    let mut text = String::new();
    for part in [&r.stderr, &r.stdout] {
        let end = part
            .char_indices()
            .map(|(i, _)| i)
            .find(|i| *i >= 8192)
            .unwrap_or(part.len());
        text.push_str(&part[..end].to_ascii_lowercase());
        text.push('\n');
    }
    if text.contains("unrecognized subcommand") || text.contains("which wasn't expected") {
        return Reason::UnsupportedVersion;
    }
    if text.contains("diagnostic inspection unavailable") {
        if text.contains("failed to connect")
            || text.contains("connection refused")
            || text.contains("timed out")
        {
            return Reason::Unreachable;
        }
        return Reason::Disabled;
    }
    Reason::Error
}

/// The answer when diagnostics cannot be read: every other field null or empty.
#[must_use]
pub fn unavailable(reason: Reason) -> Value {
    json!({
        "available": false,
        "reason": reason.as_str(),
        "relay_version": null,
        "captured_at": null,
        "elapsed_s": null,
        "totals": null,
        "last_failure_at": null,
        "components": [],
        "events": [],
        "loss": {},
        "retention": null,
        "memory": null,
    })
}

/// The stdout of one CLI call as a JSON object, or `None` (empty, not JSON, not an object, nested too deep).
#[must_use]
pub fn parse_output(stdout: &str) -> Option<Value> {
    let t = stdout.trim();
    if !t.starts_with('{') {
        return None;
    }
    serde_json::from_str::<Value>(t)
        .ok()
        .filter(Value::is_object)
}

#[derive(Default)]
struct Group {
    instances: u64,
    successes: u64,
    failures: u64,
    occurrences: u64,
    last_failure: Option<(String, i128)>,
}

fn newer(a: Option<(String, i128)>, b: Option<(String, i128)>) -> Option<(String, i128)> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y.1 > x.1 { y } else { x }),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Map a diagnostics snapshot (and the `--memory` answer, when that call worked) onto the console's answer.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn map_diagnostics(snapshot: &Value, memory: Option<&Value>) -> Value {
    map_pages(
        std::slice::from_ref(snapshot),
        page_truncated(snapshot),
        memory,
    )
}

/// Whether Relay says this page did not hold everything: `coverage.retention.query_truncated`, or a continuation.
#[must_use]
pub fn page_truncated(page: &Value) -> bool {
    at(page, &["coverage", "retention", "query_truncated"]) == Some(&Value::Bool(true))
        || page.get("continuation").is_some_and(|c| !c.is_null())
}

/// The `--query` text that asks for the page after this one: `{"continuation": <the object as received>}`, compact.
/// `None` when there is no continuation, it is not an object, or the text is too large for the CLI's query cap.
#[must_use]
pub fn next_query(page: &Value) -> Option<String> {
    let c = page.get("continuation")?;
    if !c.is_object() {
        return None;
    }
    let q = serde_json::to_string(&json!({ "continuation": c })).ok()?;
    let uri = 6 + q
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                1
            } else {
                3
            }
        })
        .sum::<usize>();
    (q.len() <= MAX_QUERY_BYTES && uri <= MAX_QUERY_URI_BYTES).then_some(q)
}

/// The answer from one or more snapshot pages (oldest page first): totals, lifetime and loss from the FIRST page;
/// components of every page grouped by key; the newest `MAX_EVENTS` events of all pages (an event seen twice, by
/// sequence, counts once); retention merged over the pages and what was gathered, `truncated` as given (still
/// truncated after the last page read). A continuation is never read into the answer.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn map_pages(pages: &[Value], truncated: bool, memory: Option<&Value>) -> Value {
    let empty = Value::Null;
    let snapshot = pages.first().unwrap_or(&empty);
    let totals_of = |t: Option<&Value>| -> Value {
        let c = |name: &str| count_json(t.and_then(|t| t.get(name)));
        json!({
            "recorded": c("recorded_events_total"),
            "successes": c("successes_total"),
            "failures": c("failures_total"),
            "unspecified": c("unspecified_total"),
            "evictions": c("history_evictions_total"),
        })
    };
    let lifetime = snapshot.get("lifetime");
    let mut dropped: u64 = 0;
    // components, grouped by key
    let mut groups: IndexMap<String, Group> = IndexMap::new();
    for c in pages.iter().flat_map(|p| list(p.get("components"))) {
        let key = match at(c, &["identity", "key"]).and_then(Value::as_str) {
            Some(k) if KEY_RE.is_match(k) => k.to_owned(),
            _ => {
                dropped = sat_add(dropped, 1);
                continue;
            }
        };
        let t = at(c, &["lifetime", "totals"]);
        let n = |name: &str| count(t.and_then(|t| t.get(name))).unwrap_or(0);
        let g = groups.entry(key).or_default();
        g.instances = sat_add(g.instances, 1);
        g.successes = sat_add(g.successes, n("successes_total"));
        g.failures = sat_add(g.failures, n("failures_total"));
        g.occurrences = sat_add(g.occurrences, n("occurrences_total"));
        g.last_failure = newer(
            g.last_failure.take(),
            timestamp(at(c, &["lifetime", "last_failure_at"])),
        );
    }
    let mut groups: Vec<(String, Group)> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.failures.cmp(&a.1.failures).then_with(|| a.0.cmp(&b.0)));
    for (_, g) in groups.iter().skip(MAX_COMPONENTS) {
        dropped = sat_add(dropped, g.instances);
    }
    let components: Vec<Value> = groups
        .into_iter()
        .take(MAX_COMPONENTS)
        .map(|(key, g)| {
            json!({
                "key": key,
                "instances": g.instances,
                "successes": g.successes,
                "failures": g.failures,
                "occurrences": g.occurrences,
                "last_failure_at": g.last_failure.map(|(s, _)| s),
            })
        })
        .collect();
    // events, newest first
    let mut seen_seq: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut events: Vec<(Option<u64>, Value)> = pages
        .iter()
        .flat_map(|p| list(p.get("events")))
        .filter(|e| e.is_object())
        // the same event on two pages (a page read twice) counts once
        .filter(|e| count(e.get("sequence")).is_none_or(|s| seen_seq.insert(s)))
        .map(|e| {
            let seq = count(e.get("sequence"));
            let component = match at(e, &["component", "key"]) {
                None | Some(Value::Null) => Value::Null,
                Some(Value::String(k)) if KEY_RE.is_match(k) => Value::String(k.clone()),
                Some(_) => Value::String("other".to_owned()),
            };
            let v = json!({
                "seq": seq,
                "at": timestamp(e.get("observed_at")).map(|(s, _)| s),
                "component": component,
                "code": code(at(e, &["classification", "canonical_code"])),
                "failure_class": code(at(e, &["classification", "failure_class"])),
                "severity": code(e.get("severity")),
                "outcome": code(e.get("outcome")),
                "count": count_json(e.get("count")),
            });
            (seq, v)
        })
        .collect();
    events.sort_by_key(|(seq, _)| Reverse(*seq));
    let gathered = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let seqs = || events.iter().filter_map(|(s, _)| *s);
    // retention: Relay's figures of every page read, widened by what was gathered
    let rel = |name: &'static str| {
        pages
            .iter()
            .filter_map(move |p| count(at(p, &["coverage", "retention", name])))
    };
    let oldest = rel("oldest_event_sequence").chain(seqs()).min();
    let latest = rel("latest_event_sequence").chain(seqs()).max();
    let retained = rel("events_retained")
        .chain((gathered > 0).then_some(gathered.min(MAX_COUNT)))
        .max();
    events.truncate(MAX_EVENTS);
    // loss: the allowlisted names only, plus what the console itself left out
    let mut loss = Map::new();
    let relay_loss = at(snapshot, &["coverage", "loss"]);
    for name in LOSS_NAMES {
        if let Some(n) = count(relay_loss.and_then(|l| l.get(name))) {
            loss.insert(name.to_owned(), json!(n));
        }
    }
    loss.insert("console_dropped".to_owned(), json!(dropped));
    let memory = memory
        .and_then(|m| m.get("aggregate"))
        .filter(|a| a.is_object())
        .map_or(Value::Null, |a| {
            let c = |name: &str| count_json(a.get(name));
            json!({
                "reserved_bytes": c("reserved_bytes"),
                "events": c("events"),
                "event_capacity": c("event_capacity"),
                "detail_bytes": c("detail_bytes"),
                "detail_capacity_bytes": c("detail_capacity_bytes"),
            })
        });
    #[allow(clippy::cast_precision_loss)]
    let elapsed_s = count(lifetime.and_then(|l| l.get("elapsed_millis")))
        .map_or(Value::Null, |ms| json!(ms as f64 / 1000.0));
    json!({
        "available": true,
        "reason": null,
        "relay_version": at(snapshot, &["source", "relay_version"])
            .and_then(Value::as_str)
            .filter(|v| VERSION_RE.is_match(v)),
        "captured_at": timestamp(snapshot.get("captured_at")).map(|(s, _)| s),
        "elapsed_s": elapsed_s,
        "totals": totals_of(lifetime.and_then(|l| l.get("totals"))),
        "last_failure_at": timestamp(lifetime.and_then(|l| l.get("last_failure_at"))).map(|(s, _)| s),
        "components": components,
        "events": events.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "loss": loss,
        "retention": {
            "events_retained": retained,
            "oldest_seq": oldest,
            "latest_seq": latest,
            "truncated": truncated,
        },
        "memory": memory,
    })
}

/// One snapshot page from a CLI result, or why there is none.
pub fn parse_page(r: &CommandResult) -> Result<Value, Reason> {
    let parsed = if r.returncode == 0 && !r.timed_out {
        parse_output(&r.stdout)
    } else {
        None
    };
    let Some(page) = parsed else {
        return Err(failure_reason(r));
    };
    // some other JSON object (an error document, another command's answer) is not a snapshot
    if ["lifetime", "components", "events", "coverage"]
        .iter()
        .any(|k| page.get(*k).is_some())
    {
        Ok(page)
    } else {
        Err(Reason::Error)
    }
}

/// The `--memory` answer, when that call worked.
#[must_use]
pub fn parse_memory(r: Option<&CommandResult>) -> Option<Value> {
    r.filter(|m| m.returncode == 0 && !m.timed_out)
        .and_then(|m| parse_output(&m.stdout))
}

/// The answer from one snapshot call and the memory call (pure; single page).
#[must_use]
pub fn answer(snapshot: &CommandResult, memory: Option<&CommandResult>) -> Value {
    match parse_page(snapshot) {
        Ok(page) => map_diagnostics(&page, parse_memory(memory).as_ref()),
        Err(reason) => unavailable(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_saturate_and_refuse_wrong_types() {
        assert_eq!(count(Some(&json!("150"))), Some(150));
        assert_eq!(count(Some(&json!("0000000000000000000000012"))), Some(12));
        assert_eq!(count(Some(&json!("9007199254740993"))), Some(MAX_COUNT));
        assert_eq!(count(Some(&json!("9".repeat(5000)))), Some(MAX_COUNT));
        assert_eq!(
            count(Some(&json!({"value": "3", "saturated": true}))),
            Some(MAX_COUNT)
        );
        assert_eq!(
            count(Some(&json!({"value": "3", "saturated": false}))),
            Some(3)
        );
        assert_eq!(count(Some(&json!(7))), Some(7));
        for bad in [
            json!("-1"),
            json!("1e5"),
            json!(" 1"),
            json!(""),
            json!(1.5),
            json!(-3),
            json!(true),
            json!([1]),
        ] {
            assert_eq!(count(Some(&bad)), None, "{bad}");
        }
    }

    #[test]
    fn timestamps_need_the_rfc3339_shape() {
        assert!(timestamp(Some(&json!("2026-11-15T08:27:37.295079873Z"))).is_some());
        assert!(timestamp(Some(&json!("2026-11-15T08:27:37+01:00"))).is_some());
        for bad in [
            "2026-13-45T99:99:99Z",
            "yesterday",
            "2026-11-15 08:27:37Z",
            "2026-11-15T08:27:37.1234567890Z",
        ] {
            assert!(timestamp(Some(&json!(bad))).is_none(), "{bad}");
        }
        let a = timestamp(Some(&json!("2026-11-15T08:27:19.15Z"))).unwrap();
        let b = timestamp(Some(&json!("2026-11-15T08:27:19.153Z"))).unwrap();
        assert!(b.1 > a.1);
    }

    #[test]
    fn every_query_is_a_fixed_relay_expression() {
        assert!(queries_are_fixed());
        assert!(QUERIES.len() <= 6);
    }
}
