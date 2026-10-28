//! Runtime settings view/overrides. Only URLs (loopback), the poll interval and credential env-var NAMES are mutable.
//! Port of `services/settings.py`. Contract: the overrides live in `settings(k='overrides')` as Python `json.dumps`
//! text; saved params in `settings(k='params:<system>')`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::config::{Settings, VERSION, credential_env_allowed, home_dir};
use crate::services::errors::ApiError;
use crate::services::pyfmt::{char_len, char_prefix, json_dumps, list_repr, str_repr};
use crate::services::store::Store;

#[allow(clippy::expect_used)]
static URL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-z][a-z0-9_]{0,31}\z").expect("static regex"));
pub const OVERRIDES_KEY: &str = "overrides";

#[must_use]
pub fn key_env_allowed(name: &str) -> bool {
    credential_env_allowed(name)
}

/// `urllib.parse.urlsplit` subset for http URLs: (scheme, username?, password?, host, port, query?, fragment?).
struct Split {
    scheme: String,
    has_user: bool,
    has_password: bool,
    host: String,
    port: Option<Result<i64, ()>>,
    has_query: bool,
    has_fragment: bool,
}

fn urlsplit(url: &str) -> Result<Split, ()> {
    let (scheme, rest) = match url.split_once(':') {
        Some((s, r))
            if !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            (s.to_ascii_lowercase(), r)
        }
        _ => (String::new(), url),
    };
    let (netloc, after) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let (path_q, fragment) = match after.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (after, None),
    };
    let query = path_q.split_once('?').map(|(_, q)| q);
    // urllib.parse: an unbalanced bracket is ValueError("Invalid IPv6 URL"); a bracketed host must be IPv6
    if netloc.contains('[') != netloc.contains(']') {
        return Err(());
    }
    let (userinfo, hostport) = match netloc.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, netloc),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((a, b)) => (Some(a), Some(b)),
            None => (Some(u), None),
        },
        None => (None, None),
    };
    // `_hostinfo`: partition on '[' / ']' for a bracketed host, else on the FIRST ':'
    let (host, port_str) = match hostport.split_once('[') {
        Some((_, bracketed)) => {
            let (h, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if h.parse::<std::net::Ipv6Addr>().is_err() && !h.starts_with(['v', 'V']) {
                return Err(());
            }
            (h.to_owned(), rest.split_once(':').map_or("", |(_, p)| p))
        }
        None => match hostport.split_once(':') {
            Some((h, p)) => (h.to_owned(), p),
            None => (hostport.to_owned(), ""),
        },
    };
    // `.port`: digits only (ASCII), 0..=65535, else ValueError
    let port = if port_str.is_empty() {
        None
    } else if port_str.chars().all(|c| c.is_ascii_digit()) {
        match port_str.parse::<u64>() {
            Ok(p) if p <= 65535 => Some(Ok(i64::try_from(p).unwrap_or(0))),
            _ => Some(Err(())),
        }
    } else {
        Some(Err(()))
    };
    Ok(Split {
        scheme,
        has_user: user.is_some_and(|u| !u.is_empty()),
        has_password: password.is_some_and(|p| !p.is_empty()),
        host: host.to_lowercase(),
        port,
        has_query: query.is_some_and(|q| !q.is_empty()),
        has_fragment: fragment.is_some_and(|f| !f.is_empty()),
    })
}

/// Validate a loopback http URL; returns it without a trailing slash.
pub fn validate_url(url: &str, allowed_hosts: &[String]) -> Result<String, ApiError> {
    let Ok(u) = urlsplit(url) else {
        return Err(ApiError::new(422, "invalid_url", "unparseable url"));
    };
    let port = match u.port {
        Some(Err(())) => return Err(ApiError::new(422, "invalid_url", "unparseable url")),
        Some(Ok(p)) => Some(p),
        None => None,
    };
    if u.scheme != "http" || u.has_user || u.has_password || u.has_query || u.has_fragment {
        return Err(ApiError::new(
            422,
            "invalid_url",
            "urls must be plain http://host:port[/path] with no credentials",
        ));
    }
    if !allowed_hosts.iter().any(|h| h.to_lowercase() == u.host) {
        let mut sorted: Vec<String> = allowed_hosts.to_vec();
        sorted.sort();
        return Err(ApiError::new(
            422,
            "invalid_url",
            format!("host must be one of {} (loopback only)", list_repr(&sorted)),
        ));
    }
    if let Some(p) = port
        && !(1..=65535).contains(&p)
    {
        return Err(ApiError::new(422, "invalid_url", "bad port"));
    }
    // audit finding 6: the parser the HTTP client really uses (`url`, WHATWG) must reach the same place. It
    // normalises some hosts the Python-style split keeps verbatim (`127.1` is 127.0.0.1), so its host has to be on
    // the allowlist too, with the same port and no credentials, query or fragment.
    if !url_crate_agrees(url, allowed_hosts, port.unwrap_or(80)) {
        let mut sorted: Vec<String> = allowed_hosts.to_vec();
        sorted.sort();
        return Err(ApiError::new(
            422,
            "invalid_url",
            format!("host must be one of {} (loopback only)", list_repr(&sorted)),
        ));
    }
    Ok(url.trim_end_matches('/').to_owned())
}

/// `url::Url` reads `url` as plain http to an allowlisted host on `port`, with no credentials, query or fragment
/// (an empty `?` / `#`, which Python's check lets through, carries nothing and is accepted).
fn url_crate_agrees(url: &str, allowed_hosts: &[String], port: i64) -> bool {
    let Ok(p) = url::Url::parse(url) else {
        return false;
    };
    let host = p
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();
    p.scheme() == "http"
        && p.username().is_empty()
        && p.password().is_none()
        && p.query().is_none_or(str::is_empty)
        && p.fragment().is_none_or(str::is_empty)
        && allowed_hosts.iter().any(|h| h.to_lowercase() == host)
        && p.port_or_known_default().map(i64::from) == Some(port)
}

pub struct SettingsService {
    pub cfg: Arc<Settings>,
    pub store: Arc<Store>,
    pub environ: crate::services::environ::Environ,
    restart_required: AtomicBool,
}

impl SettingsService {
    #[must_use]
    pub fn new(
        cfg: Arc<Settings>,
        store: Arc<Store>,
        environ: crate::services::environ::Environ,
    ) -> Self {
        SettingsService {
            cfg,
            store,
            environ,
            restart_required: AtomicBool::new(false),
        }
    }

    fn read_key(&self, k: &str) -> Option<String> {
        self.store
            .with(|db| {
                let mut s = db.prepare("SELECT v FROM settings WHERE k=?")?;
                let mut rows = s.query([k])?;
                match rows.next()? {
                    Some(r) => r.get::<_, String>(0).map(Some),
                    None => Ok(None),
                }
            })
            .ok()
            .flatten()
    }

    #[must_use]
    pub fn overrides(&self) -> Map<String, Value> {
        match self
            .read_key(OVERRIDES_KEY)
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    #[must_use]
    pub fn effective_urls(&self) -> IndexMap<String, String> {
        let mut urls = self.cfg.urls.clone();
        if let Some(Value::Object(o)) = self.overrides().get("urls") {
            for (k, v) in o {
                if let Some(s) = v.as_str()
                    && let Ok(u) = validate_url(s, &self.cfg.allowed_url_hosts)
                {
                    urls.insert(k.clone(), u);
                }
            }
        }
        urls
    }

    #[must_use]
    pub fn effective_key_env(&self) -> IndexMap<String, String> {
        let mut env = self.cfg.playground_key_env.clone();
        if let Some(Value::Object(o)) = self.overrides().get("playground_key_env") {
            for (k, v) in o {
                if let Some(s) = v.as_str()
                    && key_env_allowed(s)
                {
                    env.insert(k.clone(), s.to_owned());
                }
            }
        }
        env
    }

    #[must_use]
    pub fn effective_poll_interval(&self) -> f64 {
        match self.overrides().get("poll_interval_s") {
            Some(Value::Number(n)) => match n.as_f64() {
                Some(v) if (0.5..=60.0).contains(&v) => v,
                _ => self.cfg.poll_interval_s,
            },
            _ => self.cfg.poll_interval_s,
        }
    }

    #[must_use]
    pub fn token_status(&self) -> Map<String, Value> {
        let mut pairs: Vec<(String, String)> = self.effective_key_env().into_iter().collect();
        pairs.sort();
        let mut out = Map::new();
        for (logical, name) in pairs {
            let status = match self.environ.get(&name) {
                Some(v) if !v.is_empty() => format!("[set, {} chars]", char_len(&v)),
                _ => "[unset]".to_owned(),
            };
            out.insert(logical, Value::String(status));
        }
        out
    }

    fn state_dir_display(&self, admin: bool) -> String {
        let sd = &self.cfg.state_dir;
        if admin {
            return sd.display().to_string();
        }
        let resolved = resolve_lenient(sd);
        // Python `Path.home()` reads $HOME: take it from the (pinnable) environment
        let home = self
            .environ
            .get("HOME")
            .filter(|h| !h.is_empty())
            .map_or_else(home_dir, std::path::PathBuf::from);
        let home = resolve_lenient(&home);
        match resolved.strip_prefix(&home) {
            Ok(rel) => format!(
                "~/{}",
                if rel.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    rel.display().to_string()
                }
            ),
            Err(_) => "<state dir>".to_owned(),
        }
    }

    #[must_use]
    pub fn view(&self, admin: bool) -> Value {
        json!({
            "version": VERSION,
            "urls": self.effective_urls(),
            "state_dir": self.state_dir_display(admin),
            "poll_interval_s": self.effective_poll_interval(),
            "ring_seconds": self.cfg.ring_seconds,
            "playground_key_env": self.effective_key_env(),
            "tokens": self.token_status(),
            "trust_loopback": self.cfg.effective_loopback_trust().0,
            "restart_required": self.restart_required.load(Ordering::SeqCst),
        })
    }

    /// PUT /settings. `poll_interval_s` is already a float (pydantic turns 7 into 7.0 before this runs).
    pub fn update(
        &self,
        urls: Option<&IndexMap<String, String>>,
        poll_interval_s: Option<f64>,
        key_env: Option<&IndexMap<String, String>>,
    ) -> Result<(Value, Option<f64>), ApiError> {
        let mut ov = self.overrides();
        if let Some(urls) = urls {
            let mut merged = match ov.get("urls") {
                Some(Value::Object(m)) => m.clone(),
                _ => Map::new(),
            };
            for (k, v) in urls {
                if !self.cfg.urls.contains_key(k) || !URL_NAME.is_match(k) {
                    return Err(ApiError::new(
                        422,
                        "invalid_setting",
                        format!("unknown url name {}", str_repr(char_prefix(k, 40))),
                    ));
                }
                merged.insert(
                    k.clone(),
                    Value::String(validate_url(v, &self.cfg.allowed_url_hosts)?),
                );
            }
            ov.insert("urls".to_owned(), Value::Object(merged));
            self.restart_required.store(true, Ordering::SeqCst);
        }
        if let Some(key_env) = key_env {
            let mut merged = match ov.get("playground_key_env") {
                Some(Value::Object(m)) => m.clone(),
                _ => Map::new(),
            };
            for (k, v) in key_env {
                if !URL_NAME.is_match(k) || !key_env_allowed(v) {
                    return Err(ApiError::new(
                        422,
                        "invalid_setting",
                        "credential settings take an ENV VAR NAME (e.g. LITELLM_API_KEY), never a value; allowed: LITELLM_*, NEMO_CONSOLE_PLAYGROUND_*, *_API_KEY",
                    ));
                }
                merged.insert(k.clone(), Value::String(v.clone()));
            }
            ov.insert("playground_key_env".to_owned(), Value::Object(merged));
        }
        let mut interval = None;
        if let Some(f) = poll_interval_s {
            if !(0.5..=60.0).contains(&f) {
                return Err(ApiError::new(
                    422,
                    "invalid_setting",
                    "poll_interval_s must be between 0.5 and 60",
                ));
            }
            ov.insert("poll_interval_s".to_owned(), json!(f));
            interval = Some(f);
        }
        self.store
            .execute(
                "INSERT INTO settings(k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                rusqlite::params![OVERRIDES_KEY, json_dumps(&Value::Object(ov))],
            )
            .map_err(|_| ApiError::internal())?;
        Ok((self.view(true), interval))
    }

    #[must_use]
    pub fn saved_params(&self, system: &str) -> Map<String, Value> {
        match self
            .read_key(&format!("params:{system}"))
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    pub fn save_params(&self, system: &str, values: &Map<String, Value>) -> Result<(), ApiError> {
        self.store
            .execute(
                "INSERT INTO settings(k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                rusqlite::params![format!("params:{system}"), json_dumps(&Value::Object(values.clone()))],
            )
            .map(|_| ())
            .map_err(|_| ApiError::internal())
    }
}

/// `Path.resolve()` (strict=False): canonicalise the longest existing prefix, keep the rest.
#[must_use]
pub fn resolve_lenient(p: &Path) -> std::path::PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let mut tail = Vec::new();
    let mut cur = p.to_path_buf();
    while let Some(name) = cur.file_name().map(std::ffi::OsStr::to_owned) {
        cur.pop();
        tail.push(name);
        if let Ok(c) = std::fs::canonicalize(&cur) {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {

    /// Audit finding 6: a host the Python-style split accepts verbatim but the HTTP client's parser normalises to
    /// another address (`127.1` -> 127.0.0.1) is refused; ordinary loopback URLs still pass with Python's results.
    #[test]
    fn the_http_clients_parser_must_agree() {
        let allow = |h: &[&str]| h.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert!(validate_url("http://127.1:4010", &allow(&["127.1"])).is_err());
        assert_eq!(
            validate_url("http://127.0.0.1:4010/", &allow(&["127.0.0.1"]))
                .ok()
                .as_deref(),
            Some("http://127.0.0.1:4010")
        );
        assert!(validate_url("http://[::1]:4010", &allow(&["::1"])).is_ok());
        assert!(validate_url("http://LOCALHOST:4010/v1", &allow(&["localhost"])).is_ok());
        assert!(validate_url("http://127.0.0.1:4010#", &allow(&["127.0.0.1"])).is_ok());
    }

    use super::validate_url;

    /// CPython 3.13 `validate_url` answers (backend venv, 2026-10-27).
    #[test]
    fn validate_url_matches_urlsplit_edge_cases() {
        let hosts: Vec<String> = ["127.0.0.1", "localhost", "::1"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let msg = |u: &str| match validate_url(u, &hosts) {
            Ok(v) => format!("ok {v}"),
            Err(e) => e.message,
        };
        for (u, want) in [
            ("http://127.0.0.1:70000", "unparseable url"),
            ("http://127.0.0.1:", "ok http://127.0.0.1:"),
            ("http://127.0.0.1:0", "bad port"),
            ("http://127.0.0.1:-1", "unparseable url"),
            ("http://127.0.0.1:abc", "unparseable url"),
            ("http://[::1]:4000", "ok http://[::1]:4000"),
            ("http://LOCALHOST:1/x/", "ok http://LOCALHOST:1/x"),
            (
                "http://127.0.0.1:1?x",
                "urls must be plain http://host:port[/path] with no credentials",
            ),
            ("http://127.0.0.1:1#", "ok http://127.0.0.1:1#"),
            (
                "http:127.0.0.1:1",
                "host must be one of ['127.0.0.1', '::1', 'localhost'] (loopback only)",
            ),
            ("HTTP://127.0.0.1:1", "ok HTTP://127.0.0.1:1"),
            ("http://127.0.0.1:\u{661}\u{662}", "unparseable url"),
            ("http://[::1", "unparseable url"),
            ("http://127.0.0.1:1:2", "unparseable url"),
            (
                "http://user:pw@127.0.0.1:1",
                "urls must be plain http://host:port[/path] with no credentials",
            ),
        ] {
            assert_eq!(msg(u), want, "{u}");
        }
    }
}
