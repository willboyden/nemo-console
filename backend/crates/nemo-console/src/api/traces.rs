//! The Relay trace explorer routes (addendum 9.12): eleven read-only GETs under `/systems/{id}`.
//!
//! This layer only validates: path ids (UUID shape for traces, spans and trajectories, the identifier alphabet for
//! session keys, a decimal for step ids) and query values (422 `invalid_request`). The answers come from the
//! adapter's `trace_explorer`; every system but Relay answers 404. No value from a request ever reaches a file name:
//! what a handler passes on are ids the index looks up.

use axum::body::Body;
use axum::http::Response;

use crate::adapters::base::{AdapterError, AdapterRef};
use crate::adapters::relay_traces::Request as Explorer;
use crate::adapters::relay_traces::analytics::AnalyticsQuery;
use crate::adapters::relay_traces::content::ContentQuery;
use crate::adapters::relay_traces::derive::Status;
use crate::adapters::relay_traces::ids::{is_ident, is_uuid};
use crate::adapters::relay_traces::query::{
    Range, SessionQuery, SessionSort, TraceQuery, TraceSort, TrajQuery, WINDOWS, Window,
};
use crate::adapters::relay_traces::text::MAX_CHARS;
use crate::api::bodies::{V, query_int, query_str};
use crate::api::{
    Req, adapter_failure, dep_adapter, dep_admin, dep_viewer, json_response, validation_error,
};
use crate::domain::models::SystemId;
use crate::services::errors::ApiError;
use crate::services::pyfmt::list_repr;

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(422, "invalid_request", message)
}

/// The adapter of the route, or 404: these routes exist for Relay only.
fn relay(req: &Req, admin: bool) -> Result<AdapterRef, ApiError> {
    let a = dep_adapter(req)?;
    if admin {
        dep_admin(req)?;
    } else {
        dep_viewer(req)?;
    }
    if a.id() != SystemId::Relay {
        return Err(ApiError::new(
            404,
            "not_found",
            "no such endpoint for this system",
        ));
    }
    Ok(a)
}

fn explorer_error(e: &AdapterError) -> ApiError {
    match e {
        AdapterError::NoSuchEndpoint => {
            ApiError::new(404, "not_found", "no such endpoint for this system")
        }
        AdapterError::UnknownId(what) => ApiError::new(404, "not_found", format!("unknown {what}")),
        AdapterError::Invalid(m) => invalid(m.clone()),
        AdapterError::FileNotFound(m) => ApiError::new(409, "content_unavailable", m.clone()),
        AdapterError::Failed { class, message } if class == "PermissionError" => {
            ApiError::new(403, "content_disabled", message.clone())
        }
        other => adapter_failure(other),
    }
}

async fn answer(a: &AdapterRef, request: Explorer) -> Result<Response<Body>, ApiError> {
    match a.trace_explorer(request).await {
        Ok(v) => Ok(json_response(200, &v)),
        Err(e) => Err(explorer_error(&e)),
    }
}

/// One of a fixed set of words; absent (or empty) = the default.
fn word<T: Copy>(req: &Req, name: &str, choices: &[(&str, T)], default: T) -> Result<T, ApiError> {
    match query_str(&req.query, name).filter(|v| !v.is_empty()) {
        None => Ok(default),
        Some(v) => choices
            .iter()
            .find(|(k, _)| *k == v)
            .map(|(_, t)| *t)
            .ok_or_else(|| {
                let names: Vec<&str> = choices.iter().map(|(k, _)| *k).collect();
                invalid(format!("{name} must be one of {}", list_repr(&names)))
            }),
    }
}

/// An identifier filter (`client`, `model`, `kind`, `session`, `source`); absent or empty = no filter.
fn ident_filter(req: &Req, name: &str) -> Result<Option<String>, ApiError> {
    match query_str(&req.query, name).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if is_ident(&v) => Ok(Some(v)),
        Some(_) => Err(invalid(format!(
            "{name} must be an identifier: 1 to 120 characters of letters, digits and _ . : / @ + -"
        ))),
    }
}

/// `q`: at most 80 characters of the identifier alphabet, lower-cased for the search.
fn needle(req: &Req) -> Result<Option<String>, ApiError> {
    match query_str(&req.query, "q").filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if v.len() <= 80 && is_ident(&v) => Ok(Some(v.to_ascii_lowercase())),
        Some(_) => Err(invalid(
            "q must be at most 80 characters of letters, digits and _ . : / @ + -",
        )),
    }
}

fn range(req: &Req) -> Result<Range, ApiError> {
    Ok(Range {
        window: word(req, "window", &WINDOWS, Window::H24)?,
        anchor_now: word(req, "anchor", &[("latest", false), ("now", true)], false)?,
    })
}

/// An integer query value with bounds (FastAPI's messages, like every other route).
fn int(req: &Req, name: &str, default: i64, lo: i64, hi: i64) -> Result<i64, ApiError> {
    let mut v = V::new();
    let got = query_int(&mut v, &req.query, name, default, Some(lo), Some(hi));
    match got {
        Some(n) if v.errors.is_empty() => Ok(n),
        _ => Err(validation_error(&v.errors)),
    }
}

fn page(req: &Req) -> Result<(usize, usize), ApiError> {
    let limit = int(req, "limit", 50, 1, 200)?;
    let offset = int(req, "offset", 0, 0, 100_000_000)?;
    Ok((
        usize::try_from(limit).unwrap_or(50),
        usize::try_from(offset).unwrap_or(0),
    ))
}

fn uuid_param(req: &Req, name: &str, what: &str) -> Result<String, ApiError> {
    let v = req.param(name);
    if is_uuid(v) {
        Ok(v.to_ascii_lowercase())
    } else {
        Err(invalid(format!("{what} id must be UUID-shaped")))
    }
}

fn content_query(req: &Req) -> Result<ContentQuery, ApiError> {
    let max = i64::try_from(MAX_CHARS).unwrap_or(200_000);
    let max_chars = int(req, "max_chars", 32_768, 1000, max)?;
    Ok(ContentQuery {
        max_chars: usize::try_from(max_chars).unwrap_or(32_768),
        head: word(req, "messages", &[("tail", false), ("head", true)], false)?,
    })
}

pub async fn index(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    answer(&a, Explorer::Index).await
}

pub async fn traces(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let (limit, offset) = page(&req)?;
    let q = TraceQuery {
        range: range(&req)?,
        client: ident_filter(&req, "client")?,
        model: ident_filter(&req, "model")?,
        kind: ident_filter(&req, "kind")?,
        status: word(
            &req,
            "status",
            &[
                ("ok", Some(Status::Ok)),
                ("error", Some(Status::Error)),
                ("open", Some(Status::Open)),
            ],
            None,
        )?,
        session: ident_filter(&req, "session")?,
        q: needle(&req)?,
        min_ms: int(&req, "min_ms", 0, 0, 1_000_000_000)?,
        sort: word(
            &req,
            "sort",
            &[
                ("start", TraceSort::Start),
                ("duration", TraceSort::Duration),
                ("tokens", TraceSort::Tokens),
                ("llm_calls", TraceSort::LlmCalls),
            ],
            TraceSort::Start,
        )?,
        desc: word(&req, "order", &[("desc", true), ("asc", false)], true)?,
        limit,
        offset,
    };
    answer(&a, Explorer::Traces(q)).await
}

pub async fn trace(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let trace_id = uuid_param(&req, "trace_id", "trace")?;
    answer(&a, Explorer::Trace { trace_id }).await
}

pub async fn span_content(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, true)?;
    let trace_id = uuid_param(&req, "trace_id", "trace")?;
    let span_id = uuid_param(&req, "span_id", "span")?;
    let query = content_query(&req)?;
    answer(
        &a,
        Explorer::SpanContent {
            trace_id,
            span_id,
            query,
        },
    )
    .await
}

pub async fn sessions(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let (limit, offset) = page(&req)?;
    let q = SessionQuery {
        range: range(&req)?,
        client: ident_filter(&req, "client")?,
        kind: ident_filter(&req, "kind")?,
        q: needle(&req)?,
        sort: word(
            &req,
            "sort",
            &[
                ("last", SessionSort::Last),
                ("traces", SessionSort::Traces),
                ("tokens", SessionSort::Tokens),
            ],
            SessionSort::Last,
        )?,
        desc: word(&req, "order", &[("desc", true), ("asc", false)], true)?,
        limit,
        offset,
    };
    answer(&a, Explorer::Sessions(q)).await
}

pub async fn session(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let key = req.param("session_key");
    if !is_ident(key) {
        return Err(invalid(
            "session key must be 1 to 120 characters of letters, digits and _ . : / @ + -",
        ));
    }
    answer(
        &a,
        Explorer::Session {
            key: key.to_owned(),
        },
    )
    .await
}

pub async fn analytics(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let q = AnalyticsQuery {
        range: range(&req)?,
        client: ident_filter(&req, "client")?,
        model: ident_filter(&req, "model")?,
        kind: ident_filter(&req, "kind")?,
        buckets: usize::try_from(int(&req, "buckets", 60, 12, 240)?).unwrap_or(60),
    };
    answer(&a, Explorer::Analytics(q)).await
}

pub async fn trajectories(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let (limit, offset) = page(&req)?;
    let trace = match query_str(&req.query, "trace").filter(|v| !v.is_empty()) {
        None => None,
        Some(v) if is_uuid(&v) => Some(v.to_ascii_lowercase()),
        Some(_) => return Err(invalid("trace must be a UUID-shaped trace id")),
    };
    let q = TrajQuery {
        source: ident_filter(&req, "source")?,
        q: needle(&req)?,
        trace,
        limit,
        offset,
    };
    answer(&a, Explorer::Trajectories(q)).await
}

pub async fn trajectory(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    let id = uuid_param(&req, "trajectory_id", "trajectory")?;
    let source = ident_filter(&req, "source")?;
    answer(&a, Explorer::Trajectory { id, source }).await
}

pub async fn step_content(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, true)?;
    let id = uuid_param(&req, "trajectory_id", "trajectory")?;
    let step = req.param("step_id");
    let step_id =
        (!step.is_empty() && step.len() <= 18 && step.bytes().all(|b| b.is_ascii_digit()))
            .then(|| step.parse::<i64>().ok())
            .flatten()
            .ok_or_else(|| invalid("step id must be a decimal number"))?;
    let source = ident_filter(&req, "source")?;
    let query = content_query(&req)?;
    answer(
        &a,
        Explorer::StepContent {
            id,
            source,
            step_id,
            max_chars: query.max_chars,
        },
    )
    .await
}

pub async fn capture(req: Req) -> Result<Response<Body>, ApiError> {
    let a = relay(&req, false)?;
    answer(&a, Explorer::Capture).await
}
