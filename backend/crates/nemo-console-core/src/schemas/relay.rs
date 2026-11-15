//! The Relay plugin's `routes.toml` checks: [`validate_routes_semantics`] (a mirror of the Switchyard
//! schema_version-1 language) and [`check_routes`], the whole of the adapter's `validate_config` for that file
//! except its result envelope. Moved unchanged from the backend's `adapters/relay.rs` (ADR-0022), which re-exports
//! it; the browser runs it through `crate::check`.

use std::sync::LazyLock;

use indexmap::IndexMap;

use crate::adapter_error::AdapterError;
use crate::diagnostics::{Diagnostic, Severity};
use crate::schemas::switchyard::{re, type_name};
use crate::services::pyfmt::str_repr;
use crate::services::pytoml::{self, Table, TomlVal};
use crate::services::pyutil::py_splitlines;

static TOML_LINE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"line (\d+)"));

/// An environment-variable NAME (`api_key_env`).
static ENV_NAME: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[A-Z_][A-Z0-9_]*$"));

/// The size cap of every Relay config file the console reads or validates.
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// A literal `sk-` key (the adapter also masks it on read).
pub static SECRET_SK: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\bsk-[A-Za-z0-9_\-]{16,}"));
/// A literal Bearer token (the adapter also masks it on read).
pub static SECRET_BEARER: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)\bBearer[^\S\r\n]+[A-Za-z0-9._~+/=\-]{8,}"));

// ── routes.toml semantic validation (mirrors the Switchyard schema_version-1 language) ──────────

const CLIENT_FIELDS: [&str; 5] = [
    "format",
    "base_url",
    "api_key_env",
    "forward_auth",
    "max_retries",
];
const TARGET_FIELDS: [&str; 3] = ["id", "llm_client", "system_prompt"];
const ROUTE_FIELDS: [&str; 13] = [
    "id",
    "type",
    "capable_target",
    "efficient_target",
    "picker",
    "confidence_threshold",
    "recent_turn_window",
    "context_window",
    "tool_calling",
    "reasoning",
    "handoff_notes",
    "classifier",
    "target",
];
const CLASSIFIER_FIELDS: [&str; 3] = ["target", "base_threshold", "max_output_tokens"];
const PLUGIN_REJECTED: [&str; 2] = ["capable_system_prompt", "efficient_system_prompt"];
const FORMATS: [&str; 2] = ["anthropic_messages", "openai_chat"];
const ROUTE_TYPES: [&str; 2] = ["passthrough", "stage_router"];

static URL_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^https?://([^/?#]*)(?:[/?#][^\s\\]*)?$"));
static HOST_PORT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^([^:@\[\]]+)(?::(\d{1,5}))?$"));
static V6_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"^\[([0-9A-Fa-f:.]+)\](?::(\d{1,5}))?$"));
static SERVICE_LABEL: LazyLock<regex::Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9_-]*$"));

/// `ipaddress.IPv4Address(s)` (strict dotted decimal, no leading zeros).
fn ipv4(s: &str) -> Option<std::net::Ipv4Addr> {
    match crate::schemas::switchyard::ip_address(s) {
        Some(std::net::IpAddr::V4(v)) if !s.contains('%') => Some(v),
        _ => None,
    }
}

/// None when the URL may only reach this box (loopback or a bare single-label docker service name).
///
/// Rejects userinfo entirely (`http://127.0.0.1@evil`), whitespace/backslashes, non-loopback IPs and any dotted
/// hostname, so a routes.toml cannot send prompts or the LiteLLM credential off-box. One trailing dot is tolerated.
#[must_use]
pub fn base_url_problem(url: &str) -> Option<String> {
    if url.chars().any(|c| (c as u32) < 33 || (c as u32) > 126) || url.contains('\\') {
        return Some("contains whitespace, control or non-ASCII characters".into());
    }
    let Some(m) = URL_RE.captures(url) else {
        return Some("must be an http(s) URL".into());
    };
    let auth = m.get(1).map_or("", |g| g.as_str());
    if auth.contains('@') {
        return Some("must not contain userinfo (user@host)".into());
    }
    let (host_ok, port) = if let Some(v6) = V6_RE.captures(auth) {
        let Ok(a) = v6[1].parse::<std::net::Ipv6Addr>() else {
            return Some("has an invalid IPv6 address".into());
        };
        (a.is_loopback(), v6.get(2).map(|g| g.as_str().to_owned()))
    } else {
        let Some(hp) = HOST_PORT_RE.captures(auth) else {
            return Some("has an unparseable host".into());
        };
        let host = hp[1].to_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
        let ok = match ipv4(&host) {
            Some(a) => a.is_loopback(),
            None => host == "localhost" || SERVICE_LABEL.is_match(&host),
        };
        (ok, hp.get(2).map(|g| g.as_str().to_owned()))
    };
    if let Some(p) = port
        && p.parse::<u32>().unwrap_or(u32::MAX) > 65535
    {
        return Some("has an out-of-range port".into());
    }
    if !host_ok {
        return Some(
            "points off-box: only loopback (127.0.0.0/8, ::1, localhost) or a bare single-label service name (e.g. \
             litellm) is allowed, otherwise prompts and the LiteLLM credential would leave this machine"
                .into(),
        );
    }
    None
}

fn line_of(lines: &[&str], header: &str) -> Option<i64> {
    let pat = re(&format!(r"^\s*\[{}\]\s*(#.*)?$", regex::escape(header)));
    lines
        .iter()
        .position(|ln| pat.is_match(ln))
        .map(|i| i64::try_from(i + 1).unwrap_or(0))
}

/// `TypeError: unhashable type` for a value used as a set member / dict key.
fn hashable(v: Option<&TomlVal>) -> Result<(), AdapterError> {
    match v {
        Some(t @ (TomlVal::Array(_) | TomlVal::Table(_))) => Err(AdapterError::failed(
            "TypeError",
            format!("unhashable type: '{}'", type_name(t)),
        )),
        _ => Ok(()),
    }
}

fn repr_opt(v: Option<&TomlVal>) -> String {
    v.map_or_else(|| "None".to_owned(), TomlVal::repr)
}

fn is_int(v: &TomlVal) -> bool {
    matches!(v, TomlVal::Int(_) | TomlVal::BigInt(_))
}

/// Python `v == 1` for a TOML value (1, 1.0 and True all compare equal to 1).
fn eq_one(v: Option<&TomlVal>) -> bool {
    match v {
        Some(TomlVal::Int(i)) => *i == 1,
        Some(TomlVal::Float(f)) => *f == 1.0,
        Some(TomlVal::Bool(b)) => *b,
        _ => false,
    }
}

/// `validate_routes_semantics(text)`; an `Err` is an exception the Python lets escape (unhashable values).
pub fn validate_routes_semantics(text: &str) -> Result<Vec<Diagnostic>, AdapterError> {
    let lines = py_splitlines(text);
    let data = match pytoml::loads(text) {
        Ok(d) => d,
        Err(e) => {
            let line = TOML_LINE
                .captures(&e.0)
                .and_then(|c| c[1].parse::<i64>().ok());
            return Ok(vec![Diagnostic {
                severity: Severity::Error,
                message: format!("TOML syntax error: {e}"),
                line,
            }]);
        }
    };
    let mut diags: Vec<Diagnostic> = Vec::new();
    let push = |diags: &mut Vec<Diagnostic>, sev: Severity, msg: String, header: Option<&str>| {
        diags.push(Diagnostic {
            severity: sev,
            message: msg,
            line: header.and_then(|h| line_of(&lines, h)),
        });
    };
    if !eq_one(data.get("schema_version")) {
        push(
            &mut diags,
            Severity::Error,
            format!(
                "schema_version must be 1 (got {})",
                repr_opt(data.get("schema_version"))
            ),
            None,
        );
    }
    for k in data.keys() {
        if !["schema_version", "llm_clients", "targets", "routes"].contains(&k.as_str()) {
            push(
                &mut diags,
                Severity::Warn,
                format!(
                    "unknown top-level key {}; the plugin rejects unknown fields",
                    str_repr(k)
                ),
                None,
            );
        }
    }
    let table = |key: &str, diags: &mut Vec<Diagnostic>| -> IndexMap<String, Table> {
        match data.get(key) {
            None => IndexMap::new(),
            Some(TomlVal::Table(t)) if t.values().all(|x| matches!(x, TomlVal::Table(_))) => t
                .iter()
                .filter_map(|(k, v)| v.as_table().map(|t| (k.clone(), t.clone())))
                .collect(),
            Some(_) => {
                diags.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!("[{key}] must be a table of tables"),
                    line: None,
                });
                IndexMap::new()
            }
        }
    };
    let clients = table("llm_clients", &mut diags);
    let targets = table("targets", &mut diags);
    let routes = table("routes", &mut diags);
    if routes.is_empty() {
        push(
            &mut diags,
            Severity::Error,
            "no [routes.*] defined; the gateway would have nothing to route".into(),
            None,
        );
    }
    let fmts = "['anthropic_messages', 'openai_chat']";
    for (cname, c) in &clients {
        let h = format!("llm_clients.{cname}");
        hashable(c.get("format"))?;
        if !matches!(c.get("format"), Some(TomlVal::Str(f)) if FORMATS.contains(&f.as_str())) {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: format must be one of {fmts}"),
                Some(&h),
            );
        }
        match c.get("base_url") {
            Some(TomlVal::Str(bu)) => {
                if let Some(bad) = base_url_problem(bu) {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: base_url {bad}"),
                        Some(&h),
                    );
                }
            }
            _ => push(
                &mut diags,
                Severity::Error,
                format!("{h}: base_url must be an http(s) URL"),
                Some(&h),
            ),
        }
        if c.contains_key("api_key") || c.contains_key("apikey") {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: literal API keys are not allowed; use api_key_env = \"ENV_NAME\""),
                Some(&h),
            );
        }
        if let Some(ake) = c.get("api_key_env")
            && !matches!(ake, TomlVal::Str(s) if ENV_NAME.is_match(s))
        {
            push(
                &mut diags,
                Severity::Error,
                format!(
                    "{h}: api_key_env must be an environment variable NAME (e.g. LITELLM_MASTER_KEY), never a value"
                ),
                Some(&h),
            );
        }
        if let Some(mr) = c.get("max_retries") {
            let ok = match mr {
                TomlVal::Int(i) => *i >= 0,
                TomlVal::BigInt(s) => !s.starts_with('-'),
                _ => false,
            };
            if !ok {
                push(
                    &mut diags,
                    Severity::Error,
                    format!("{h}: max_retries must be a non-negative integer"),
                    Some(&h),
                );
            }
        }
        for k in c.keys() {
            if !CLIENT_FIELDS.contains(&k.as_str()) && k != "api_key" && k != "apikey" {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: unknown field {}; the plugin rejects unknown fields",
                        str_repr(k)
                    ),
                    Some(&h),
                );
            }
        }
    }
    let mut target_identity: IndexMap<(String, String), String> = IndexMap::new();
    for (tname, t) in &targets {
        let h = format!("targets.{tname}");
        let (tid, cl) = (t.get("id"), t.get("llm_client"));
        if !matches!(tid, Some(TomlVal::Str(s)) if !s.is_empty()) {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: id must be a non-empty string"),
                Some(&h),
            );
        }
        if !matches!(cl, Some(TomlVal::Str(s)) if clients.contains_key(s)) {
            push(
                &mut diags,
                Severity::Error,
                format!(
                    "{h}: llm_client {} is not defined under [llm_clients]",
                    repr_opt(cl)
                ),
                Some(&h),
            );
        }
        if let (Some(TomlVal::Str(tid)), Some(TomlVal::Str(cl))) = (tid, cl) {
            let key = (tid.clone(), cl.clone());
            if let Some(prev) = target_identity.get(&key) {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: same (id, llm_client) as targets.{prev}; the plugin silently DROPS one with only a WARN"
                    ),
                    Some(&h),
                );
            }
            target_identity.insert(key, tname.clone());
        }
        for k in t.keys() {
            if !TARGET_FIELDS.contains(&k.as_str()) {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: unknown field {}; the plugin rejects unknown fields",
                        str_repr(k)
                    ),
                    Some(&h),
                );
            }
        }
    }
    // fmt_of(tname)
    let fmt_of = |tname: &str| -> Result<Option<String>, AdapterError> {
        let Some(t) = targets.get(tname) else {
            return Ok(None);
        };
        let lc = t.get("llm_client");
        hashable(lc)?;
        let c = match lc {
            None => clients.get(""),
            Some(TomlVal::Str(s)) => clients.get(s),
            Some(_) => None,
        };
        Ok(match c.and_then(|c| c.get("format")) {
            Some(TomlVal::Str(f)) => Some(f.clone()),
            _ => None,
        })
    };
    let types = "['passthrough', 'stage_router']";
    let mut route_ids: IndexMap<String, String> = IndexMap::new();
    for (rname, r) in &routes {
        let h = format!("routes.{rname}");
        let (rid, rtype) = (r.get("id"), r.get("type"));
        match rid {
            Some(TomlVal::Str(s)) if !s.is_empty() => {
                if let Some(prev) = route_ids.get(s) {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: route id {} duplicates routes.{prev}", str_repr(s)),
                        Some(&h),
                    );
                } else {
                    route_ids.insert(s.clone(), rname.clone());
                }
            }
            _ => push(
                &mut diags,
                Severity::Error,
                format!("{h}: id must be a non-empty string"),
                Some(&h),
            ),
        }
        hashable(rtype)?;
        let rtype_s = match rtype {
            Some(TomlVal::Str(s)) => Some(s.as_str()),
            _ => None,
        };
        if !rtype_s.is_some_and(|t| ROUTE_TYPES.contains(&t)) {
            push(
                &mut diags,
                Severity::Error,
                format!("{h}: type must be one of {types}"),
                Some(&h),
            );
        }
        // Python iterates `_PLUGIN_REJECTED & set(r)` (set order); sorted here, see the workstream A deviations
        for bad in PLUGIN_REJECTED {
            if r.contains_key(bad) {
                push(
                    &mut diags,
                    Severity::Error,
                    format!(
                        "{h}: {bad} is rejected by the Relay plugin ('unknown field') although switchyard-server \
                         0.2.0 accepts it"
                    ),
                    Some(&h),
                );
            }
        }
        for k in r.keys() {
            if !ROUTE_FIELDS.contains(&k.as_str()) && !PLUGIN_REJECTED.contains(&k.as_str()) {
                push(
                    &mut diags,
                    Severity::Warn,
                    format!(
                        "{h}: unknown field {}; the plugin rejects unknown fields",
                        str_repr(k)
                    ),
                    Some(&h),
                );
            }
        }
        let mut used: Vec<String> = Vec::new();
        let need = |field: &str, diags: &mut Vec<Diagnostic>| -> Option<String> {
            let v = r.get(field);
            match v {
                Some(TomlVal::Str(s)) if targets.contains_key(s) => Some(s.clone()),
                _ => {
                    diags.push(Diagnostic {
                        severity: Severity::Error,
                        message: format!(
                            "{h}: {field} {} is not defined under [targets]",
                            repr_opt(v)
                        ),
                        line: line_of(&lines, &h),
                    });
                    None
                }
            }
        };
        if rtype_s == Some("passthrough") {
            if let Some(tv) = need("target", &mut diags) {
                used.push(tv);
            }
        } else if rtype_s == Some("stage_router") {
            for f in ["capable_target", "efficient_target"] {
                if let Some(tv) = need(f, &mut diags) {
                    used.push(tv);
                }
            }
            if let Some(thr) = r.get("confidence_threshold") {
                let ok = match thr {
                    TomlVal::Int(i) => (0..=1).contains(i),
                    TomlVal::Float(f) => (0.0..=1.0).contains(f),
                    _ => false,
                };
                if !ok {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: confidence_threshold must be a number between 0 and 1"),
                        Some(&h),
                    );
                }
            }
            for f in ["recent_turn_window", "context_window"] {
                if let Some(v) = r.get(f) {
                    let ok = match v {
                        TomlVal::Int(i) => *i > 0,
                        TomlVal::BigInt(s) => !s.starts_with('-'),
                        _ => false,
                    };
                    if !ok || !is_int(v) {
                        push(
                            &mut diags,
                            Severity::Error,
                            format!("{h}: {f} must be a positive integer"),
                            Some(&h),
                        );
                    }
                }
            }
            for f in ["tool_calling", "reasoning"] {
                if let Some(v) = r.get(f)
                    && !matches!(v, TomlVal::Bool(_))
                {
                    push(
                        &mut diags,
                        Severity::Error,
                        format!("{h}: {f} must be a boolean"),
                        Some(&h),
                    );
                }
            }
        }
        if let Some(cl) = r.get("classifier") {
            let ch = format!("routes.{rname}.classifier");
            match cl {
                TomlVal::Table(cl) => {
                    let tv = cl.get("target");
                    match tv {
                        Some(TomlVal::Str(s)) if targets.contains_key(s) => used.push(s.clone()),
                        _ => push(
                            &mut diags,
                            Severity::Error,
                            format!(
                                "{ch}: target {} is not defined under [targets]",
                                repr_opt(tv)
                            ),
                            Some(&ch),
                        ),
                    }
                    for k in cl.keys() {
                        if !CLASSIFIER_FIELDS.contains(&k.as_str()) {
                            push(
                                &mut diags,
                                Severity::Warn,
                                format!(
                                    "{ch}: unknown field {}; the plugin rejects unknown fields",
                                    str_repr(k)
                                ),
                                Some(&ch),
                            );
                        }
                    }
                }
                _ => push(
                    &mut diags,
                    Severity::Error,
                    format!("{ch} must be a table"),
                    Some(&ch),
                ),
            }
        }
        // Switchyard 0.3.0 plugin refuses a route that holds one model id on two wire formats.
        let mut by_model: IndexMap<String, Vec<String>> = IndexMap::new();
        for tn in &used {
            let fm = fmt_of(tn)?;
            let tid = targets.get(tn).and_then(|t| t.get("id"));
            if let (Some(fm), Some(TomlVal::Str(tid))) = (fm, tid) {
                let e = by_model.entry(tid.clone()).or_default();
                if !e.contains(&fm) {
                    e.push(fm);
                }
            }
        }
        for (tid, mut fs) in by_model {
            if fs.len() > 1 {
                fs.sort();
                push(
                    &mut diags,
                    Severity::Error,
                    format!(
                        "{h}: model id {} is used on two wire formats ({}); the 0.3.0 plugin refuses this. Give the \
                         second use its own alias (see the nemotron-lightning-judge alias)",
                        str_repr(&tid),
                        fs.join(", ")
                    ),
                    Some(&h),
                );
            }
        }
    }
    Ok(diags)
}

/// The Relay adapter's `validate_config` for `routes` without its result envelope: the size cap, the semantic
/// pass and the literal-secret check. An `Err` is an exception the Python lets escape (unhashable values).
pub fn check_routes(content: &str) -> Result<Vec<Diagnostic>, AdapterError> {
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Ok(vec![Diagnostic {
            severity: Severity::Error,
            message: "content exceeds 256 KiB".into(),
            line: None,
        }]);
    }
    let mut diags = validate_routes_semantics(content)?;
    if SECRET_SK.is_match(content) || SECRET_BEARER.is_match(content) {
        diags.push(Diagnostic {
            severity: Severity::Error,
            message:
                "content looks like it contains a literal secret; use api_key_env = \"ENV_NAME\""
                    .into(),
            line: None,
        });
    }
    Ok(diags)
}
