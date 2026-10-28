//! Failure-is-data wrappers around the adapters + a short-TTL status cache shared by overview/health/topology.
//! Port of `services/status.py`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;

use crate::adapters::base::{AdapterError, AdapterRef};
use crate::domain::models::{
    Health, MetricsSnapshot, Problem, Severity, SystemEvent, SystemId, SystemStatus, now_ts,
};
use crate::services::common::RecoverLock as _;
use crate::services::common::{redact, redact_obj};
use crate::services::pyfmt::char_prefix;

/// Defence in depth: adapters redact, the core scrubs again before anything leaves the process.
#[must_use]
pub fn clean_status(mut st: SystemStatus) -> SystemStatus {
    st.summary = redact(&st.summary);
    for p in &mut st.problems {
        p.message = redact(&p.message);
        p.fix = p.fix.as_deref().filter(|f| !f.is_empty()).map(redact);
    }
    st
}

#[must_use]
pub fn clean_event(mut ev: SystemEvent) -> SystemEvent {
    ev.summary = redact(&ev.summary);
    if let serde_json::Value::Object(m) = redact_obj(&serde_json::Value::Object(ev.data)) {
        ev.data = m;
    } else {
        ev.data = serde_json::Map::new();
    }
    ev
}

/// An async status enricher (the Switchyard capable-tier check).
pub type Enricher = Arc<
    dyn Fn(SystemStatus) -> Pin<Box<dyn Future<Output = Result<SystemStatus, AdapterError>> + Send>>
        + Send
        + Sync,
>;

pub struct StatusService {
    pub adapters: IndexMap<SystemId, AdapterRef>,
    pub ttl: f64,
    pub timeout: f64,
    pub global_problems: Vec<Problem>,
    pub enrichers: HashMap<SystemId, Enricher>,
    cache: Mutex<HashMap<SystemId, (Instant, SystemStatus)>>,
}

async fn with_timeout<T>(
    secs: f64,
    fut: impl Future<Output = Result<T, AdapterError>>,
) -> Result<T, AdapterError> {
    match tokio::time::timeout(Duration::from_secs_f64(secs.max(0.0)), fut).await {
        Ok(r) => r,
        Err(_) => Err(AdapterError::Timeout),
    }
}

impl StatusService {
    #[must_use]
    pub fn new(
        adapters: IndexMap<SystemId, AdapterRef>,
        ttl: f64,
        timeout: f64,
        global_problems: Vec<Problem>,
        enrichers: HashMap<SystemId, Enricher>,
    ) -> Self {
        StatusService {
            adapters,
            ttl,
            timeout,
            global_problems,
            enrichers,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn status(&self, system: SystemId, fresh: bool) -> SystemStatus {
        if !fresh
            && let Ok(c) = self.cache.lock_recovering()
            && let Some((t, st)) = c.get(&system)
            && t.elapsed().as_secs_f64() < self.ttl
        {
            return st.clone();
        }
        let st = match self.adapters.get(&system) {
            Some(a) => match with_timeout(self.timeout, a.status()).await {
                Ok(st) => {
                    let mut st = clean_status(st);
                    if let Some(enrich) = self.enrichers.get(&system) {
                        match enrich(st.clone()).await {
                            Ok(e) => st = e,
                            Err(_) => tracing::warn!("status enrichment for {system} failed"),
                        }
                    }
                    if !self.global_problems.is_empty() {
                        let mut p = self.global_problems.clone();
                        p.append(&mut st.problems);
                        st.problems = p;
                    }
                    st
                }
                Err(e) => {
                    tracing::warn!("adapter {system} status failed: {}", e.py_class());
                    failed_status(system, &e)
                }
            },
            None => failed_status(system, &AdapterError::UnknownId(system.to_string())),
        };
        if let Ok(mut c) = self.cache.lock_recovering() {
            c.insert(system, (Instant::now(), st.clone()));
        }
        st
    }

    pub fn invalidate(&self, system: SystemId) {
        if let Ok(mut c) = self.cache.lock_recovering() {
            c.remove(&system);
        }
    }

    /// Test seam (Python `status._cache.clear()`).
    pub fn clear(&self) {
        if let Ok(mut c) = self.cache.lock_recovering() {
            c.clear();
        }
    }

    pub async fn all(&self) -> Vec<SystemStatus> {
        futures::future::join_all(self.adapters.keys().map(|s| self.status(*s, false))).await
    }

    pub async fn metrics(&self, system: SystemId) -> MetricsSnapshot {
        let res = match self.adapters.get(&system) {
            Some(a) => with_timeout(self.timeout, a.metrics()).await,
            None => Err(AdapterError::UnknownId(system.to_string())),
        };
        res.unwrap_or_else(|e| MetricsSnapshot {
            system,
            ts: now_ts(),
            available: false,
            points: vec![],
            error: Some(char_prefix(&redact(&e.py_text()), 200).to_owned()),
        })
    }
}

/// An adapter bug must not become a 5xx: health unknown + `console.adapter.error`.
#[must_use]
pub fn failed_status(system: SystemId, e: &AdapterError) -> SystemStatus {
    SystemStatus {
        system,
        health: Health::Unknown,
        version: None,
        summary: "status check failed".to_owned(),
        endpoints: vec![],
        components: vec![],
        problems: vec![Problem::new(
            Severity::Error,
            "console.adapter.error",
            char_prefix(&redact(&e.py_text()), 200).to_owned(),
            Some(
                "check the console log; the adapter raised instead of returning health".to_owned(),
            ),
        )],
        checked_at: now_ts(),
    }
}
