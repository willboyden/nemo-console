//! Config files (validate / apply / history / rollback) and structured params. Port of `api/routers/configure.py`.

use axum::body::Body;
use axum::http::Response;
use serde_json::{Value, json};

use crate::adapters::base::AdapterError;
use crate::api::bodies::{V, apply_body, params_body, rollback_body, validate_body};
use crate::api::{
    Req, adapter_failure, dep_adapter, dep_admin, dep_id, dep_viewer, exclusive, json_response,
    ok_json, validation_error,
};
use crate::services::auth::PrincipalRole;
use crate::services::common::{cap, redact, redact_keep_env};
use crate::services::errors::ApiError;
use crate::services::params::validate_values;
use crate::services::pyfmt::char_prefix;

pub async fn config_list(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    ok_json(&a.config_files().map_err(|e| adapter_failure(&e))?)
}

pub async fn config_get(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    let file_id = dep_id(req.param("file_id"))?;
    dep_viewer(&req)?;
    let mut cf = req.state.config_store.read(&a, &file_id).await?;
    if req.principal.role != PrincipalRole::Admin {
        // viewers never see literal secrets an unvetted on-disk file may hold (env NAMES stay)
        cf.content = redact_keep_env(&cf.content);
    }
    ok_json(&cf)
}

pub async fn config_validate(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    let file_id = dep_id(req.param("file_id"))?;
    dep_admin(&req)?;
    let mut v = V::new();
    let body = validate_body(&req.body, &mut v);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    ok_json(
        &req.state
            .config_store
            .validate(&a, &file_id, &body.content)
            .await?,
    )
}

pub async fn config_apply(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    let file_id = dep_id(req.param("file_id"))?;
    dep_admin(&req)?;
    let mut v = V::new();
    let body = apply_body(&req.body, &mut v);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    let _busy = exclusive(&req.state, a.id())?;
    let res = req
        .state
        .config_store
        .apply(
            &a,
            &file_id,
            &body.content,
            &body.base_sha256,
            body.restart,
            body.confirm,
            &req.principal.name,
        )
        .await?;
    ok_json(&res)
}

pub async fn config_history(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    let file_id = dep_id(req.param("file_id"))?;
    dep_viewer(&req)?;
    ok_json(&req.state.config_store.history(&a, &file_id)?)
}

pub async fn config_rollback(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    let file_id = dep_id(req.param("file_id"))?;
    dep_admin(&req)?;
    let mut v = V::new();
    let body = rollback_body(&req.body, &mut v);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    let _busy = exclusive(&req.state, a.id())?;
    let res = req
        .state
        .config_store
        .rollback(
            &a,
            &file_id,
            &body.revision_id,
            body.confirm,
            &req.principal.name,
        )
        .await?;
    ok_json(&res)
}

pub async fn params_get(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_viewer(&req)?;
    let catalog = a.param_catalog().map_err(|e| adapter_failure(&e))?;
    let values = if catalog.is_empty() {
        serde_json::Map::new()
    } else {
        req.state.settings.saved_params(a.id().as_str())
    };
    Ok(json_response(
        200,
        &json!({"catalog": catalog, "values": values}),
    ))
}

pub async fn params_preview(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_admin(&req)?;
    let mut v = V::new();
    let body = params_body(&req.body, &mut v, false);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    let catalog = a.param_catalog().map_err(|e| adapter_failure(&e))?;
    if catalog.is_empty() {
        return Err(ApiError::new(
            404,
            "unsupported",
            "this system has no tunable parameters",
        ));
    }
    let values = validate_values(&catalog, &body.values)?;
    let rendered = match a.preview_params(&values).await {
        Ok(r) => r,
        Err(AdapterError::NotImplemented) => {
            return Err(ApiError::new(
                404,
                "unsupported",
                "this system cannot preview parameters",
            ));
        }
        Err(AdapterError::Invalid(msg)) => {
            return Err(ApiError::new(
                422,
                "params_invalid",
                char_prefix(&redact(&msg), 500),
            ));
        }
        Err(e) => return Err(adapter_failure(&e)),
    };
    let warnings: Vec<String> = match a.param_warnings(&values) {
        Some(Ok(w)) => w
            .iter()
            .take(20)
            .map(|x| char_prefix(&redact(x), 500).to_owned())
            .collect(),
        _ => Vec::new(),
    };
    Ok(json_response(
        200,
        &json!({"rendered": redact(&cap(&rendered, 200_000)), "warnings": warnings}),
    ))
}

pub async fn params_put(req: Req) -> Result<Response<Body>, ApiError> {
    let a = dep_adapter(&req)?;
    dep_admin(&req)?;
    let mut v = V::new();
    let body = params_body(&req.body, &mut v, true);
    let Some(body) = body.filter(|_| v.errors.is_empty()) else {
        return Err(validation_error(&v.errors));
    };
    let catalog = a.param_catalog().map_err(|e| adapter_failure(&e))?;
    if catalog.is_empty() {
        return Err(ApiError::new(
            404,
            "unsupported",
            "this system has no tunable parameters",
        ));
    }
    if !body.confirm {
        return Err(ApiError::new(
            400,
            "confirm_required",
            "confirm:true is required to apply parameters",
        ));
    }
    let values = validate_values(&catalog, &body.values)?;
    let _busy = exclusive(&req.state, a.id())?;
    let mut res = match a.apply_params(&values).await {
        Ok(r) => r,
        Err(AdapterError::NotImplemented) => {
            return Err(ApiError::new(
                404,
                "unsupported",
                "this system cannot apply parameters",
            ));
        }
        Err(e) => {
            let detail =
                char_prefix(&redact(&format!("apply failed: {}", e.py_text())), 500).to_owned();
            return Ok(json_response(
                200,
                &serde_json::to_value(crate::domain::models::ApplyResult::new(false, detail))
                    .unwrap_or(Value::Null),
            ));
        }
    };
    if res.ok {
        req.state.settings.save_params(a.id().as_str(), &values)?;
    }
    res.detail = cap(&redact(&res.detail), 2000);
    ok_json(&res)
}
