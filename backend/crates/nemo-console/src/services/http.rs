//! The console's HTTP client (Python: one shared `httpx.AsyncClient(trust_env=False, follow_redirects=False,
//! timeout=Timeout(10.0, connect=3.0))`). Contract (PORTING.md section 6.4).
//!
//! Errors carry the httpx exception CLASS NAME (`ConnectError`, `ConnectTimeout`, `ReadTimeout`, ...) because the
//! Python adapters and core print `type(e).__name__` in problems and messages.

use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;

/// A transport failure, named like the httpx exception it replaces.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HttpError {
    /// `ConnectError` | `ConnectTimeout` | `ReadTimeout` | `WriteTimeout` | `RemoteProtocolError` | `ReadError` |
    /// `UnsupportedProtocol` | `InvalidURL` | `HTTPError`.
    pub class: &'static str,
    pub message: String,
}

impl HttpError {
    /// `isinstance(e, httpx.TimeoutException)`.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(
            self.class,
            "ConnectTimeout" | "ReadTimeout" | "WriteTimeout" | "PoolTimeout"
        )
    }

    #[must_use]
    pub fn from_reqwest(e: &reqwest::Error) -> HttpError {
        let class = if e.is_timeout() {
            if e.is_connect() {
                "ConnectTimeout"
            } else {
                "ReadTimeout"
            }
        } else if e.is_connect() {
            "ConnectError"
        } else if e.is_builder() {
            "UnsupportedProtocol"
        } else if e.is_body() || e.is_decode() {
            "ReadError"
        } else if e.is_request() {
            "RemoteProtocolError"
        } else {
            "HTTPError"
        };
        HttpError {
            class,
            message: e.to_string(),
        }
    }
}

/// A fully read response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: http::HeaderMap,
    pub body: Bytes,
}

impl HttpResponse {
    /// `resp.json()`: `Err` is Python's ValueError (not JSON).
    pub fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// `resp.text` (UTF-8 with replacement).
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Case-insensitive header lookup.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// The shared client. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Http {
    pub client: reqwest::Client,
}

impl Http {
    /// trust_env off (no proxies from the environment), redirects never followed, connect timeout 3 s, default
    /// request timeout 10 s.
    pub fn new() -> Result<Http, HttpError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| HttpError::from_reqwest(&e))?;
        Ok(Http { client })
    }

    /// GET with a per-request timeout (seconds), body read in full.
    pub async fn get(&self, url: &str, timeout_s: f64) -> Result<HttpResponse, HttpError> {
        self.send(self.client.get(url), timeout_s).await
    }

    /// GET with query parameters.
    pub async fn get_query(
        &self,
        url: &str,
        query: &[(&str, &str)],
        timeout_s: f64,
    ) -> Result<HttpResponse, HttpError> {
        self.send(self.client.get(url).query(query), timeout_s)
            .await
    }

    /// POST a JSON body (`json=`), with extra headers.
    pub async fn post_json(
        &self,
        url: &str,
        body: &Value,
        headers: &[(&str, &str)],
        timeout_s: f64,
    ) -> Result<HttpResponse, HttpError> {
        let mut rb = self.client.post(url).json(body);
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        self.send(rb, timeout_s).await
    }

    /// POST with no body.
    pub async fn post_empty(&self, url: &str, timeout_s: f64) -> Result<HttpResponse, HttpError> {
        self.send(self.client.post(url), timeout_s).await
    }

    /// Send any request builder from `self.client` and read the whole body.
    pub async fn send(
        &self,
        rb: reqwest::RequestBuilder,
        timeout_s: f64,
    ) -> Result<HttpResponse, HttpError> {
        let resp = rb
            .timeout(Duration::from_secs_f64(timeout_s.max(0.001)))
            .send()
            .await
            .map_err(|e| HttpError::from_reqwest(&e))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| HttpError::from_reqwest(&e))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
