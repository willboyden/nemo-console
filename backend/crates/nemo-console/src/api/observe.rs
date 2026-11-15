//! Health, overview, topology and per-system observation (status, metrics, events). Port of `api/routers/observe.py`.

use std::sync::{Arc, LazyLock};

use axum::body::Body;
use axum::http::{HeaderValue, Response};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::base::AdapterError;
use crate::adapters::dcgm::{FLEET_DEFAULT_WINDOW, FLEET_METRICS, FLEET_WINDOWS};
use crate::api::bodies::{V, query_int, query_str};
use crate::api::{
    Req, adapter_failure, dep_adapter, dep_admin, dep_viewer, json_response, ok_json,
    validation_error,
};
use crate::config::VERSION;
use crate::domain::models::{Health, SystemId, now_ts};
use crate::services::errors::ApiError;
use crate::services::events::event_stream;
use crate::services::metrics::{
    SERIES_MAX_KEYS, SERIES_WINDOWS, WINDOWS, series_window, window_span,
};
use crate::services::pyfmt::list_repr;
use crate::services::status::clean_event;

/// `metrics_keys.KEY_RE`.
#[allow(clippy::expect_used)]
pub static KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-z][a-z0-9_.]{0,63}(?:\{[A-Za-z0-9_.:/\-=,]{1,200}\})?\z")
        .expect("static regex")
});

pub async fn health(req: Req) -> Result<Response<Body>, ApiError> {
    let statuses = req.state.status.all().await;
    let mut systems = Map::new();
    for s in &statuses {
        systems.insert(s.system.to_string(), json!(s.health));
    }
    let ok = statuses
        .iter()
        .all(|s| matches!(s.health, Health::Up | Health::Off));
    let trust = if req.principal.name == "loopback" {
        "loopback-admin"
    } else {
        "bearer"
    };
    Ok(json_response(
        200,
        &json!({
            "status": if ok { "ok" } else { "degraded" }, "version": VERSION, "trust": trust,
            "role": req.principal.role.as_str(), "trust_notice": req.state.trust_notice, "systems": systems,
        }),
    ))
}

pub async fn overview(req: Req) -> Result<Response<Body>, ApiError> {
    dep_viewer(&req)?;
    let statuses = req.state.status.all().await;
    let topo = req.state.topology.get().await;
    Ok(json_response(
        200,
        &json!({"systems": statuses, "topology": topo, "ts": now_ts()}),
    ))
}

pub async fn topology(req: Req) -> Result<Response<Body>, ApiError> {
    dep_viewer(&req)?;
    ok_json(&req.state.topology.get().await)
}

pub async fn systems(req: Req) -> Result<Response<Body>, ApiError> {
    dep_viewer(&req)?;
    let statuses = req.state.status.all().await;
    let rows: Vec<Value> = statuses
        .iter()
        .map(|s| {
            let label = req
                .state
                .adapter(s.system)
                .map(|a| a.label())
                .unwrap_or_default();
            json!({"id": s.system, "label": label, "health": s.health})
        })
        .collect();
    Ok(json_response(200, &Value::Array(rows)))
}

pub async fn system_status(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    ok_json(&req.state.status.status(a.id(), true).await)
}

pub async fn system_metrics(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    ok_json(&req.state.status.metrics(a.id()).await)
}

pub async fn metrics_history(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    let window = query_str(&req.query, "window").unwrap_or_else(|| "15m".to_owned());
    let key = query_str(&req.query, "key").unwrap_or_default();
    if window_span(&window).is_none() {
        let mut names: Vec<&str> = WINDOWS.iter().map(|(k, _)| *k).collect();
        names.sort_unstable();
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("window must be one of {}", list_repr(&names)),
        ));
    }
    if !KEY_RE.is_match(&key) {
        return Err(ApiError::new(
            422,
            "invalid_request",
            "key must be a canonical metric key, e.g. sy.requests_total",
        ));
    }
    let points = req.state.metrics.history(a.id(), &window, &key, None);
    Ok(json_response(200, &json!({"key": key, "points": points})))
}

/// The longest `keys` parameter of the series route: 12 keys of the longest form `KEY_RE` accepts (64 characters of
/// name, 200 of labels and the two braces) and the 11 commas between them. Longer is refused before it is split.
pub const SERIES_KEYS_MAX_LEN: usize = SERIES_MAX_KEYS * (64 + 2 + 200) + (SERIES_MAX_KEYS - 1);

/// Split the `keys` parameter of the series route on the commas outside braces (a labelled key such as
/// `sy.tokens{kind=prompt,model=x}` keeps its own commas).
fn split_keys(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    for c in raw.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

/// `GET /systems/{id}/metrics/series` (addendum 9.13, Rust-only, PORTING.md D-36): several keys over 15m (the
/// ring) / 1h / 24h (1-minute rollups) / 7d (10-minute buckets), `[ts, avg, max]` points, optionally one line per
/// label variant.
pub async fn metrics_series(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    let window = query_str(&req.query, "window").unwrap_or_else(|| "1h".to_owned());
    if series_window(&window).is_none() {
        let mut names: Vec<&str> = SERIES_WINDOWS.iter().map(|(k, _, _)| *k).collect();
        names.sort_unstable();
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("window must be one of {}", list_repr(&names)),
        ));
    }
    let raw = query_str(&req.query, "keys").unwrap_or_default();
    if raw.len() > SERIES_KEYS_MAX_LEN {
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("keys is longer than {SERIES_KEYS_MAX_LEN} characters"),
        ));
    }
    let keys = split_keys(&raw);
    if keys.len() > SERIES_MAX_KEYS {
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("at most {SERIES_MAX_KEYS} keys"),
        ));
    }
    if !keys.iter().all(|k| KEY_RE.is_match(k)) {
        return Err(ApiError::new(
            422,
            "invalid_request",
            "keys must be a comma-separated list of canonical metric keys, e.g. sy.requests_total",
        ));
    }
    let split = match query_str(&req.query, "split").as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => {
            return Err(ApiError::new(
                422,
                "invalid_request",
                "split must be 0 or 1",
            ));
        }
    };
    // SQLite reads: on the blocking pool, never on a runtime worker
    let metrics = Arc::clone(&req.state.metrics);
    let system = a.id();
    let answer =
        tokio::task::spawn_blocking(move || metrics.series(system, &window, &keys, split, None))
            .await
            .map_err(|_| ApiError::internal())?;
    ok_json(&answer)
}

/// `GET /systems/dcgm/fleet?metric=util|temp|power|fb_used&window=1h|24h|7d` (addendum 9.14, Rust-only, PORTING.md
/// D-38): per-GPU history of every DCGM host Prometheus scrapes. Every other system answers 404 (after the role
/// check, before the query is looked at); an unknown metric or window is 422 `invalid_request`. The adapter builds
/// the PromQL from its fixed table; nothing from the request reaches the expression but the table key.
pub async fn dcgm_fleet(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    if a.id() != SystemId::Dcgm {
        return Err(ApiError::new(
            404,
            "not_found",
            "no such endpoint for this system",
        ));
    }
    let metric = query_str(&req.query, "metric").unwrap_or_default();
    if !FLEET_METRICS.iter().any(|(m, _, _, _)| *m == metric) {
        let mut names: Vec<&str> = FLEET_METRICS.iter().map(|(m, _, _, _)| *m).collect();
        names.sort_unstable();
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("metric must be one of {}", list_repr(&names)),
        ));
    }
    let window = query_str(&req.query, "window").unwrap_or_else(|| FLEET_DEFAULT_WINDOW.to_owned());
    if !FLEET_WINDOWS.iter().any(|(w, _, _)| *w == window) {
        let names: Vec<&str> = FLEET_WINDOWS.iter().map(|(w, _, _)| *w).collect();
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("window must be one of {}", list_repr(&names)),
        ));
    }
    match a.dcgm_fleet(&metric, &window).await {
        Ok(v) => Ok(json_response(200, &v)),
        Err(AdapterError::NoSuchEndpoint) => Err(ApiError::new(
            404,
            "not_found",
            "no such endpoint for this system",
        )),
        Err(AdapterError::Invalid(m)) => Err(ApiError::new(422, "invalid_request", m)),
        Err(e) => Err(adapter_failure(&e)),
    }
}

pub async fn events(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    let mut v = V::new();
    let limit = query_int(&mut v, &req.query, "limit", 100, Some(1), Some(500));
    let Some(limit) = limit.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let n = limit as usize;
    match a.recent_events(n).await {
        Ok(evs) => {
            let out: Vec<_> = evs.into_iter().take(n).map(clean_event).collect();
            ok_json(&out)
        }
        Err(e) => Err(ApiError::new(
            502,
            "adapter_error",
            format!("could not read events: {}", e.py_class()),
        )),
    }
}

pub async fn events_stream(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    let st = &req.state;
    if st.sse.active() >= st.sse.cap {
        return Err(ApiError::new(
            429,
            "too_many_streams",
            format!(
                "at most {} concurrent event streams",
                st.cfg.sse_max_connections
            ),
        ));
    }
    let stream = event_stream(
        Arc::clone(&a),
        Arc::clone(&st.sse),
        st.cfg.sse_idle_timeout_s,
        st.cfg.sse_heartbeat_s,
        st.shutdown.clone(),
    );
    let body = Body::from_stream(futures::StreamExt::map(stream, Ok::<_, std::io::Error>));
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    resp.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(resp)
}

fn run_query(req: &Req, default_limit: i64) -> Result<(i64, i64), ApiError> {
    let mut v = V::new();
    let limit = query_int(
        &mut v,
        &req.query,
        "limit",
        default_limit,
        Some(1),
        Some(200),
    );
    let offset = query_int(&mut v, &req.query, "offset", 0, Some(0), None);
    if !v.errors.is_empty() {
        return Err(validation_error(&v.errors));
    }
    Ok((limit.unwrap_or(default_limit), offset.unwrap_or(0)))
}

fn run_error(e: &AdapterError) -> ApiError {
    match e {
        AdapterError::NoSuchEndpoint => {
            ApiError::new(404, "not_found", "no such endpoint for this system")
        }
        AdapterError::UnknownId(_) => ApiError::new(404, "not_found", "unknown run"),
        other => adapter_failure(other),
    }
}

/// Data Designer run records (addendum 9.7): records.jsonl only, values capped by the adapter. Admin-only since
/// addendum 9.13 (model-written content, like Relay span content; PORTING.md D-37: the Python reference served it
/// to viewers).
pub async fn run_records(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_admin(&req)?;
    let (limit, offset) = run_query(&req, 50)?;
    match a.records_page(req.param("run_id"), limit, offset).await {
        Ok(v) => Ok(json_response(200, &v)),
        Err(e) => Err(run_error(&e)),
    }
}

/// Paths + sizes of a Data Designer run's artifacts.
pub async fn run_files(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    match a.run_files(req.param("run_id")).await {
        Ok(v) => Ok(json_response(200, &v)),
        Err(e) => Err(run_error(&e)),
    }
}
