//! Routing summary, audit, settings, the console's own /metrics and the OpenAPI document.
//! Port of `api/routers/admin.py` (+ FastAPI's openapi route).

use axum::body::Body;
use axum::http::{HeaderValue, Response};
use serde_json::Value;

use crate::api::bodies::{V, query_int, query_str, settings_body};
use crate::api::{Req, dep_admin, dep_viewer, json_response, validation_error};
use crate::domain::models::SystemId;
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
    let rows = req
        .state
        .audit
        .list(limit.unwrap_or(100), system.as_deref())
        .map_err(|_| ApiError::internal())?;
    Ok(json_response(200, &Value::Array(rows)))
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
