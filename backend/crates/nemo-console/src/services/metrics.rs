//! Metrics poller: ring buffer (15 min) in memory + 1-minute rollups in SQLite for 1h/24h history.
//! Port of `services/metrics.py`. Storage contract: table `rollup(system, series, bucket, avg, max, n)`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::domain::models::{MetricPoint, SystemId, now_ts};
use crate::services::common::RecoverLock as _;
use crate::services::status::StatusService;
use crate::services::store::Store;

pub const WINDOWS: [(&str, f64); 3] = [("15m", 900.0), ("1h", 3600.0), ("24h", 86400.0)];
pub const MAX_SERIES_PER_SYSTEM: usize = 256;
pub const ROLLUP_KEEP_S: f64 = 8.0 * 86400.0;

#[must_use]
pub fn window_span(w: &str) -> Option<f64> {
    WINDOWS.iter().find(|(k, _)| *k == w).map(|(_, v)| *v)
}

/// Windows of the series route (addendum 9.13): name, span in seconds, bucket in seconds (0: the ring's raw
/// points). `WINDOWS` above stays the history route's (it has no `7d`).
pub const SERIES_WINDOWS: [(&str, f64, i64); 4] = [
    ("15m", 900.0, 0),
    ("1h", 3600.0, 60),
    ("24h", 86400.0, 60),
    ("7d", 7.0 * 86400.0, 600),
];
/// At most this many keys per series request.
pub const SERIES_MAX_KEYS: usize = 12;
/// With `split=1`, at most this many label variants per key (the rest dropped, `truncated: true`).
pub const SERIES_PER_KEY: usize = 32;

/// `(span_s, bucket_s)` of a series window; `None` for an unknown one.
#[must_use]
pub fn series_window(w: &str) -> Option<(f64, i64)> {
    SERIES_WINDOWS
        .iter()
        .find(|(k, _, _)| *k == w)
        .map(|(_, span, bucket)| (*span, *bucket))
}

/// The labels of a stored series name as an object: `sy.tokens{kind=prompt,model=x}` -> `{"kind": "prompt",
/// "model": "x"}`; `{}` for a name without labels. Inverse of [`series_name`], which does not escape: a part
/// without `=` belongs to the previous value (a value with a comma), and a value wrapped in double quotes
/// (`model="x"`) is unwrapped.
#[must_use]
pub fn series_labels(name: &str) -> Map<String, Value> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let Some(open) = name.find('{') else {
        return Map::new();
    };
    let rest = &name[open + 1..];
    let inner = rest.strip_suffix('}').unwrap_or(rest);
    if !inner.is_empty() {
        for part in inner.split(',') {
            match part.split_once('=') {
                Some((k, v)) if !k.is_empty() => pairs.push((k.to_owned(), v.to_owned())),
                _ => {
                    if let Some(last) = pairs.last_mut() {
                        last.1.push(',');
                        last.1.push_str(part);
                    }
                }
            }
        }
    }
    let mut out = Map::new();
    for (k, v) in pairs {
        let v = match v.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
            Some(inner) => inner.to_owned(),
            None => v,
        };
        out.insert(k, Value::String(v));
    }
    out
}

/// One line of a series answer: `points` are `[ts, avg, max]`, bucket starts in epoch seconds, in time order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Series {
    pub key: String,
    /// `null` with `split=0` (and for a key without data); the parsed labels with `split=1`.
    pub label: Option<Map<String, Value>>,
    pub points: Vec<[f64; 3]>,
}

/// The answer of `GET /systems/{id}/metrics/series` (addendum 9.13).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesAnswer {
    pub window: String,
    pub step_s: f64,
    pub from: f64,
    pub to: f64,
    pub series: Vec<Series>,
    pub truncated: bool,
}

/// Raw rows of one requested key: (stored series name, points), names in byte order.
type RawSeries = Vec<(String, Vec<[f64; 3]>)>;

/// Bounds for a range scan of a bare key's label variants in the `(system, series, bucket)` primary key:
/// `key{...}` sorts at or after `key{` and before `key|` (`|` follows `{`). A labelled key matches itself only.
fn variant_range(key: &str) -> (String, String) {
    if key.contains('{') {
        (String::new(), String::new())
    } else {
        (format!("{key}{{"), format!("{key}|"))
    }
}

/// `key` or `key{a=1,b=2}` (labels sorted by name).
#[must_use]
pub fn series_name(p: &MetricPoint) -> String {
    if p.labels.is_empty() {
        return p.key.clone();
    }
    let mut labels: Vec<(&String, &String)> = p.labels.iter().collect();
    labels.sort();
    let inner: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{}{{{}}}", p.key, inner.join(","))
}

/// `split=0`: the label variants summed per bucket (avg and max each summed), in time order.
fn sum_variants(raw: RawSeries) -> Vec<[f64; 3]> {
    let mut all: Vec<[f64; 3]> = raw.into_iter().flat_map(|(_, p)| p).collect();
    all.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut out: Vec<[f64; 3]> = Vec::with_capacity(all.len());
    for p in all {
        match out.last_mut() {
            Some(last) if last[0].total_cmp(&p[0]).is_eq() => {
                last[1] += p[1];
                last[2] += p[2];
            }
            _ => out.push(p),
        }
    }
    out
}

struct Ring {
    maxlen: usize,
    points: VecDeque<(f64, f64)>,
}

#[derive(Default)]
struct Inner {
    /// Insertion-ordered like the Python dict (history sums in this order).
    ring: IndexMap<(SystemId, String), Ring>,
    /// bucket, sum, max, n
    pending: HashMap<(SystemId, String), [f64; 4]>,
    series_count: HashMap<SystemId, usize>,
    last_prune: f64,
}

pub struct MetricsPoller {
    pub status: Arc<StatusService>,
    pub store: Arc<Store>,
    interval: Mutex<f64>,
    pub ring_seconds: i64,
    inner: Mutex<Inner>,
}

impl MetricsPoller {
    #[must_use]
    pub fn new(
        status: Arc<StatusService>,
        store: Arc<Store>,
        interval: f64,
        ring_seconds: i64,
    ) -> Self {
        MetricsPoller {
            status,
            store,
            interval: Mutex::new(interval),
            ring_seconds,
            inner: Mutex::new(Inner::default()),
        }
    }

    #[must_use]
    pub fn interval(&self) -> f64 {
        self.interval.lock_recovering().map(|i| *i).unwrap_or(2.0)
    }

    pub fn set_interval(&self, v: f64) {
        if let Ok(mut i) = self.interval.lock_recovering() {
            *i = v;
        }
    }

    #[must_use]
    pub fn maxlen(&self) -> usize {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let n = (self.ring_seconds as f64 / self.interval().max(0.1)) as usize + 1;
        n.max(2)
    }

    /// One tick inside [`crate::services::common::contained`]: a panic in an adapter's `metrics()` is logged and the
    /// poller goes on with the next tick (Python: each tick inside `try/except Exception`; audit finding 3).
    pub async fn tick_contained(&self) {
        crate::services::common::contained("metrics poll", self.tick()).await;
    }

    pub async fn tick(&self) {
        for system in self.status.adapters.keys().copied().collect::<Vec<_>>() {
            let snap = self.status.metrics(system).await;
            let points = if snap.available {
                snap.points
            } else {
                Vec::new()
            };
            self.record(system, snap.ts, &points);
        }
    }

    pub fn record(&self, system: SystemId, ts: f64, points: &[MetricPoint]) {
        let maxlen = self.maxlen();
        let Ok(mut g) = self.inner.lock_recovering() else {
            return;
        };
        #[allow(clippy::cast_possible_truncation)]
        let bucket = (ts / 60.0).floor() as i64;
        for p in points {
            let k = (system, series_name(p));
            if !g.ring.contains_key(&k) {
                let count = g.series_count.get(&system).copied().unwrap_or(0);
                if count >= MAX_SERIES_PER_SYSTEM {
                    continue;
                }
                g.series_count.insert(system, count + 1);
                g.ring.insert(
                    k.clone(),
                    Ring {
                        maxlen,
                        points: VecDeque::new(),
                    },
                );
            }
            if let Some(r) = g.ring.get_mut(&k) {
                if r.points.len() == r.maxlen {
                    r.points.pop_front();
                }
                r.points.push_back((ts, p.value));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let flush = match g.pending.get(&k) {
                Some(cur) if cur[0] as i64 != bucket => Some(*cur),
                _ => None,
            };
            if let Some(cur) = flush {
                self.flush_one(&k, cur);
                g.pending.remove(&k);
            }
            #[allow(clippy::cast_precision_loss)]
            match g.pending.get_mut(&k) {
                None => {
                    g.pending.insert(k, [bucket as f64, p.value, p.value, 1.0]);
                }
                Some(cur) => {
                    cur[1] += p.value;
                    cur[2] = cur[2].max(p.value);
                    cur[3] += 1.0;
                }
            }
        }
        if ts - g.last_prune > 3600.0 {
            g.last_prune = ts;
            #[allow(clippy::cast_possible_truncation)]
            let cutoff = ((ts - ROLLUP_KEEP_S) / 60.0).floor() as i64;
            let _ = self
                .store
                .execute("DELETE FROM rollup WHERE bucket < ?", [cutoff]);
        }
    }

    fn flush_one(&self, k: &(SystemId, String), cur: [f64; 4]) {
        #[allow(clippy::cast_possible_truncation)]
        let (bucket, n) = (cur[0] as i64, cur[3] as i64);
        let _ = self.store.execute(
            "INSERT INTO rollup(system, series, bucket, avg, max, n) VALUES (?,?,?,?,?,?) \
             ON CONFLICT(system, series, bucket) DO UPDATE SET avg=excluded.avg, max=excluded.max, n=excluded.n",
            rusqlite::params![k.0.as_str(), k.1, bucket, cur[1] / cur[3], cur[2], n],
        );
    }

    /// Write every pending rollup (shutdown, and before a 1h/24h history read).
    pub fn flush(&self) {
        let Ok(g) = self.inner.lock_recovering() else {
            return;
        };
        for (k, cur) in &g.pending {
            self.flush_one(k, *cur);
        }
    }

    fn matches(series: &str, key: &str) -> bool {
        if key.contains('{') {
            return series == key;
        }
        series == key || series.starts_with(&format!("{key}{{"))
    }

    /// `[[ts, value], ...]`, label variants of a bare key summed.
    #[must_use]
    pub fn history(
        &self,
        system: SystemId,
        window: &str,
        key: &str,
        now: Option<f64>,
    ) -> Vec<[f64; 2]> {
        let now = now.unwrap_or_else(now_ts);
        let span = window_span(window).unwrap_or(900.0);
        let mut acc: Vec<(f64, f64)> = Vec::new();
        let mut add = |ts: f64, v: f64| match acc.iter_mut().find(|(t, _)| *t == ts) {
            Some(slot) => slot.1 += v,
            None => acc.push((ts, 0.0 + v)),
        };
        if window == "15m" {
            if let Ok(g) = self.inner.lock_recovering() {
                for ((sys, name), r) in &g.ring {
                    if *sys == system && Self::matches(name, key) {
                        for (ts, v) in &r.points {
                            if *ts >= now - span {
                                add(*ts, *v);
                            }
                        }
                    }
                }
            }
        } else {
            self.flush();
            #[allow(clippy::cast_possible_truncation)]
            let since = ((now - span) / 60.0).floor() as i64;
            let rows: Vec<(String, i64, f64)> = self
                .store
                .with(|db| {
                    let mut s = db.prepare(
                        "SELECT series, bucket, avg FROM rollup WHERE system=? AND bucket>=? ORDER BY bucket",
                    )?;
                    s.query_map(rusqlite::params![system.as_str(), since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect()
                })
                .unwrap_or_default();
            for (series, bucket, avg) in rows {
                if Self::matches(&series, key) {
                    #[allow(clippy::cast_precision_loss)]
                    add((bucket * 60) as f64, avg);
                }
            }
        }
        acc.sort_by(|a, b| a.0.total_cmp(&b.0));
        acc.into_iter().map(|(t, v)| [t, v]).collect()
    }

    /// `GET /systems/{id}/metrics/series` (addendum 9.13). `window` must be one of [`SERIES_WINDOWS`] (the route
    /// validates it; an unknown one reads as `1h`), every key is a validated canonical key.
    #[must_use]
    pub fn series(
        &self,
        system: SystemId,
        window: &str,
        keys: &[String],
        split: bool,
        now: Option<f64>,
    ) -> SeriesAnswer {
        let now = now.unwrap_or_else(now_ts);
        let (window, (span, bucket)) = match series_window(window) {
            Some(w) => (window, w),
            None => ("1h", (3600.0, 60)),
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let (step_s, from, since) = match bucket {
            0 => (self.interval(), now - span, 0),
            60 => {
                let since = ((now - span) / 60.0).floor() as i64;
                (60.0, (since * 60) as f64, since)
            }
            // 10-minute buckets start on a multiple of ten minutes, so the first one is never partial
            _ => {
                let since = ((now - span) / 600.0).floor() as i64 * 10;
                (600.0, (since * 60) as f64, since)
            }
        };
        if bucket > 0 {
            self.flush();
        }
        let mut series = Vec::new();
        let mut truncated = false;
        for key in keys {
            let mut raw = match bucket {
                0 => self.ring_series(system, key, now - span),
                60 => self.rollup_series(system, key, since),
                _ => self.rollup_series_10m(system, key, since),
            };
            raw.sort_by(|a, b| a.0.cmp(&b.0));
            if !split {
                series.push(Series {
                    key: key.clone(),
                    label: None,
                    points: sum_variants(raw),
                });
            } else if raw.is_empty() {
                series.push(Series {
                    key: key.clone(),
                    label: None,
                    points: Vec::new(),
                });
            } else {
                if raw.len() > SERIES_PER_KEY {
                    truncated = true;
                    raw.truncate(SERIES_PER_KEY);
                }
                for (name, points) in raw {
                    series.push(Series {
                        key: key.clone(),
                        label: Some(series_labels(&name)),
                        points,
                    });
                }
            }
        }
        SeriesAnswer {
            window: window.to_owned(),
            step_s,
            from,
            to: now,
            series,
            truncated,
        }
    }

    /// The ring's raw points at or after `cutoff` (`max` = the value itself); series without one are left out.
    fn ring_series(&self, system: SystemId, key: &str, cutoff: f64) -> RawSeries {
        let Ok(g) = self.inner.lock_recovering() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ((sys, name), r) in &g.ring {
            if *sys != system || !Self::matches(name, key) {
                continue;
            }
            let mut points: Vec<[f64; 3]> = r
                .points
                .iter()
                .filter(|(ts, v)| *ts >= cutoff && v.is_finite())
                .map(|(ts, v)| [*ts, *v, *v])
                .collect();
            points.sort_by(|a, b| a[0].total_cmp(&b[0]));
            if !points.is_empty() {
                out.push((name.clone(), points));
            }
        }
        out
    }

    /// The 1-minute rollups from bucket `since` on.
    fn rollup_series(&self, system: SystemId, key: &str, since: i64) -> RawSeries {
        self.rollup_query(
            "SELECT series, bucket * 60, avg, max FROM rollup WHERE system=?1 AND bucket>=?2 \
             AND (series=?3 OR (series>=?4 AND series<?5)) ORDER BY series, bucket",
            system,
            key,
            since,
        )
    }

    /// The 1-minute rollups regrouped into 10-minute buckets in SQL: avg weighted by each minute's sample count
    /// `n`, max = max of maxes.
    fn rollup_series_10m(&self, system: SystemId, key: &str, since: i64) -> RawSeries {
        self.rollup_query(
            "SELECT series, (bucket / 10) * 600 AS b, \
             CASE WHEN SUM(n) > 0 THEN SUM(avg * n) / SUM(n) ELSE AVG(avg) END, MAX(max) \
             FROM rollup WHERE system=?1 AND bucket>=?2 AND (series=?3 OR (series>=?4 AND series<?5)) \
             GROUP BY series, bucket / 10 ORDER BY series, b",
            system,
            key,
            since,
        )
    }

    /// Run one rollup query (`series, ts, avg, max` rows ordered by series then time) and group it by series.
    fn rollup_query(&self, sql: &str, system: SystemId, key: &str, since: i64) -> RawSeries {
        let (lo, hi) = variant_range(key);
        let rows: Vec<(String, i64, f64, f64)> = self
            .store
            .with(|db| {
                let mut s = db.prepare(sql)?;
                s.query_map(
                    rusqlite::params![system.as_str(), since, key, lo, hi],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?
                .collect()
            })
            .unwrap_or_default();
        let mut out: RawSeries = Vec::new();
        for (name, ts, avg, max) in rows {
            if !avg.is_finite() || !max.is_finite() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let p = [ts as f64, avg, max];
            match out.last_mut() {
                Some((last, pts)) if *last == name => pts.push(p),
                _ => out.push((name, vec![p])),
            }
        }
        out
    }

    /// Ring length of one series (tests).
    #[must_use]
    pub fn ring_len(&self, system: SystemId, series: &str) -> Option<usize> {
        self.inner.lock_recovering().ok().and_then(|g| {
            g.ring
                .get(&(system, series.to_owned()))
                .map(|r| r.points.len())
        })
    }

    /// Number of series held for a system (tests).
    #[must_use]
    pub fn series_for(&self, system: SystemId) -> usize {
        self.inner
            .lock_recovering()
            .map(|g| g.ring.keys().filter(|(s, _)| *s == system).count())
            .unwrap_or(0)
    }

    /// The background loop (`run()`): tick, then sleep the CURRENT interval (PUT /settings changes it live).
    pub async fn run(self: Arc<Self>) {
        loop {
            self.tick_contained().await;
            tokio::time::sleep(Duration::from_secs_f64(self.interval().max(0.0))).await;
        }
    }
}
