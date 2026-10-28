//! Health, overview, topology and per-system observation (status, metrics, events). Port of `api/routers/observe.py`.

use std::sync::{Arc, LazyLock};

use axum::body::Body;
use axum::http::{HeaderValue, Response};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::base::AdapterError;
use crate::api::bodies::{V, query_int, query_str};
use crate::api::{
    Req, adapter_failure, dep_adapter, dep_viewer, json_response, ok_json, validation_error,
};
use crate::config::VERSION;
use crate::domain::models::{Health, now_ts};
use crate::services::errors::ApiError;
use crate::services::events::event_stream;
use crate::services::metrics::{WINDOWS, window_span};
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

/// Data Designer run records (addendum 9.7): records.jsonl only, values capped by the adapter.
pub async fn run_records(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
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
