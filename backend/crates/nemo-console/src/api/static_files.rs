//! The frontend: a static ALLOWLIST (index.html + a few asset dirs), never dev/ or tests/. Unknown paths (including
//! /docs) are a JSON 404. Every file is served with `Cache-Control: no-cache` and Starlette's ETag
//! (`md5(f"{st_mtime}-{st_size}")`) so a browser revalidates and gets a 304. Port of `main.py` `_mount_frontend`.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, Response, StatusCode};
use md5::{Digest, Md5};

use crate::api::{Req, problem, unquote};
use crate::services::config_store::mtime_f64;
use crate::services::errors::ApiError;
use crate::services::pyfmt::float_repr;
use crate::services::settings::resolve_lenient;

pub const STATIC_FILES: [&str; 3] = ["index.html", "favicon.svg", "favicon.ico"];
pub const STATIC_DIRS: [&str; 6] = ["css", "js", "assets", "img", "fonts", "i18n"];
pub const STATIC_CACHE: &str = "no-cache";

/// Judged on the raw string BEFORE any resolve(): resolve() would collapse `..` into an allowlisted-looking path.
#[must_use]
pub fn unsafe_static_path(path: &str) -> bool {
    let once = unquote(path);
    let twice = unquote(&once);
    [path.to_owned(), once, twice].iter().any(|cand| {
        ["\0", "..", "//", "\\"]
            .iter()
            .any(|bad| cand.contains(bad))
            || cand.split('/').any(|seg| seg == ".")
    })
}

/// Python `mimetypes.guess_type` + Starlette's `; charset=utf-8` for text types, for the frontend's file kinds.
#[must_use]
pub fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn not_found(msg: &str) -> Response<Body> {
    problem(404, "not_found", msg)
}

pub async fn serve(req: Req) -> Result<Response<Body>, ApiError> {
    let path = req.param("path").to_owned();
    if unsafe_static_path(&path) {
        return Ok(not_found("no such file"));
    }
    let rel = if path.is_empty() {
        "index.html".to_owned()
    } else {
        path
    };
    let base_root = resolve_lenient(&req.state.cfg.frontend_dir);
    if !base_root.is_dir() {
        return Ok(not_found("frontend not installed"));
    }
    let top = rel.split('/').next().unwrap_or("");
    let listed = STATIC_FILES.contains(&rel.as_str());
    if listed || (STATIC_DIRS.contains(&top) && rel.contains('/')) {
        let target = resolve_lenient(&base_root.join(&rel));
        // every served file, the listed top-level ones (index.html, favicon.*) included, must resolve inside the
        // frontend dir: a symlink out of it is not served (audit finding 9)
        let root = if listed {
            base_root.clone()
        } else {
            resolve_lenient(&base_root.join(top))
        };
        let ok = target.is_file() && target.starts_with(&root);
        if ok {
            return Ok(file_response(&req, &target).await);
        }
    }
    Ok(not_found("no such file"))
}

/// `md5(f"{st_mtime}-{st_size}")` in double quotes.
#[must_use]
pub fn etag_for(md: &std::fs::Metadata) -> String {
    let base = format!("{}-{}", float_repr(mtime_f64(md).unwrap_or(0.0)), md.len());
    format!("\"{}\"", hex::encode(Md5::digest(base.as_bytes())))
}

/// The file is read on tokio's blocking pool (`tokio::fs`), not on the async worker (audit finding 9).
async fn file_response(req: &Req, target: &PathBuf) -> Response<Body> {
    let Ok(md) = tokio::fs::metadata(target).await else {
        return not_found("no such file");
    };
    let etag = etag_for(&md);
    let sent: Vec<String> = req
        .headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(|t| {
            let t = t.trim();
            t.strip_prefix("W/").unwrap_or(t).to_owned()
        })
        .collect();
    if sent.contains(&etag) {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        if let Ok(v) = HeaderValue::from_str(&etag) {
            r.headers_mut().insert("etag", v);
        }
        r.headers_mut()
            .insert("cache-control", HeaderValue::from_static(STATIC_CACHE));
        return r;
    }
    let Ok(data) = tokio::fs::read(target).await else {
        return not_found("no such file");
    };
    let size = data.len();
    let last_modified = md
        .modified()
        .map(httpdate::fmt_http_date)
        .unwrap_or_default();
    let ctype = content_type(target);
    let mut status = StatusCode::OK;
    let mut body = data;
    let mut content_range: Option<String> = None;
    let range = req.headers.get("range").and_then(|v| v.to_str().ok());
    let if_range = req.headers.get("if-range").and_then(|v| v.to_str().ok());
    if let Some(range) = range
        && if_range.is_none_or(|ir| ir == last_modified || ir == etag)
    {
        match parse_range(range, size) {
            Err(RangeErr::Malformed(msg)) => {
                let mut r = Response::new(Body::from(msg));
                *r.status_mut() = StatusCode::BAD_REQUEST;
                r.headers_mut().insert(
                    "content-type",
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                return r;
            }
            Err(RangeErr::NotSatisfiable) => {
                let mut r = Response::new(Body::empty());
                *r.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                r.headers_mut().insert(
                    "content-type",
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                    r.headers_mut().insert("content-range", v);
                }
                return r;
            }
            Ok(ranges) if ranges.len() == 1 => {
                let (s, e) = ranges[0];
                body = body[s..e].to_vec();
                status = StatusCode::PARTIAL_CONTENT;
                content_range = Some(format!("bytes {s}-{}/{size}", e - 1));
            }
            // none, or several (multipart/byteranges is PORTING.md deviation D-7): the whole file
            Ok(_) => {}
        }
    }
    let len = body.len();
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert("cache-control", HeaderValue::from_static(STATIC_CACHE));
    h.insert("content-type", HeaderValue::from_static(ctype));
    h.insert("accept-ranges", HeaderValue::from_static("bytes"));
    if let Ok(v) = HeaderValue::from_str(&len.to_string()) {
        h.insert("content-length", v);
    }
    if let Ok(v) = HeaderValue::from_str(&last_modified) {
        h.insert("last-modified", v);
    }
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert("etag", v);
    }
    if let Some(cr) = content_range
        && let Ok(v) = HeaderValue::from_str(&cr)
    {
        h.insert("content-range", v);
    }
    r
}

enum RangeErr {
    Malformed(&'static str),
    NotSatisfiable,
}

/// Starlette's `_parse_range_header` (single and multiple ranges; at most 100).
fn parse_range(header: &str, size: usize) -> Result<Vec<(usize, usize)>, RangeErr> {
    let Some((units, spec)) = header.split_once('=') else {
        return Err(RangeErr::Malformed("Malformed range header."));
    };
    if !units.trim().eq_ignore_ascii_case("bytes") {
        return Err(RangeErr::Malformed("Only support bytes range"));
    }
    if spec.matches(',').count() + 1 > 100 {
        return Ok(Vec::new());
    }
    let mut ranges = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() || part == "-" || !part.contains('-') {
            continue;
        }
        let (a, b) = part.split_once('-').unwrap_or((part, ""));
        let (a, b) = (a.trim(), b.trim());
        let start = if a.is_empty() {
            match b.parse::<usize>() {
                Ok(n) => size.saturating_sub(n),
                Err(_) => continue,
            }
        } else {
            match a.parse::<usize>() {
                Ok(n) => n,
                Err(_) => continue,
            }
        };
        let end = if !a.is_empty() && !b.is_empty() {
            match b.parse::<usize>() {
                Ok(n) if n < size => n + 1,
                Ok(_) => size,
                Err(_) => continue,
            }
        } else {
            size
        };
        ranges.push((start, end));
    }
    if ranges.is_empty() {
        return Err(RangeErr::Malformed("Range header: range must be requested"));
    }
    if ranges.iter().any(|(s, _)| *s >= size) {
        return Err(RangeErr::NotSatisfiable);
    }
    if ranges.iter().any(|(s, e)| s >= e) {
        return Err(RangeErr::Malformed(
            "Range header: start must be less than end",
        ));
    }
    Ok(ranges)
}
