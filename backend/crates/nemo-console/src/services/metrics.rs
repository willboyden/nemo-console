//! Metrics poller: ring buffer (15 min) in memory + 1-minute rollups in SQLite for 1h/24h history.
//! Port of `services/metrics.py`. Storage contract: table `rollup(system, series, bucket, avg, max, n)`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;

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
