//! Actions (danger levels enforced here, params validated against the ActionSpec) and the playground proxy.
//! Port of `api/routers/act.py`.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Response};
use regex::Regex;
use serde_json::Value;

use crate::adapters::base::AdapterError;
use crate::api::bodies::{V, action_body, chat_body};
use crate::api::{
    Req, adapter_failure, dep_adapter, dep_admin, dep_id, dep_viewer, exclusive, json_response,
    ok_json, validation_error,
};
use crate::domain::models::{ActionResult, Danger, Problem, Role, Severity};
use crate::services::auth::PrincipalRole;
use crate::services::common::{cap, redact};
use crate::services::errors::ApiError;
use crate::services::params::validate_values;
use crate::services::playground;
use crate::services::pyfmt::char_prefix;

#[allow(clippy::expect_used)]
pub static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._:/@\-]{0,127}\z").expect("static regex")
});

pub async fn actions_list(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    ok_json(&a.actions().map_err(|e| adapter_failure(&e))?)
}

pub async fn action_run(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    let action = dep_id(req.param("action"))?;
    dep_viewer(&req)?;
    let mut v = V::new();
    let body = action_body(&req.body, &mut v);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    let specs = a.actions().map_err(|e| adapter_failure(&e))?;
    let Some(spec) = specs.into_iter().find(|s| s.id == action) else {
        return Err(ApiError::new(404, "not_found", "no such action"));
    };
    if spec.role == Role::Admin && req.principal.role != PrincipalRole::Admin {
        return Err(ApiError::new(
            403,
            "forbidden",
            "this action needs the admin role",
        ));
    }
    if spec.danger != Danger::Safe && !body.confirm {
        return Err(ApiError::new(
            400,
            "confirm_required",
            format!("'{action}' is {}; send confirm:true", spec.danger.as_str()),
        ));
    }
    let params = validate_values(&spec.params, &body.params)?;
    let st = &req.state;
    let _busy = exclusive(st, a.id())?;
    let t0 = Instant::now();
    let limit = st
        .cfg
        .action_timeouts
        .get(&format!("{}.{action}", a.id()))
        .copied()
        .unwrap_or(st.cfg.action_timeout_s);
    let outcome = match tokio::time::timeout(
        Duration::from_secs_f64(limit.max(0.0)),
        a.run_action(&action, &params),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err(AdapterError::Timeout),
    };
    let mut res = match outcome {
        Ok(r) => r,
        Err(AdapterError::UnknownId(_)) => {
            return Err(ApiError::new(404, "not_found", "no such action"));
        }
        Err(e) => {
            let msg = if matches!(e, AdapterError::Timeout) {
                "timed out".to_owned()
            } else {
                e.py_text()
            };
            #[allow(clippy::cast_possible_truncation)]
            let duration_ms = t0.elapsed().as_millis() as i64;
            let failed = ActionResult {
                ok: false,
                output: String::new(),
                duration_ms,
                problems: vec![Problem::new(
                    Severity::Error,
                    "console.action.failed",
                    char_prefix(&redact(&msg), 300).to_owned(),
                    None,
                )],
            };
            return ok_json(&failed);
        }
    };
    st.status.invalidate(a.id());
    res.output = cap(&redact(&res.output), 64_000);
    for p in &mut res.problems {
        p.message = redact(&p.message);
        p.fix = p.fix.as_deref().filter(|f| !f.is_empty()).map(redact);
    }
    ok_json(&res)
}

pub async fn playground_spec(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    ok_json(&a.playground().map_err(|e| adapter_failure(&e))?)
}

pub async fn playground_chat(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_admin(&req)?;
    let mut v = V::new();
    let body = chat_body(&req.body, &mut v);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    if !MODEL_RE.is_match(&body.model) {
        return Err(ApiError::new(
            422,
            "invalid_request",
            "model must be a plain model/route id",
        ));
    }
    let st = &req.state;
    let prepared = playground::prepare(
        &a,
        body.dump(),
        &st.settings.effective_key_env(),
        st.cfg.playground_max_tokens,
        &st.environ,
    )?;
    let resp = playground::open_upstream(&st.http, &prepared, 600.0).await?;
    let extra = playground::served_by(resp.headers());
    let mut out = if prepared.stream {
        let stream = playground::relay_stream(resp, prepared.secret.clone());
        let mut r = Response::new(Body::from_stream(futures::StreamExt::map(
            stream,
            Ok::<_, std::io::Error>,
        )));
        r.headers_mut().insert(
            "content-type",
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        r.headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
        r
    } else {
        let data = playground::read_capped(resp, prepared.secret.as_deref()).await?;
        let mut r = Response::new(Body::from(data));
        r.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        r
    };
    for (k, val) in extra {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(k), HeaderValue::try_from(val)) {
            out.headers_mut().insert(name, value);
        }
    }
    let _ = json_response; // (non-JSON answers are passed through as bytes)
    let _: Option<Value> = None;
    Ok(out)
}
