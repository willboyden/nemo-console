//! Switchyard's `routes.toml` checks that need no binary: [`base_url_problem`], [`semantic_check`] and
//! [`check_routes`] (the size cap, the TOML load and the semantic pass; the backend adds `switchyard-server
//! --dry-run`). Moved unchanged from the backend's `adapters/switchyard.rs` (ADR-0022), which re-exports it; the
//! browser runs it through `crate::check`.

use std::sync::LazyLock;

use indexmap::IndexMap;

use crate::diagnostics::{Diagnostic, Severity};
use crate::services::pyfmt::str_repr;
use crate::services::pytoml::{self, Table, TomlVal};
use crate::services::pyutil::py_strip;

pub const MAX_CONFIG_BYTES: usize = 256 * 1024;

/// Compile a literal pattern (all patterns here are constants; a failure would be a typo caught by the tests).
pub fn re(pat: &str) -> regex::Regex {
    regex::Regex::new(pat).unwrap_or_else(|_| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"[^\s\S]").unwrap()
    })
}

static LINE_RE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"line (\d+)"));

static SERVICE_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9_-]*$"));

/// Python `urllib.parse.urlsplit` (+ the `port` / `hostname` properties), for [`base_url_problem`].
struct SplitUrl {
    scheme: String,
    netloc: String,
}

fn ip_v4(s: &str) -> Option<std::net::Ipv4Addr> {
    // ipaddress.IPv4Address: four decimal octets, no leading zeros, each <= 255
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut o = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if p.len() > 1 && p.starts_with('0') {
            return None;
        }
        o[i] = p.parse::<u8>().ok()?;
    }
    Some(std::net::Ipv4Addr::from(o))
}

/// `ipaddress.ip_address(s)` (a `%scope` suffix is allowed on IPv6, as in Python 3.9+).
pub fn ip_address(s: &str) -> Option<std::net::IpAddr> {
    if let Some(v4) = ip_v4(s) {
        return Some(std::net::IpAddr::V4(v4));
    }
    let core = match s.split_once('%') {
        Some((a, z)) if !z.is_empty() => a,
        Some(_) => return None,
        None => s,
    };
    core.parse::<std::net::Ipv6Addr>()
        .ok()
        .map(std::net::IpAddr::V6)
}

fn urlsplit(url: &str) -> Result<SplitUrl, String> {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = String::new();
    let mut rest = url.as_str();
    if let Some(i) = url.find(':')
        && i > 0
        && url.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && url[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        scheme = url[..i].to_ascii_lowercase();
        rest = &url[i + 1..];
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_owned();
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err("Invalid IPv6 URL".to_owned());
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    Ok(SplitUrl { scheme, netloc })
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    static FUT: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?s)^v[a-fA-F0-9]+\..+$"));
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err("Invalid IPv6 URL".to_owned());
            }
            let (h, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err("Invalid IPv6 URL".to_owned());
            }
            h
        }
        None => host_port.split_once(']').map_or(host_port, |(h, _)| h),
    };
    if hostname.starts_with('v') {
        if !FUT.is_match(hostname) {
            return Err("IPvFuture address is invalid".to_owned());
        }
        return Ok(());
    }
    match ip_address(hostname) {
        Some(std::net::IpAddr::V4(_)) => Err("An IPv4 address cannot be in brackets".to_owned()),
        Some(std::net::IpAddr::V6(_)) => Ok(()),
        None => Err(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            str_repr(hostname)
        )),
    }
}

impl SplitUrl {
    /// `_hostinfo`: (hostname, port) as raw strings.
    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, h)| h);
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (h, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (h, after.split_once(':').map_or("", |(_, p)| p))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (
            hostname.to_owned(),
            if port.is_empty() {
                None
            } else {
                Some(port.to_owned())
            },
        )
    }

    fn port(&self) -> Result<Option<u32>, String> {
        let (_, port) = self.hostinfo();
        let Some(p) = port else { return Ok(None) };
        if !(p.chars().all(char::is_numeric) && p.is_ascii()) {
            return Err(format!(
                "Port could not be cast to integer value as {}",
                str_repr(&p)
            ));
        }
        match p.parse::<u64>() {
            Ok(v) if v <= 65535 => Ok(Some(u32::try_from(v).unwrap_or(0))),
            _ => Err("Port out of range 0-65535".to_owned()),
        }
    }

    fn hostname(&self) -> Option<String> {
        let (h, _) = self.hostinfo();
        if h.is_empty() {
            return None;
        }
        Some(match h.split_once('%') {
            Some((a, z)) => format!("{}%{z}", a.to_lowercase()),
            None => h.to_lowercase(),
        })
    }
}

/// Why an llm_client base_url is not allowed, or None. Allowed hosts: loopback (127.0.0.0/8, ::1, localhost) and bare
/// single-label names (docker service names such as `litellm`). No userinfo.
#[must_use]
pub fn base_url_problem(value: Option<&TomlVal>) -> Option<String> {
    let Some(TomlVal::Str(value)) = value else {
        return Some("base_url is missing or not a string".to_owned());
    };
    let stripped = py_strip(value);
    if stripped.is_empty() {
        return Some("base_url is missing or not a string".to_owned());
    }
    let parts = match urlsplit(stripped) {
        Ok(p) => p,
        Err(e) => return Some(format!("base_url '{value}' does not parse: {e}")),
    };
    if let Err(e) = parts.port() {
        return Some(format!("base_url '{value}' does not parse: {e}"));
    }
    if parts.scheme != "http" && parts.scheme != "https" {
        return Some(format!("base_url '{value}' must use http or https"));
    }
    if parts.netloc.contains('@') {
        return Some("base_url must not contain userinfo (user:pass@host)".to_owned());
    }
    let host = parts.hostname().unwrap_or_default().to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
    if host.is_empty() {
        return Some(format!("base_url '{value}' has no host"));
    }
    if let Some(ip) = ip_address(&host) {
        let loopback = match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback(),
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|m| m.is_loopback())
            }
        };
        if loopback {
            return None;
        }
        return Some(format!(
            "base_url host {host} is not loopback; only loopback or an in-network service name is allowed"
        ));
    }
    if host == "localhost" || SERVICE_NAME_RE.is_match(&host) {
        return None;
    }
    Some(format!(
        "base_url host '{host}' is neither loopback nor a bare service name (no dots); external hosts need an \
         egress-allowlist change, not a routes.toml edit"
    ))
}

pub fn line_of(msg: &str) -> Option<i64> {
    LINE_RE.captures(msg).and_then(|c| c[1].parse::<i64>().ok())
}

pub fn diag(sev: Severity, message: impl Into<String>, line: Option<i64>) -> Diagnostic {
    Diagnostic {
        severity: sev,
        message: message.into(),
        line,
    }
}

/// Python `str(v)` of an optional TOML value (`None` for a missing key).
fn py_str_opt(v: Option<&TomlVal>) -> String {
    v.map_or_else(|| "None".to_owned(), TomlVal::py_str)
}

/// `SwitchyardAdapter.semantic_check(cfg)`.
#[must_use]
pub fn semantic_check(cfg: &Table) -> Vec<Diagnostic> {
    let empty = Table::new();
    let tbl = |k: &str| cfg.get(k).and_then(TomlVal::as_table).unwrap_or(&empty);
    let (clients, targets, routes) = (tbl("llm_clients"), tbl("targets"), tbl("routes"));
    let mut d = Vec::new();
    for (cname, c) in clients {
        if let TomlVal::Table(c) = c
            && let Some(why) = base_url_problem(c.get("base_url"))
        {
            d.push(diag(
                Severity::Error,
                format!("llm_client '{cname}': {why}"),
                None,
            ));
        }
    }
    let mut seen: IndexMap<(String, String), String> = IndexMap::new();
    for (name, t) in targets {
        let TomlVal::Table(t) = t else { continue };
        let client = t.get("llm_client");
        let known = matches!(client, Some(TomlVal::Str(s)) if clients.contains_key(s));
        if !known {
            d.push(diag(
                Severity::Error,
                format!(
                    "target '{name}' uses llm_client '{}' which is not defined under [llm_clients]",
                    py_str_opt(client)
                ),
                None,
            ));
        }
        let id = (py_str_opt(t.get("id")), py_str_opt(client));
        if let Some(prev) = seen.get(&id) {
            d.push(diag(
                Severity::Warn,
                format!(
                    "targets '{prev}' and '{name}' share (id '{}', llm_client '{}'): switchyard keeps only one and \
                     silently drops the other (with its system_prompt). Give one its own [llm_clients.*] block.",
                    id.0, id.1
                ),
                None,
            ));
        } else {
            seen.insert(id, name.clone());
        }
    }
    for (rname, r) in routes {
        let TomlVal::Table(r) = r else { continue };
        let mut refs: Vec<(String, &TomlVal)> = r
            .iter()
            .filter(|(k, _)| *k == "target" || k.ends_with("_target"))
            .map(|(k, v)| (k.clone(), v))
            .collect();
        if let Some(TomlVal::Table(cls)) = r.get("classifier")
            && let Some(t) = cls.get("target")
        {
            refs.push(("classifier.target".to_owned(), t));
        }
        for (field, rf) in refs {
            let ok = matches!(rf, TomlVal::Str(s) if targets.contains_key(s));
            if !ok {
                d.push(diag(
                    Severity::Error,
                    format!(
                        "route '{rname}': {field} = '{}' is not a defined [targets.*] entry",
                        rf.py_str()
                    ),
                    None,
                ));
            }
        }
        if let Some(id) = r.get("id")
            && !matches!(id, TomlVal::Str(s) if s == rname)
        {
            d.push(diag(
                Severity::Warn,
                format!(
                    "route table key '{rname}' differs from its id '{}' (clients see the id)",
                    id.py_str()
                ),
                None,
            ));
        }
    }
    if routes.is_empty() {
        d.push(diag(Severity::Error, "no [routes.*] defined", None));
    }
    d
}

/// `type(v).__name__`.
pub fn type_name(v: &TomlVal) -> &'static str {
    match v {
        TomlVal::Str(_) => "str",
        TomlVal::Int(_) | TomlVal::BigInt(_) => "int",
        TomlVal::Float(_) => "float",
        TomlVal::Bool(_) => "bool",
        TomlVal::Date(_) => "date",
        TomlVal::Time(_) => "time",
        TomlVal::DateTime(..) => "datetime",
        TomlVal::Array(_) => "list",
        TomlVal::Table(_) => "dict",
    }
}

/// What [`check_routes`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutesCheck {
    /// Refused before anything ran (too large): the validation result is these diagnostics alone.
    Refused(Vec<Diagnostic>),
    /// The TOML syntax error, or the semantic findings; the backend then adds the authoritative dry run.
    Checked(Vec<Diagnostic>),
}

/// The binary-free part of the Switchyard adapter's `validate_config`: the size cap, `tomllib.loads` and
/// [`semantic_check`].
#[must_use]
pub fn check_routes(content: &str) -> RoutesCheck {
    if content.len() > MAX_CONFIG_BYTES {
        return RoutesCheck::Refused(vec![diag(
            Severity::Error,
            format!("content exceeds {MAX_CONFIG_BYTES} bytes"),
            None,
        )]);
    }
    let mut diags = Vec::new();
    match pytoml::loads(content) {
        Ok(parsed) => diags.extend(semantic_check(&parsed)),
        Err(e) => {
            let line = line_of(&e.0);
            diags.push(diag(Severity::Error, format!("TOML syntax: {e}"), line));
        }
    }
    RoutesCheck::Checked(diags)
}
