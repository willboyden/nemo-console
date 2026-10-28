//! HTTP layer: the route table, a FastAPI-equivalent request pipeline and the security middleware.
//! Port of `main.py` (app factory), `api/security.py`, `api/errors.py`, `api/deps.py`. Contract (PORTING.md 6.2/7).
//!
//! The pipeline mirrors Starlette + FastAPI exactly where an answer can differ:
//!   1. middleware precheck (Host 421, Origin 403, X-Console 403, Content-Length 413), then auth (401) for guarded
//!      paths; security headers on EVERY response, `Cache-Control: no-store` on /api/* and /metrics;
//!   2. routing on the percent-DECODED path, in Starlette's order; a path that matches with another method is a 405
//!      (the GET catch-all makes every non-GET path a 405, never a 404);
//!   3. per route: read the body (capped) when the route has a body model, parse it like FastAPI (`json.loads`, 422
//!      `body.<pos>` / 400), run the dependencies in declaration order (adapter 404, id 404, role 401/403), then
//!      validate query + body (422 with pydantic's texts), then the handler;
//!   4. after the answer: Prometheus counters, the denial log, and the audit row of every mutating /api call.

pub mod act;
pub mod admin;
pub mod bodies;
pub mod configure;
pub mod observe;
pub mod pyjson;
pub mod static_files;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use futures::FutureExt;
use regex::Regex;
use serde_json::{Value, json};

use crate::adapters::base::AdapterRef;
use crate::api::bodies::RawBody;
use crate::api::pyjson::{JsonFail, loads};
use crate::container::AppState;
use crate::domain::models::SystemId;
use crate::services::audit::summarize_body;
use crate::services::auth::{Principal, PrincipalRole};
use crate::services::common::redact;
use crate::services::errors::ApiError;
use crate::services::pyfmt::char_prefix;
use crate::services::pyvalidate::{PyError, render_detail};

pub const API: &str = "/api/v1";
pub const OPEN_PATHS: [&str; 1] = ["/api/v1/health"];
pub const CSRF_HEADER: &str = "x-console";
pub const AUDIT_BODY_CAP: usize = 64 * 1024;
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
pub const SECURITY_HEADERS: [(&str, &str); 7] = [
    ("content-security-policy", CSP),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), interest-cohort=()",
    ),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
];
const SAFE: [&str; 3] = ["GET", "HEAD", "OPTIONS"];
const KNOWN_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// The role dependency a route declares (`public` / `viewer` / `admin`); `None`: none (the OpenAPI document).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleDep {
    Public,
    Viewer,
    Admin,
    None,
}

impl RoleDep {
    #[must_use]
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            RoleDep::Public => Some("public"),
            RoleDep::Viewer => Some("viewer"),
            RoleDep::Admin => Some("admin"),
            RoleDep::None => None,
        }
    }
}

/// Which handler a route runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H {
    Health,
    Overview,
    Topology,
    Systems,
    Status,
    Metrics,
    History,
    Events,
    EventsStream,
    RunRecords,
    RunFiles,
    ConfigList,
    ConfigGet,
    ConfigValidate,
    ConfigApply,
    ConfigHistory,
    ConfigRollback,
    ParamsGet,
    ParamsPreview,
    ParamsPut,
    ActionsList,
    ActionRun,
    PlaygroundSpec,
    PlaygroundChat,
    Routing,
    Audit,
    SettingsGet,
    SettingsPut,
    ConsoleMetrics,
    OpenApi,
    Static,
}

/// One route: method(s), full path template, the metric `route` label (FastAPI >= 0.140 reports the router-relative
/// path, e.g. "/systems/{system_id}/status"; the OpenAPI route reports none), role dependency, body model, handler.
pub struct RouteDef {
    pub methods: &'static [&'static str],
    pub path: &'static str,
    pub label: Option<&'static str>,
    pub role: RoleDep,
    pub body: bool,
    pub handler: H,
}

macro_rules! r {
    ($m:expr, $label:expr, $role:ident, $body:expr, $h:ident) => {
        RouteDef {
            methods: $m,
            path: concat!("/api/v1", $label),
            label: Some($label),
            role: RoleDep::$role,
            body: $body,
            handler: H::$h,
        }
    };
}

const GET: &[&str] = &["GET"];
const POST: &[&str] = &["POST"];
const PUT: &[&str] = &["PUT"];

/// Starlette's matching order: the OpenAPI route, the /api/v1 routers (observe, configure, act, admin), the root
/// router (/metrics), the static catch-all.
pub static ROUTES: &[RouteDef] = &[
    RouteDef {
        methods: &["GET", "HEAD"],
        path: "/api/v1/openapi.json",
        label: None,
        role: RoleDep::None,
        body: false,
        handler: H::OpenApi,
    },
    // observe
    r!(GET, "/health", Public, false, Health),
    r!(GET, "/overview", Viewer, false, Overview),
    r!(GET, "/topology", Viewer, false, Topology),
    r!(GET, "/systems", Viewer, false, Systems),
    r!(GET, "/systems/{system_id}/status", Viewer, false, Status),
    r!(GET, "/systems/{system_id}/metrics", Viewer, false, Metrics),
    r!(
        GET,
        "/systems/{system_id}/metrics/history",
        Viewer,
        false,
        History
    ),
    r!(GET, "/systems/{system_id}/events", Viewer, false, Events),
    r!(
        GET,
        "/systems/{system_id}/events/stream",
        Viewer,
        false,
        EventsStream
    ),
    r!(
        GET,
        "/systems/{system_id}/runs/{run_id}/records",
        Viewer,
        false,
        RunRecords
    ),
    r!(
        GET,
        "/systems/{system_id}/runs/{run_id}/files",
        Viewer,
        false,
        RunFiles
    ),
    // configure
    r!(
        GET,
        "/systems/{system_id}/config",
        Viewer,
        false,
        ConfigList
    ),
    r!(
        GET,
        "/systems/{system_id}/config/{file_id}",
        Viewer,
        false,
        ConfigGet
    ),
    r!(
        POST,
        "/systems/{system_id}/config/{file_id}/validate",
        Admin,
        true,
        ConfigValidate
    ),
    r!(
        POST,
        "/systems/{system_id}/config/{file_id}/apply",
        Admin,
        true,
        ConfigApply
    ),
    r!(
        GET,
        "/systems/{system_id}/config/{file_id}/history",
        Viewer,
        false,
        ConfigHistory
    ),
    r!(
        POST,
        "/systems/{system_id}/config/{file_id}/rollback",
        Admin,
        true,
        ConfigRollback
    ),
    r!(GET, "/systems/{system_id}/params", Viewer, false, ParamsGet),
    r!(
        POST,
        "/systems/{system_id}/params/preview",
        Admin,
        true,
        ParamsPreview
    ),
    r!(PUT, "/systems/{system_id}/params", Admin, true, ParamsPut),
    // act
    r!(
        GET,
        "/systems/{system_id}/actions",
        Viewer,
        false,
        ActionsList
    ),
    r!(
        POST,
        "/systems/{system_id}/actions/{action}",
        Viewer,
        true,
        ActionRun
    ),
    r!(
        GET,
        "/systems/{system_id}/playground",
        Viewer,
        false,
        PlaygroundSpec
    ),
    r!(
        POST,
        "/systems/{system_id}/playground/chat",
        Admin,
        true,
        PlaygroundChat
    ),
    // admin
    r!(GET, "/routing/summary", Viewer, false, Routing),
    r!(GET, "/audit", Admin, false, Audit),
    r!(GET, "/settings", Viewer, false, SettingsGet),
    r!(PUT, "/settings", Admin, true, SettingsPut),
    // root router
    RouteDef {
        methods: GET,
        path: "/metrics",
        label: Some("/metrics"),
        role: RoleDep::Viewer,
        body: false,
        handler: H::ConsoleMetrics,
    },
    // the static catch-all (public)
    RouteDef {
        methods: GET,
        path: "/{path:path}",
        label: Some("/{path:path}"),
        role: RoleDep::Public,
        body: false,
        handler: H::Static,
    },
];

#[allow(clippy::expect_used)]
/// A route table compiled for matching: one (regex, path-parameter names) per route.
pub type Compiled = Vec<(Regex, Vec<&'static str>)>;

static COMPILED: LazyLock<Compiled> = LazyLock::new(|| compile_routes(ROUTES));

/// Compile a route table (`{name}` and `{name:path}` parameters).
#[must_use]
#[allow(clippy::expect_used)]
pub fn compile_routes(routes: &'static [RouteDef]) -> Compiled {
    let param = Regex::new(r"\{([a-z_]+)(:path)?\}").expect("static regex");
    routes
        .iter()
        .map(|r| {
            let mut names = Vec::new();
            let mut out = String::from("^");
            let mut last = 0;
            for c in param.captures_iter(r.path) {
                let (Some(m), Some(name)) = (c.get(0), c.get(1)) else {
                    continue;
                };
                out.push_str(&regex::escape(&r.path[last..m.start()]));
                let name: &'static str = &r.path[name.start()..name.end()];
                names.push(name);
                out.push_str(if c.get(2).is_some() {
                    "(.*)"
                } else {
                    "([^/]+)"
                });
                last = m.end();
            }
            out.push_str(&regex::escape(&r.path[last..]));
            out.push('$');
            (Regex::new(&out).expect("route regex"), names)
        })
        .collect()
}

/// Structural role enforcement (audit finding 1): the route table's role is checked for EVERY route before its handler
/// runs (the handlers keep their own `dep_*` calls as defence in depth). FastAPI resolves a route's dependencies in
/// declaration order and every Python route declares the adapter and the id dependencies before the role, so the
/// same order is kept here: `{system_id}` (404 unknown system), `{file_id}` / `{action}` (404 not found), then the role
/// (401 without a key, 403 for a viewer on an admin route). The JSON body is parsed before (FastAPI does the same);
/// body and query validation come after (in the handler).
pub fn route_guard(def: &RouteDef, req: &Req) -> Result<(), ApiError> {
    match def.role {
        RoleDep::Viewer | RoleDep::Admin => {}
        RoleDep::Public | RoleDep::None => return Ok(()),
    }
    if req.params.contains_key("system_id") {
        dep_adapter(req)?;
    }
    for id in ["file_id", "action"] {
        if let Some(v) = req.params.get(id) {
            dep_id(v)?;
        }
    }
    match def.role {
        RoleDep::Admin => dep_admin(req),
        _ => dep_viewer(req),
    }
}

/// The route table as rows (method, path, auth) for parity/routes.rust.json.
#[must_use]
pub fn route_rows() -> Vec<Value> {
    let mut rows = Vec::new();
    for r in ROUTES {
        for m in r.methods {
            rows.push(
                json!({"method": m, "path": r.path, "auth": r.role.as_str(), "body": r.body}),
            );
        }
    }
    rows.sort_by(|a, b| {
        (a["path"].as_str(), a["method"].as_str()).cmp(&(b["path"].as_str(), b["method"].as_str()))
    });
    rows
}

/// One request as the handlers see it.
pub struct Req {
    pub state: Arc<AppState>,
    pub method: String,
    /// Percent-DECODED path (ASGI `scope["path"]`).
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: HeaderMap,
    pub principal: Principal,
    pub params: HashMap<&'static str, String>,
    pub body: RawBody,
}

impl Req {
    #[must_use]
    pub fn param(&self, name: &str) -> &str {
        self.params.get(name).map_or("", String::as_str)
    }
}

// ── responses ───────────────────────────────────────────────────────────────────────────────────

/// `problem(status, code, message)`: `{"error": {"code", "message": redact(message)[:1200]}}`.
#[must_use]
pub fn problem(status: u16, code: &str, message: &str) -> Response<Body> {
    let body = json!({"error": {"code": code, "message": char_prefix(&redact(message), 1200)}});
    json_response(status, &body)
}

#[must_use]
pub fn json_response(status: u16, body: &Value) -> Response<Body> {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let mut resp = Response::new(Body::from(bytes));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    resp.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    resp
}

/// Serialise any model as a 200 JSON answer.
pub fn ok_json<T: serde::Serialize>(v: &T) -> Result<Response<Body>, ApiError> {
    let value = serde_json::to_value(v).map_err(|_| ApiError::internal())?;
    Ok(json_response(200, &value))
}

impl ApiError {
    #[must_use]
    pub fn into_response(self) -> Response<Body> {
        problem(self.status, &self.code, &self.message)
    }
}

/// 422 `invalid_request` with pydantic's detail text (`api/errors.py` `_val`).
#[must_use]
pub fn validation_error(errors: &[PyError]) -> ApiError {
    ApiError::new(422, "invalid_request", render_detail(errors))
}

// ── dependencies ────────────────────────────────────────────────────────────────────────────────

#[allow(clippy::expect_used)]
static ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9_.\-]{0,63}\z").expect("static regex"));

/// `get_adapter`: 404 "unknown system"; a not-yet-ported adapter answers 501 (skeleton phase).
pub fn dep_adapter(req: &Req) -> Result<AdapterRef, ApiError> {
    let sid = SystemId::parse(req.param("system_id"))
        .ok_or_else(|| ApiError::new(404, "not_found", "unknown system"))?;
    let a = req
        .state
        .adapter(sid)
        .cloned()
        .ok_or_else(|| ApiError::new(404, "not_found", "unknown system"))?;
    Ok(a)
}

/// `valid_id` (file_id / action): 404 "not found".
pub fn dep_id(value: &str) -> Result<String, ApiError> {
    if ID_RE.is_match(value) {
        Ok(value.to_owned())
    } else {
        Err(ApiError::new(404, "not_found", "not found"))
    }
}

/// `viewer` dependency.
pub fn dep_viewer(req: &Req) -> Result<(), ApiError> {
    match req.principal.role {
        PrincipalRole::Viewer | PrincipalRole::Admin => Ok(()),
        PrincipalRole::Public => Err(ApiError::new(
            401,
            "unauthorized",
            "a valid bearer API key is required",
        )),
    }
}

/// `admin` dependency.
pub fn dep_admin(req: &Req) -> Result<(), ApiError> {
    match req.principal.role {
        PrincipalRole::Admin => Ok(()),
        PrincipalRole::Viewer => Err(ApiError::new(
            403,
            "forbidden",
            "this key has the viewer role (read-only)",
        )),
        PrincipalRole::Public => Err(ApiError::new(
            401,
            "unauthorized",
            "a valid bearer API key is required",
        )),
    }
}

/// `exclusive(c, system)`: one mutating operation per system at a time (409 busy otherwise).
pub fn exclusive(
    state: &AppState,
    system: SystemId,
) -> Result<tokio::sync::OwnedMutexGuard<()>, ApiError> {
    state.lock(system).try_lock_owned().map_err(|_| {
        ApiError::new(
            409,
            "busy",
            format!("another change to {system} is in progress"),
        )
    })
}

// ── middleware ──────────────────────────────────────────────────────────────────────────────────

fn guarded(path: &str) -> bool {
    (path.starts_with("/api/") || path == "/metrics") && !OPEN_PATHS.contains(&path)
}

/// `urlsplit(origin).netloc`: the authority after `scheme://` (or a leading `//`), up to `/`, `?` or `#`.
#[must_use]
pub fn netloc(url: &str) -> &str {
    let after_scheme = match url.find(':') {
        Some(i)
            if i > 0
                && url[..i].starts_with(|c: char| c.is_ascii_alphabetic())
                && url[..i]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            &url[i + 1..]
        }
        _ => url,
    };
    let Some(rest) = after_scheme.strip_prefix("//") else {
        return "";
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    &rest[..end]
}

#[cfg(test)]
mod netloc_tests {
    use super::netloc;

    #[test]
    fn netloc_like_urlsplit() {
        assert_eq!(netloc("http://127.0.0.1:8793"), "127.0.0.1:8793");
        assert_eq!(netloc("http://evil.example"), "evil.example");
        assert_eq!(netloc("https://[::1]:8793/x?y"), "[::1]:8793");
        assert_eq!(netloc("//host:1/p"), "host:1");
        assert_eq!(netloc("foo"), "");
        assert_eq!(netloc("null"), "");
    }
}

/// Python `urllib.parse.unquote` (UTF-8, errors replaced) for the ASGI path.
#[must_use]
pub fn unquote(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// `parse_qsl(qs, keep_blank_values=True)`.
#[must_use]
pub fn parse_qsl(qs: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in qs.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        let dec = |s: &str| unquote(&s.replace('+', " "));
        out.push((dec(k), dec(v)));
    }
    out
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<std::borrow::Cow<'a, str>> {
    // Python builds a dict from the header list: the LAST occurrence wins. Starlette decodes header bytes as latin-1,
    // so a value with bytes >= 0x80 is still a value (audit finding 10: `to_str()` alone made a non-ASCII Origin
    // look absent and skip the Origin check).
    h.get_all(name)
        .iter()
        .next_back()
        .map(|v| match v.to_str() {
            Ok(s) => std::borrow::Cow::Borrowed(s),
            Err(_) => {
                std::borrow::Cow::Owned(v.as_bytes().iter().map(|&b| char::from(b)).collect())
            }
        })
}

struct Precheck<'a> {
    state: &'a AppState,
    hosts: Vec<String>,
}

impl<'a> Precheck<'a> {
    fn new(state: &'a AppState) -> Self {
        let cfg = &state.cfg;
        let port = cfg.port;
        let mut hosts: Vec<String> = [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
            format!("{}:{port}", cfg.host),
        ]
        .iter()
        .map(|h| h.to_lowercase())
        .collect();
        hosts.extend(cfg.allowed_hosts.iter().map(|h| h.to_lowercase()));
        Precheck { state, hosts }
    }

    fn check(
        &self,
        method: &str,
        headers: &HeaderMap,
    ) -> Option<(u16, &'static str, &'static str)> {
        let host = header(headers, "host")
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        if !self.hosts.contains(&host) {
            return Some((421, "misdirected_request", "unrecognised Host header"));
        }
        if !SAFE.contains(&method) {
            if let Some(origin) = header(headers, "origin")
                && (origin == "null" || netloc(&origin).to_lowercase() != host)
            {
                return Some((403, "cross_origin", "cross-origin requests are not allowed"));
            }
            if header(headers, CSRF_HEADER).as_deref() != Some("1") {
                return Some((
                    403,
                    "csrf_header_required",
                    "send the header 'X-Console: 1' on non-GET requests",
                ));
            }
            if let Some(cl) = header(headers, "content-length")
                && !cl.is_empty()
                && cl.chars().all(|c| c.is_ascii_digit())
                && cl.parse::<u128>().is_ok_and(|n| {
                    n > u128::try_from(self.state.cfg.body_cap_bytes.max(0)).unwrap_or(0)
                })
            {
                return Some((413, "payload_too_large", "request body too large"));
            }
        }
        None
    }
}

/// The body as read so far, for the audit row.
#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    total: usize,
}

/// Read the body with the cap (`BodyTooLarge` -> 413 `request body too large`).
/// Total time a request body may take to arrive (audit finding 4; uvicorn had no limit: a slow body held a worker).
/// Past it the request is answered 408 (PORTING D-26). A test seam: tests lower it.
pub static BODY_READ_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(30_000);

/// "There was an error parsing the body": FastAPI's answer to any exception while reading or decoding the body.
fn body_error() -> ApiError {
    ApiError::new(400, "bad_request", "There was an error parsing the body")
}

async fn read_body(body: Body, cap: usize, capture: &mut Capture) -> Result<Vec<u8>, ApiError> {
    let limit =
        Duration::from_millis(BODY_READ_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed));
    match tokio::time::timeout(limit, read_body_inner(body, cap, capture)).await {
        Ok(r) => r,
        Err(_) => Err(ApiError::new(
            408,
            "request_timeout",
            "the request body did not arrive in time",
        )),
    }
}

async fn read_body_inner(
    body: Body,
    cap: usize,
    capture: &mut Capture,
) -> Result<Vec<u8>, ApiError> {
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        // a body that ends early (the client went away: Content-Length not reached, a broken chunked encoding) is NOT
        // a complete body: Starlette raises ClientDisconnect and FastAPI answers 400 (audit finding 4)
        let Ok(chunk) = chunk else {
            return Err(body_error());
        };
        capture.total += chunk.len();
        if capture.total > cap {
            return Err(ApiError::new(
                413,
                "payload_too_large",
                "request body too large",
            ));
        }
        if capture.bytes.len() < AUDIT_BODY_CAP {
            let take = chunk.len().min(AUDIT_BODY_CAP - capture.bytes.len());
            capture.bytes.extend_from_slice(&chunk[..take]);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// FastAPI's body parsing: empty -> absent; JSON content types (or none) -> `json.loads`; others -> raw bytes.
fn parse_body(bytes: &[u8], content_type: Option<&str>) -> Result<RawBody, ApiError> {
    if bytes.is_empty() {
        return Ok(RawBody::Absent);
    }
    // FastAPI >= 0.120 `strict_content_type=True`: a body without a Content-Type is NOT parsed as JSON
    let is_json = match content_type {
        None => false,
        Some(ct) => {
            let mt = ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match mt.split_once('/') {
                Some(("application", sub)) => sub == "json" || sub.ends_with("+json"),
                _ => false,
            }
        }
    };
    if !is_json {
        return Ok(RawBody::Bytes);
    }
    match loads(bytes) {
        Ok(v) => Ok(RawBody::Json(v)),
        Err(JsonFail::Decode(pos)) => Err(ApiError::new(
            422,
            "invalid_request",
            format!("body.{pos}: JSON decode error"),
        )),
        Err(JsonFail::NotText | JsonFail::TooDeep) => Err(body_error()),
    }
}

fn add_security_headers(resp: &mut Response<Body>, api: bool) {
    let h = resp.headers_mut();
    for (name, _) in SECURITY_HEADERS {
        h.remove(name);
    }
    if api {
        h.remove("cache-control");
    }
    for (name, value) in SECURITY_HEADERS {
        h.append(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    if api {
        h.append("cache-control", HeaderValue::from_static("no-store"));
    }
}

/// The axum fallback: every request goes through here.
pub async fn dispatch(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Response<Body> {
    if let Some(resp) = h11_reject(&req) {
        return resp;
    }
    handle(state, Some(addr.ip().to_string()), req).await
}

/// The whole pipeline for one request (also used by tests with any client host).
pub async fn handle(
    state: Arc<AppState>,
    client_host: Option<String>,
    req: Request<Body>,
) -> Response<Body> {
    handle_in(ROUTES, &COMPILED, state, client_host, req).await
}

/// uvicorn (h11) refuses some requests before the app sees them: an HTTP/1.1 request without a Host header, or with
/// several. Its answer: 400, `text/plain; charset=utf-8`, `Connection: close`, "Invalid HTTP request received.", no
/// app headers, no metrics, no audit.
fn h11_reject(req: &Request<Body>) -> Option<Response<Body>> {
    let hosts = req.headers().get_all("host").iter().count();
    let bad = hosts > 1 || (hosts == 0 && req.version() == axum::http::Version::HTTP_11);
    if !bad {
        return None;
    }
    let mut r = Response::new(Body::from("Invalid HTTP request received."));
    *r.status_mut() = StatusCode::BAD_REQUEST;
    r.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r.headers_mut()
        .insert("connection", HeaderValue::from_static("close"));
    Some(r)
}

/// [`handle`] over any route table (tests prove the pipeline itself enforces a route's role).
pub async fn handle_in(
    routes: &'static [RouteDef],
    compiled: &Compiled,
    state: Arc<AppState>,
    client_host: Option<String>,
    req: Request<Body>,
) -> Response<Body> {
    let t0 = Instant::now();
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_owned();
    let path = unquote(parts.uri.path());
    let query = parse_qsl(parts.uri.query().unwrap_or(""));
    let headers = parts.headers;
    let pre = Precheck::new(&state);
    let mut deny = pre.check(&method, &headers);
    let mut principal = Principal::public();
    if deny.is_none() && (guarded(&path) || OPEN_PATHS.contains(&path.as_str())) {
        let forwarded =
            headers.contains_key("x-forwarded-for") || headers.contains_key("forwarded");
        match state.auth.principal_for(
            header(&headers, "authorization").as_deref(),
            client_host.as_deref(),
            forwarded,
        ) {
            Some(p) => principal = p,
            None if guarded(&path) => {
                deny = Some((401, "unauthorized", "a valid bearer API key is required"));
            }
            None => {}
        }
    }
    let mut capture = Capture::default();
    let mut route_label: Option<&'static str> = None;
    let content_type = header(&headers, "content-type").map(std::borrow::Cow::into_owned);
    let mut resp = if let Some((status, code, msg)) = deny {
        problem(status, code, &format!("{}: {msg}", code.replace('_', " ")))
    } else {
        let mut partial: Option<&RouteDef> = None;
        let mut found: Option<(&RouteDef, HashMap<&'static str, String>)> = None;
        for (def, (rx, names)) in routes.iter().zip(compiled.iter()) {
            if let Some(c) = rx.captures(&path) {
                if def.methods.contains(&method.as_str()) {
                    let mut params = HashMap::new();
                    for (i, n) in names.iter().enumerate() {
                        params.insert(
                            *n,
                            c.get(i + 1)
                                .map_or(String::new(), |m| m.as_str().to_owned()),
                        );
                    }
                    found = Some((def, params));
                    break;
                }
                if partial.is_none() {
                    partial = Some(def);
                }
            }
        }
        match found {
            Some((def, params)) => {
                route_label = def.label;
                let cap = usize::try_from(state.cfg.body_cap_bytes.max(0)).unwrap_or(usize::MAX);
                let body_result = if def.body {
                    read_body(body, cap, &mut capture).await.map(Some)
                } else {
                    Ok(None)
                };
                let run = async {
                    let raw = match body_result {
                        Ok(Some(bytes)) => parse_body(&bytes, content_type.as_deref())?,
                        Ok(None) => RawBody::Absent,
                        Err(e) => return Err(e),
                    };
                    let r = Req {
                        state: Arc::clone(&state),
                        method: method.clone(),
                        path: path.clone(),
                        query,
                        headers: headers.clone(),
                        principal: principal.clone(),
                        params,
                        body: raw,
                    };
                    route_guard(def, &r)?;
                    run_handler(def.handler, r).await
                };
                match std::panic::AssertUnwindSafe(run).catch_unwind().await {
                    Ok(Ok(resp)) => resp,
                    Ok(Err(e)) => e.into_response(),
                    Err(_) => {
                        tracing::error!(
                            "unhandled error in {} {}",
                            if KNOWN_METHODS.contains(&method.as_str()) {
                                method.as_str()
                            } else {
                                "OTHER"
                            },
                            char_prefix(&path, 80)
                        );
                        problem(500, "internal_error", "internal error")
                    }
                }
            }
            None => {
                route_label = partial.and_then(|p| p.label);
                problem(405, "method_not_allowed", "Method Not Allowed")
            }
        }
    };
    let api = path.starts_with("/api/") || path == "/metrics";
    add_security_headers(&mut resp, api);
    if method == "HEAD" {
        // Starlette sends the headers of a HEAD answer without its body
        *resp.body_mut() = Body::empty();
    }
    let status = resp.status().as_u16();
    let route = route_label.unwrap_or(if guarded(&path) {
        "unmatched"
    } else {
        "static"
    });
    let m = if KNOWN_METHODS.contains(&method.as_str()) {
        method.as_str()
    } else {
        "OTHER"
    };
    state.telemetry.request(m, route, &status.to_string());
    state
        .telemetry
        .latency(m, route, t0.elapsed().as_secs_f64());
    let denied = deny.is_some()
        || (principal.role == PrincipalRole::Viewer
            && status == 403
            && !SAFE.contains(&method.as_str()));
    if denied && api {
        let label = if [401, 403, 421, 429].contains(&status) {
            status
        } else {
            403
        };
        state.telemetry.denied(&label.to_string());
        state.denials.note(status, &method);
    } else if path.starts_with("/api/")
        && !SAFE.contains(&method.as_str())
        && principal.role != PrincipalRole::Public
    {
        let params = if !capture.bytes.is_empty()
            && content_type.as_deref().unwrap_or("").contains("json")
        {
            match loads(&capture.bytes) {
                Ok(doc) => summarize_body(&doc.to_value()),
                Err(JsonFail::TooDeep) => {
                    // Python's `_audit` catches ValueError only: the RecursionError escapes the middleware after
                    // the answer was sent (uvicorn logs it) and no row is written
                    tracing::error!(
                        "Exception in ASGI application: RecursionError while auditing the body"
                    );
                    return resp;
                }
                Err(_) => Value::String(format!("[unparseable json, {} bytes]", capture.total)),
            }
        } else if capture.total > 0 {
            Value::String(format!("[{} bytes, non-json]", capture.total))
        } else {
            Value::Null
        };
        if state
            .audit
            .record(
                &principal.name,
                principal.role.as_str(),
                &method,
                &path,
                status,
                &params,
            )
            .is_err()
        {
            tracing::error!("audit write failed");
        }
    }
    resp
}

async fn run_handler(h: H, req: Req) -> Result<Response<Body>, ApiError> {
    match h {
        H::Health => observe::health(req).await,
        H::Overview => observe::overview(req).await,
        H::Topology => observe::topology(req).await,
        H::Systems => observe::systems(req).await,
        H::Status => observe::system_status(req).await,
        H::Metrics => observe::system_metrics(req).await,
        H::History => observe::metrics_history(req).await,
        H::Events => observe::events(req).await,
        H::EventsStream => observe::events_stream(req).await,
        H::RunRecords => observe::run_records(req).await,
        H::RunFiles => observe::run_files(req).await,
        H::ConfigList => configure::config_list(req).await,
        H::ConfigGet => configure::config_get(req).await,
        H::ConfigValidate => configure::config_validate(req).await,
        H::ConfigApply => configure::config_apply(req).await,
        H::ConfigHistory => configure::config_history(req).await,
        H::ConfigRollback => configure::config_rollback(req).await,
        H::ParamsGet => configure::params_get(req).await,
        H::ParamsPreview => configure::params_preview(req).await,
        H::ParamsPut => configure::params_put(req).await,
        H::ActionsList => act::actions_list(req).await,
        H::ActionRun => act::action_run(req).await,
        H::PlaygroundSpec => act::playground_spec(req).await,
        H::PlaygroundChat => act::playground_chat(req).await,
        H::Routing => admin::routing(req).await,
        H::Audit => admin::audit(req).await,
        H::SettingsGet => admin::settings_get(req).await,
        H::SettingsPut => admin::settings_put(req).await,
        H::ConsoleMetrics => admin::console_metrics(req).await,
        H::OpenApi => admin::openapi(req).await,
        H::Static => static_files::serve(req).await,
    }
}

/// The axum app: one fallback handler (routing is done by [`handle`] to keep Starlette's semantics).
pub fn app(state: Arc<AppState>) -> axum::Router {
    axum::Router::new().fallback(dispatch).with_state(state)
}

/// Turn a failing adapter call into the HTTP answer the Python core gives for an uncaught exception (500).
#[must_use]
pub fn adapter_failure(_e: &crate::adapters::base::AdapterError) -> ApiError {
    ApiError::internal()
}
