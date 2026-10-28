//! Playground proxy: streamed, clamped, never persisted. The credential is resolved server-side from an env-var NAME.
//! Port of `services/playground.py`.

use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use crate::adapters::base::AdapterRef;
use crate::services::common::{KNOWN, redact};
use crate::services::environ::Environ;
use crate::services::errors::ApiError;
use crate::services::http::{Http, HttpError};
use crate::services::pyfmt::char_prefix;

pub const ROUTE_HEADERS: [&str; 3] = ["x-served-by-route", "x-switchyard-route", "x-route"];
pub const TIER_HEADERS: [&str; 3] = ["x-served-by-tier", "x-switchyard-tier", "x-tier"];
pub const MAX_NONSTREAM_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ERROR_BYTES: usize = 4096;

pub struct Prepared {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Map<String, Value>,
    pub stream: bool,
    pub secret: Option<String>,
}

/// (env NAME, value). The value is registered with the redactor so it can never leak out of any response/log.
#[must_use]
pub fn resolve_secret(
    key_env: &IndexMap<String, String>,
    system: &str,
    environ: &Environ,
) -> (Option<String>, Option<String>) {
    let Some(name) = key_env.get(system).filter(|n| !n.is_empty()) else {
        return (None, None);
    };
    let value = environ.get(name).filter(|v| !v.is_empty());
    if let Some(v) = &value {
        KNOWN.add(v);
    }
    (Some(name.clone()), value)
}

/// Register an env var's VALUE with the redactor (startup: every credential name the console knows).
pub fn register_secret_env(name: &str, environ: &Environ) {
    if let Some(v) = environ.get(name)
        && !v.is_empty()
    {
        KNOWN.add(&v);
    }
}

/// `prepare(a, body, key_env, max_tokens_cap)`; `body` is `ChatBody.model_dump(exclude_none=True)`.
pub fn prepare(
    a: &AdapterRef,
    mut body: Map<String, Value>,
    key_env: &IndexMap<String, String>,
    max_tokens_cap: i64,
    environ: &Environ,
) -> Result<Prepared, ApiError> {
    let spec = a.playground().map_err(|_| ApiError::internal())?;
    let url = a.playground_url();
    let (Some(spec), Some(url)) = (spec, url) else {
        return Err(ApiError::new(
            404,
            "not_found",
            "this system has no playground",
        ));
    };
    let (name, secret) = resolve_secret(key_env, a.id().as_str(), environ);
    if let Some(n) = &name
        && secret.is_none()
    {
        return Err(ApiError::new(
            503,
            "credential_missing",
            format!("env var {n} is not set in the console process"),
        ));
    }
    let openai = spec.wire == "openai_chat";
    if openai
        && let Some(sys) = body
            .get("system")
            .filter(|s| crate::services::routing::truthy(s))
            .cloned()
    {
        let mut msgs = vec![json!({"role": "system", "content": sys})];
        if let Some(Value::Array(m)) = body.get("messages") {
            msgs.extend(m.iter().cloned());
        }
        body.insert("messages".to_owned(), Value::Array(msgs));
    }
    if openai {
        body.shift_remove("system");
    }
    let mt = body
        .get("max_tokens")
        .and_then(Value::as_i64)
        .filter(|v| *v != 0);
    let clamped = match mt {
        Some(v) => v.min(max_tokens_cap).max(1),
        None => 1024.min(max_tokens_cap),
    };
    body.insert("max_tokens".to_owned(), json!(clamped));
    let stream = body
        .get("stream")
        .is_some_and(crate::services::routing::truthy);
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept-encoding".to_owned(), "identity".to_owned()),
        (
            "accept".to_owned(),
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            }
            .to_owned(),
        ),
    ];
    if let Some(s) = &secret {
        headers.push(("authorization".to_owned(), format!("Bearer {s}")));
        if spec.wire == "anthropic_messages" {
            headers.push(("x-api-key".to_owned(), s.clone()));
        }
    }
    if spec.wire == "anthropic_messages" {
        headers.push(("anthropic-version".to_owned(), "2023-06-01".to_owned()));
    }
    Ok(Prepared {
        url,
        headers,
        body,
        stream,
        secret,
    })
}

/// The X-Served-By-Route / -Tier headers copied from the upstream answer.
#[must_use]
pub fn served_by(headers: &http::HeaderMap) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (src, dst) in [
        (&ROUTE_HEADERS, "X-Served-By-Route"),
        (&TIER_HEADERS, "X-Served-By-Tier"),
    ] {
        for h in src {
            if let Some(v) = headers
                .get(*h)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
            {
                out.push((dst, char_prefix(&redact(v), 100).replace('\n', " ")));
                break;
            }
        }
    }
    out
}

/// Send and return a response whose body is NOT yet read. Transport/upstream errors become ApiErrors.
pub async fn open_upstream(
    http: &Http,
    p: &Prepared,
    timeout_s: f64,
) -> Result<reqwest::Response, ApiError> {
    let mut rb = http
        .client
        .post(&p.url)
        .body(serde_json::to_vec(&Value::Object(p.body.clone())).unwrap_or_default());
    for (k, v) in &p.headers {
        rb = rb.header(k, v);
    }
    let resp = match rb.timeout(Duration::from_secs_f64(timeout_s)).send().await {
        Ok(r) => r,
        Err(e) => {
            let class = HttpError::from_reqwest(&e).class;
            return Err(ApiError::new(
                502,
                "upstream_unreachable",
                redact(&format!("could not reach the system: {class}")),
            ));
        }
    };
    let status = resp.status().as_u16();
    if status >= 400 {
        let mut raw: Vec<u8> = Vec::new();
        let mut s = resp.bytes_stream();
        while let Some(Ok(chunk)) = s.next().await {
            raw.extend_from_slice(&chunk);
            if raw.len() >= MAX_ERROR_BYTES {
                break;
            }
        }
        raw.truncate(MAX_ERROR_BYTES);
        let text = redact(&String::from_utf8_lossy(&raw));
        let msg = format!("system answered {status}: {text}");
        return Err(ApiError::new(
            502,
            "upstream_error",
            char_prefix(&msg, 1200),
        ));
    }
    Ok(resp)
}

fn scrub(chunk: &[u8], secret: Option<&str>) -> Bytes {
    match secret {
        Some(s) if !s.is_empty() => {
            let needle = s.as_bytes();
            let mut out = Vec::with_capacity(chunk.len());
            let mut i = 0;
            while i < chunk.len() {
                if chunk[i..].starts_with(needle) {
                    out.extend_from_slice(b"[redacted]");
                    i += needle.len();
                } else {
                    out.push(chunk[i]);
                    i += 1;
                }
            }
            Bytes::from(out)
        }
        _ => Bytes::copy_from_slice(chunk),
    }
}

/// Relay the upstream body chunk by chunk, the secret value replaced.
pub fn relay_stream(
    resp: reqwest::Response,
    secret: Option<String>,
) -> impl Stream<Item = Bytes> + Send + 'static {
    async_stream::stream! {
        let mut s = resp.bytes_stream();
        while let Some(Ok(chunk)) = s.next().await {
            yield scrub(&chunk, secret.as_deref());
        }
    }
}

/// Read a non-streamed answer (capped at 4 MiB), the secret value replaced.
pub async fn read_capped(resp: reqwest::Response, secret: Option<&str>) -> Result<Bytes, ApiError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut s = resp.bytes_stream();
    while let Some(chunk) = s.next().await {
        let Ok(chunk) = chunk else { break };
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_NONSTREAM_BYTES {
            return Err(ApiError::new(
                502,
                "upstream_too_large",
                "upstream response too large",
            ));
        }
    }
    Ok(scrub(&buf, secret))
}
