//! Routing summary, audit, settings, the console's own /metrics and the OpenAPI document.
//! Port of `api/routers/admin.py` (+ FastAPI's openapi route).

use axum::body::Body;
use axum::http::{HeaderValue, Response};
use serde_json::Value;

use crate::api::bodies::{V, query_int, query_str, settings_body};
use crate::api::{Req, dep_admin, dep_viewer, json_response, validation_error};
use crate::domain::models::SystemId;
use crate::domain::models::now_ts;
use crate::services::audit::{
    ACTOR_RE, AuditFilter, SUMMARY_DEFAULT_WINDOW, SUMMARY_WINDOWS, list_filtered, summary,
};
use crate::services::auth::PrincipalRole;
use crate::services::errors::ApiError;
use crate::services::pyfmt::list_repr;
use crate::services::routing::{WINDOW_SECONDS, summarize, window_seconds};

/// The OpenAPI document generated from the Python app (`parity/gen/gen_openapi.py`), served as-is.
pub const OPENAPI_JSON: &str = include_str!("../../assets/openapi.json");

pub async fn routing(req: Req) -> Result<Response<Body>, ApiError> {
    dep_viewer(&req)?;
    let window = query_str(&req.query, "window").unwrap_or_else(|| "1h".to_owned());
    if window_seconds(&window).is_none() {
        let mut names: Vec<&str> = WINDOW_SECONDS.iter().map(|(k, _)| *k).collect();
        names.sort_unstable();
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("window must be one of {}", list_repr(&names)),
        ));
    }
    let Some(sy) = req.state.adapter(SystemId::Switchyard) else {
        return Err(ApiError::internal());
    };
    match sy.recent_events(5000).await {
        Ok(evs) => Ok(json_response(200, &summarize(&evs, &window, None))),
        Err(e) => Err(ApiError::new(
            502,
            "adapter_error",
            format!("could not read routing events: {}", e.py_class()),
        )),
    }
}

pub async fn audit(req: Req) -> Result<Response<Body>, ApiError> {
    dep_admin(&req)?;
    let mut v = V::new();
    let limit = query_int(&mut v, &req.query, "limit", 100, Some(1), Some(500));
    if !v.errors.is_empty() {
        return Err(validation_error(&v.errors));
    }
    let system = query_str(&req.query, "system");
    if let Some(s) = &system
        && SystemId::parse(s).is_none()
    {
        return Err(ApiError::new(422, "invalid_request", "unknown system"));
    }
    let filter = audit_filter(&req.query)?;
    let store = std::sync::Arc::clone(&req.state.audit.store);
    let limit = limit.unwrap_or(100);
    let rows = tokio::task::spawn_blocking(move || {
        list_filtered(&store, limit, system.as_deref(), &filter)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|_| ApiError::internal())?;
    Ok(json_response(200, &Value::Array(rows)))
}

/// Epoch seconds from 0 to 10^11 (a query value; empty = absent).
fn epoch_param(query: &[(String, String)], name: &str) -> Result<Option<f64>, ApiError> {
    let Some(raw) = query_str(query, name).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match raw.trim().parse::<f64>() {
        Ok(t) if t.is_finite() && (0.0..=1e11).contains(&t) && raw.len() <= 32 => Ok(Some(t)),
        _ => Err(ApiError::new(
            422,
            "invalid_request",
            format!("{name} must be epoch seconds"),
        )),
    }
}

/// The optional filters of `GET /audit` (addendum 9.16, D-43): `since`, `until` (epoch seconds), `actor` (a stored
/// actor), `status` (`ok` | `error`). An empty value means absent; anything else malformed is 422.
fn audit_filter(query: &[(String, String)]) -> Result<AuditFilter, ApiError> {
    let since = epoch_param(query, "since")?;
    let until = epoch_param(query, "until")?;
    if let (Some(a), Some(b)) = (since, until)
        && a > b
    {
        return Err(ApiError::new(
            422,
            "invalid_request",
            "since must not be after until",
        ));
    }
    let actor = query_str(query, "actor").filter(|s| !s.is_empty());
    if let Some(a) = &actor
        && !ACTOR_RE.is_match(a)
    {
        return Err(ApiError::new(
            422,
            "invalid_request",
            "actor must be a key alias such as key:<id> or loopback",
        ));
    }
    let ok = match query_str(query, "status").as_deref() {
        None | Some("") => None,
        Some("ok") => Some(true),
        Some("error") => Some(false),
        Some(_) => {
            return Err(ApiError::new(
                422,
                "invalid_request",
                "status must be one of 'error', 'ok'",
            ));
        }
    };
    Ok(AuditFilter {
        since,
        until,
        actor,
        ok,
    })
}

/// `GET /audit/summary?window=24h|7d|30d` (addendum 9.16, admin; Rust-only, PORTING.md D-42): counts from the audit
/// table per day and system, per actor and system, errors per bucket and the 401/403 total. No path, no params.
pub async fn audit_summary(req: Req) -> Result<Response<Body>, ApiError> {
    dep_admin(&req)?;
    let window =
        query_str(&req.query, "window").unwrap_or_else(|| SUMMARY_DEFAULT_WINDOW.to_owned());
    if !SUMMARY_WINDOWS.iter().any(|(w, _, _)| *w == window) {
        let names: Vec<&str> = SUMMARY_WINDOWS.iter().map(|(w, _, _)| *w).collect();
        return Err(ApiError::new(
            422,
            "invalid_request",
            format!("window must be one of {}", list_repr(&names)),
        ));
    }
    let store = std::sync::Arc::clone(&req.state.audit.store);
    let v = tokio::task::spawn_blocking(move || summary(&store, &window, now_ts()))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|_| ApiError::internal())?;
    Ok(json_response(200, &v))
}

pub async fn settings_get(req: Req) -> Result<Response<Body>, ApiError> {
    dep_viewer(&req)?;
    Ok(json_response(
        200,
        &req.state
            .settings
            .view(req.principal.role == PrincipalRole::Admin),
    ))
}

pub async fn settings_put(req: Req) -> Result<Response<Body>, ApiError> {
    dep_admin(&req)?;
    let mut v = V::new();
    let body = settings_body(&req.body, &mut v);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    let (view, interval) = req.state.settings.update(
        body.urls.as_ref(),
        body.poll_interval_s,
        body.playground_key_env.as_ref(),
    )?;
    if let Some(i) = interval {
        req.state.metrics.set_interval(i);
    }
    Ok(json_response(200, &view))
}

pub async fn console_metrics(req: Req) -> Result<Response<Body>, ApiError> {
    dep_viewer(&req)?;
    req.state.telemetry.set_sse(req.state.sse.active());
    let mut r = Response::new(Body::from(req.state.telemetry.render()));
    r.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    Ok(r)
}

pub async fn openapi(_req: Req) -> Result<Response<Body>, ApiError> {
    let mut r = Response::new(Body::from(OPENAPI_JSON));
    r.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    Ok(r)
}
