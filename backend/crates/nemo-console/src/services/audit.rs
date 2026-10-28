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
        let limit = limit.clamp(1, 500);
        self.store.with(|db| {
            let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<Value> {
                let params: String = r.get("params")?;
                Ok(json!({
                    "id": r.get::<_, i64>("id")?, "ts": r.get::<_, f64>("ts")?, "actor": r.get::<_, String>("actor")?,
                    "role": r.get::<_, String>("role")?, "method": r.get::<_, String>("method")?,
                    "path": r.get::<_, String>("path")?, "system": r.get::<_, Option<String>>("system")?,
                    "status": r.get::<_, i64>("status")?, "params": load_params(&params),
                }))
            };
            match system.filter(|s| !s.is_empty()) {
                Some(s) => {
                    let mut st = db.prepare("SELECT * FROM audit WHERE system=? ORDER BY id DESC LIMIT ?")?;
                    st.query_map(rusqlite::params![s, limit], map)?.collect()
                }
                None => {
                    let mut st = db.prepare("SELECT * FROM audit ORDER BY id DESC LIMIT ?")?;
                    st.query_map(rusqlite::params![limit], map)?.collect()
                }
            }
        })
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
