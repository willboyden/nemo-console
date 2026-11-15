//! Append-only audit of every mutating call. Params are redacted; chat text and file bodies are never stored.
//! Port of `services/audit.py`. Contract (PORTING.md section 5.3): `audit.params` holds Python `json.dumps` text
//! (so the 8000-character truncation decision is identical), rows are append-only (triggers in the schema).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::domain::models::now_ts;
use crate::services::common::RecoverLock as _;
use crate::services::common::{redact, redact_obj};
use crate::services::pyfmt::{char_len, char_prefix, json_dumps};
use crate::services::store::{Store, StoreError};

#[allow(clippy::expect_used)]
static SYSTEM_IN_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A/api/v1/systems/([a-z]+)/").expect("static regex"));
/// Body keys replaced by a size marker: key -> unit.
pub const CONTENT_KEYS: [(&str, &str); 4] = [
    ("content", "chars"),
    ("messages", "messages"),
    ("system", "chars"),
    ("rendered", "chars"),
];

/// Redact a JSON request body; replace bulk/private fields with a size marker.
#[must_use]
pub fn summarize_body(doc: &Value) -> Value {
    let Value::Object(m) = doc else {
        return Value::String("[non-object body]".to_owned());
    };
    let mut out = Map::new();
    for (k, v) in m {
        if let Some((_, unit)) = CONTENT_KEYS.iter().find(|(c, _)| c == k) {
            let n = match v {
                Value::String(s) => char_len(s),
                Value::Array(a) => a.len(),
                _ => 0,
            };
            out.insert(k.clone(), Value::String(format!("[{n} {unit}]")));
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    redact_obj(&Value::Object(out))
}

/// Valid JSON by construction: values are redacted per string; oversize becomes a marker object.
#[must_use]
pub fn params_blob(params: &Value) -> String {
    let safe = redact_obj(params);
    let blob = json_dumps(&safe);
    if blob.len() <= 8000 {
        return blob;
    }
    let keys: Vec<Value> = match &safe {
        Value::Object(m) => {
            let mut k: Vec<&String> = m.keys().collect();
            k.sort();
            k.into_iter()
                .take(20)
                .map(|s| Value::String(s.clone()))
                .collect()
        }
        _ => Vec::new(),
    };
    json_dumps(&json!({"_truncated": true, "bytes": blob.len(), "keys": keys}))
}

#[must_use]
pub fn load_params(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({"_unparseable": true}))
}

pub struct AuditLog {
    pub store: Arc<Store>,
}

impl AuditLog {
    pub fn record(
        &self,
        actor: &str,
        role: &str,
        method: &str,
        path: &str,
        status: u16,
        params: &Value,
    ) -> Result<(), StoreError> {
        let system = SYSTEM_IN_PATH
            .captures(path)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned());
        let blob = params_blob(params);
        self.store.execute(
            "INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
            rusqlite::params![now_ts(), actor, role, method, char_prefix(&redact(path), 300), system, status, blob],
        )?;
        Ok(())
    }

    pub fn list(&self, limit: i64, system: Option<&str>) -> Result<Vec<Value>, StoreError> {
        list_filtered(&self.store, limit, system, &AuditFilter::default())
    }
}

/// The optional filters of `GET /audit` (addendum 9.16, additive; PORTING.md D-43). `None` = no filter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditFilter {
    /// `ts >= since` (epoch seconds)
    pub since: Option<f64>,
    /// `ts <= until` (epoch seconds)
    pub until: Option<f64>,
    /// the actor exactly as stored
    pub actor: Option<String>,
    /// `Some(true)`: status < 400 (`ok`); `Some(false)`: status >= 400 (`error`)
    pub ok: Option<bool>,
}

/// An actor as the summary shows it and as the `actor` filter accepts it: a key's stored alias (`key:<id>`,
/// `loopback`).
#[allow(clippy::expect_used)]
pub static ACTOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9_.:@-]{0,63}\z").expect("static regex"));

/// The audit rows, newest first, with the optional filters. Without filters the query is the one the Python port
/// ran (so the answer is the same row for row).
pub fn list_filtered(
    store: &Store,
    limit: i64,
    system: Option<&str>,
    f: &AuditFilter,
) -> Result<Vec<Value>, StoreError> {
    let limit = limit.clamp(1, 500);
    store.with(|db| {
        let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<Value> {
            let params: String = r.get("params")?;
            Ok(json!({
                "id": r.get::<_, i64>("id")?, "ts": r.get::<_, f64>("ts")?, "actor": r.get::<_, String>("actor")?,
                "role": r.get::<_, String>("role")?, "method": r.get::<_, String>("method")?,
                "path": r.get::<_, String>("path")?, "system": r.get::<_, Option<String>>("system")?,
                "status": r.get::<_, i64>("status")?, "params": load_params(&params),
            }))
        };
        let mut wh: Vec<&str> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(s) = system.filter(|s| !s.is_empty()) {
            wh.push("system=?");
            args.push(s.to_owned().into());
        }
        if let Some(t) = f.since {
            wh.push("ts>=?");
            args.push(t.into());
        }
        if let Some(t) = f.until {
            wh.push("ts<=?");
            args.push(t.into());
        }
        if let Some(a) = &f.actor {
            wh.push("actor=?");
            args.push(a.clone().into());
        }
        match f.ok {
            Some(true) => wh.push("status<400"),
            Some(false) => wh.push("status>=400"),
            None => {}
        }
        args.push(limit.into());
        let sql = if wh.is_empty() {
            "SELECT * FROM audit ORDER BY id DESC LIMIT ?".to_owned()
        } else {
            format!(
                "SELECT * FROM audit WHERE {} ORDER BY id DESC LIMIT ?",
                wh.join(" AND ")
            )
        };
        let mut st = db.prepare(&sql)?;
        st.query_map(rusqlite::params_from_iter(args), map)?
            .collect()
    })
}

/// The summary windows: name, span (s), error bucket (s).
pub const SUMMARY_WINDOWS: [(&str, i64, i64); 3] = [
    ("24h", 86_400, 3_600),
    ("7d", 7 * 86_400, 6 * 3_600),
    ("30d", 30 * 86_400, 86_400),
];
/// The default summary window.
pub const SUMMARY_DEFAULT_WINDOW: &str = "7d";
/// At most this many (actor, system) rows (the busiest; `truncated` when more).
pub const SUMMARY_MAX_ACTOR_ROWS: usize = 200;

/// A stored system as the summary shows it: a known system id, null for a row without one, `other` for any other
/// stored value (the path's first segment after `/systems/`, which a request to an unknown system also records).
fn summary_system(s: Option<String>) -> Value {
    match s {
        None => Value::Null,
        Some(s) => match crate::domain::models::SystemId::parse(&s) {
            Some(id) => Value::String(id.to_string()),
            None => Value::String("other".to_owned()),
        },
    }
}

/// `YYYY-MM-DD` (UTC) of a day number since the epoch.
fn day_name(day: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(day.saturating_mul(86_400))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
}

/// `GET /audit/summary?window=` (addendum 9.16, admin; PORTING.md D-42): counts only, from the audit table; no path,
/// no params. `window` must be one of [`SUMMARY_WINDOWS`] (the handler checks it).
pub fn summary(store: &Store, window: &str, now: f64) -> Result<Value, StoreError> {
    let (span, bucket) = SUMMARY_WINDOWS
        .iter()
        .find(|(w, _, _)| *w == window)
        .map_or((7 * 86_400, 6 * 3_600), |(_, s, b)| (*s, *b));
    #[allow(clippy::cast_precision_loss)]
    let from = now - span as f64;
    // a read-only connection of its own (WAL readers do not wait for the writer), so the aggregation never holds the
    // store lock the audit writes and the poller go through; the store's connection only when it cannot be opened
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if let Ok(c) = rusqlite::Connection::open_with_flags(&store.path, flags)
        && c.busy_timeout(std::time::Duration::from_secs(5)).is_ok()
        && let Ok(v) = summary_on(&c, window, from, now, bucket)
    {
        return Ok(v);
    }
    store.with(|db| summary_on(db, window, from, now, bucket))
}

fn summary_on(
    db: &rusqlite::Connection,
    window: &str,
    from: f64,
    now: f64,
    bucket: i64,
) -> rusqlite::Result<Value> {
    {
        // actions per (UTC day, system)
        let mut by_day: BTreeMap<(i64, String), (Value, i64)> = BTreeMap::new();
        {
            let mut st = db.prepare(
                "SELECT CAST(ts / 86400 AS INTEGER) AS d, system, COUNT(*) FROM audit WHERE ts >= ? AND ts <= ? \
                 GROUP BY d, system",
            )?;
            let rows = st.query_map(rusqlite::params![from, now], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (Some(d), sys, n) = row? else { continue };
                let sys = summary_system(sys);
                let key = (d, sys.as_str().map_or_else(String::new, str::to_owned));
                by_day.entry(key).or_insert((sys, 0)).1 += n;
            }
        }
        // actions per (actor, system): the busiest groups first
        let mut by_actor: BTreeMap<(String, String), (Value, Value, i64)> = BTreeMap::new();
        let mut truncated = false;
        {
            let mut st = db.prepare(
                "SELECT actor, system, COUNT(*) AS n FROM audit WHERE ts >= ? AND ts <= ? \
                 GROUP BY actor, system ORDER BY n DESC, actor, system LIMIT ?",
            )?;
            let cap = i64::try_from(SUMMARY_MAX_ACTOR_ROWS * 4).unwrap_or(800);
            let rows = st.query_map(rusqlite::params![from, now, cap + 1], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut seen = 0i64;
            for row in rows {
                let (actor, sys, n) = row?;
                seen += 1;
                if seen > cap {
                    truncated = true;
                    break;
                }
                let actor = if ACTOR_RE.is_match(&actor) {
                    Value::String(actor)
                } else {
                    Value::Null
                };
                let sys = summary_system(sys);
                let key = (
                    actor.as_str().map_or_else(String::new, str::to_owned),
                    sys.as_str().map_or_else(String::new, str::to_owned),
                );
                by_actor.entry(key).or_insert((actor, sys, 0)).2 += n;
            }
        }
        let mut actors: Vec<(Value, Value, i64)> = by_actor.into_values().collect();
        actors.sort_by_key(|a| std::cmp::Reverse(a.2));
        if actors.len() > SUMMARY_MAX_ACTOR_ROWS {
            actors.truncate(SUMMARY_MAX_ACTOR_ROWS);
            truncated = true;
        }
        // errors (status >= 400) per bucket
        let mut errors = Vec::new();
        {
            let mut st = db.prepare(
                "SELECT CAST(ts / ? AS INTEGER) AS b, COUNT(*) FROM audit WHERE ts >= ? AND ts <= ? AND status >= 400 \
                 GROUP BY b ORDER BY b",
            )?;
            let rows = st.query_map(rusqlite::params![bucket, from, now], |r| {
                Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (Some(b), n) = row? else { continue };
                errors.push(json!({"ts_bucket": b.saturating_mul(bucket), "count": n}));
            }
        }
        let denied: i64 = db.query_row(
            "SELECT COUNT(*) FROM audit WHERE ts >= ? AND ts <= ? AND status IN (401, 403)",
            rusqlite::params![from, now],
            |r| r.get(0),
        )?;
        Ok(json!({
            "window": window,
            "from": from,
            "to": now,
            "bucket_s": bucket,
            "by_day": by_day
                .into_iter()
                .map(|((d, _), (sys, n))| json!({"day": day_name(d), "system": sys, "count": n}))
                .collect::<Vec<_>>(),
            "by_actor": actors
                .into_iter()
                .map(|(a, sys, n)| json!({"actor": a, "system": sys, "count": n}))
                .collect::<Vec<_>>(),
            "errors": errors,
            "denied_total": denied,
            "truncated": truncated,
        }))
    }
}

/// Unauthenticated/denied requests are attacker-controlled volume: never rows in the audit table. They are counted
/// (bounded label set), kept in a small ring, and logged as ONE aggregated line per interval.
pub struct DenialLog {
    inner: Mutex<DenialInner>,
}

struct DenialInner {
    interval_s: f64,
    ring: VecDeque<(f64, u16, String)>,
    ring_cap: usize,
    pending: BTreeMap<u16, u64>,
    last_log: Instant,
    total: BTreeMap<u16, u64>,
}

pub const DENIAL_STATUSES: [u16; 4] = [401, 403, 421, 429];

impl Default for DenialLog {
    fn default() -> Self {
        DenialLog::new(60.0, 100)
    }
}

impl DenialLog {
    #[must_use]
    pub fn new(interval_s: f64, ring: usize) -> Self {
        DenialLog {
            inner: Mutex::new(DenialInner {
                interval_s,
                ring: VecDeque::new(),
                ring_cap: ring,
                pending: BTreeMap::new(),
                last_log: Instant::now(),
                total: BTreeMap::new(),
            }),
        }
    }

    pub fn note(&self, status: u16, method: &str) {
        let status = if DENIAL_STATUSES.contains(&status) {
            status
        } else {
            403
        };
        let Ok(mut d) = self.inner.lock_recovering() else {
            return;
        };
        *d.total.entry(status).or_insert(0) += 1;
        *d.pending.entry(status).or_insert(0) += 1;
        if d.ring.len() == d.ring_cap {
            d.ring.pop_front();
        }
        d.ring
            .push_back((now_ts(), status, char_prefix(method, 7).to_owned()));
        let elapsed = d.last_log.elapsed().as_secs_f64();
        if elapsed >= d.interval_s && !d.pending.is_empty() {
            let parts: Vec<String> = d.pending.iter().map(|(k, v)| format!("{k}={v}")).collect();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let secs = elapsed as u64;
            tracing::warn!("denied requests in the last {secs}s: {}", parts.join(", "));
            d.pending.clear();
            d.last_log = Instant::now();
        }
    }

    /// Total denials per status (tests read `denials.total[401]`).
    #[must_use]
    pub fn total(&self, status: u16) -> u64 {
        self.inner
            .lock_recovering()
            .map(|d| d.total.get(&status).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    #[must_use]
    pub fn ring_len(&self) -> usize {
        self.inner
            .lock_recovering()
            .map(|d| d.ring.len())
            .unwrap_or(0)
    }
}
