//! Redaction, secret detection and small helpers shared by the whole core. Port of `services/common.py`.
//!
//! Stable API (PORTING.md section 6.7): [`redact`], [`redact_keep_env`], [`redact_obj`], [`find_secret`],
//! [`key_is_sensitive`], [`sha256_hex`], [`cap`], [`cap_default`], [`KNOWN`]. The patterns are the Python ones
//! verbatim (fancy-regex for the look-arounds); every regex runs on chunks of at most [`CHUNK`] code points so the
//! whole scan stays linear, exactly like the Python version.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use fancy_regex::{Captures, Regex};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::services::pyfmt::{char_len, char_prefix};

pub const REDACTED: &str = "[redacted]";

/// Audit finding 3: a std `Mutex` whose holder panicked is recovered, not abandoned. Python has no lock poisoning, so
/// one panic must not disable a structure (the key store: every request 401) for the rest of the process.
/// `lock_recovering` keeps the `LockResult` shape of `lock()` (the call sites stay as they are) but is always `Ok`.
pub trait RecoverLock<T> {
    fn lock_recovering(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>>;
}

impl<T> RecoverLock<T> for Mutex<T> {
    fn lock_recovering(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>> {
        Ok(self
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

/// The text of a panic payload (`&str` or `String`; anything else is "panic").
#[must_use]
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_owned())
}

/// Run one unit of background work (a poller tick, a probe pass, a tool run) so that a panic is contained like
/// Python's `except Exception: log.exception(...)`: caught, logged once with the message redacted, and the task
/// carries on (`None`) instead of dying silently (audit finding 3).
pub async fn contained<T>(what: &str, fut: impl std::future::Future<Output = T>) -> Option<T> {
    use futures::FutureExt as _;
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(v) => Some(v),
        Err(p) => {
            tracing::error!("{what} failed: {}", redact(&panic_text(p.as_ref())));
            None
        }
    }
}
/// Every regex runs on at most this many characters: linear overall, never quadratic.
pub const CHUNK: usize = 4000;

#[allow(clippy::expect_used)] // static patterns: a typo is a programming error caught by every test run
fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)")
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(Bearer)\s+[A-Za-z0-9._~+/=\-]{8,}"));
static PREFIXED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?<![A-Za-z0-9])(?:sk-|nck_|hf_|ghp_|gho_|xox[bap]-|AKIA)[A-Za-z0-9_\-]{8,}")
});
static JWT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*"));
/// key = value / key: value / "key": "value" where the key NAME ends in a secret-ish word. See the Python comment:
/// the unquoted alternative never swallows the closing quote of an enclosing JSON string.
static KEYVAL: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"(?i)(?P<pre>(?<![A-Za-z0-9])[\w.\-]{0,40}?(?:api[_\-]?key|secret|token|passw(?:or)?d|authorization)["']?\s*[=:]\s*)"#,
        r#"(?P<val>"[^"\n]*"|'[^'\n]*'|(?:[^\s,;#}\]"']|["'](?![,;\]}\s]|$))+)"#
    ))
});
static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"\A[A-Z_][A-Z0-9_]{2,}\z"));
static ENV_REF: LazyLock<Regex> =
    LazyLock::new(|| re(r"\A(?:os\.environ/|env:|\$\{?)[A-Za-z_][A-Za-z0-9_]*\}?\z"));
static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:^|[_\-])(?:api[_\-]?key|secret|token|password|passwd|authorization|credential|cookie|bearer)(?=\n?\z)",
    )
});
static USERINFO: LazyLock<Regex> = LazyLock::new(|| re(r"(?<=://)[^/\s:@]{1,64}:[^/\s@]{1,128}@"));

/// Exact secret VALUES (bootstrap key, playground credential, probe key) that must never appear in any output.
pub struct Known {
    values: Mutex<HashSet<String>>,
}

impl Known {
    /// Register a value (ignored when shorter than 6 characters, like Python).
    pub fn add(&self, value: &str) {
        if char_len(value) >= 6
            && let Ok(mut v) = self.values.lock_recovering()
        {
            v.insert(value.to_owned());
        }
    }

    /// Replace every registered value, longest first.
    #[must_use]
    pub fn scrub(&self, text: &str) -> String {
        let mut values: Vec<String> = match self.values.lock_recovering() {
            Ok(v) => v.iter().cloned().collect(),
            Err(_) => Vec::new(),
        };
        values.sort_by_key(|v| std::cmp::Reverse(char_len(v)));
        let mut out = text.to_owned();
        for v in values {
            if out.contains(&v) {
                out = out.replace(&v, REDACTED);
            }
        }
        out
    }

    /// Test seam (Python tests clear `KNOWN._values`).
    pub fn clear(&self) {
        if let Ok(mut v) = self.values.lock_recovering() {
            v.clear();
        }
    }
}

pub static KNOWN: LazyLock<Known> = LazyLock::new(|| Known {
    values: Mutex::new(HashSet::new()),
});

fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'') {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

fn is_env_reference(value: &str) -> bool {
    let v = unquote(value);
    v.is_empty() || ENV_NAME.is_match(v).unwrap_or(false) || ENV_REF.is_match(v).unwrap_or(false)
}

/// Split on whitespace near the chunk edge so a token is very unlikely to straddle two chunks (code points).
fn chunks(text: &str) -> Vec<&str> {
    let offs: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let n = offs.len();
    let at = |ci: usize| if ci >= n { text.len() } else { offs[ci] };
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < n {
        let mut j = (i + CHUNK).min(n);
        if j < n {
            // max(text.rfind(c, i + CHUNK - 200, j) for c in " \n\t"); rfind returns -1 when absent
            let lo = i + CHUNK - 200;
            let mut k: Option<usize> = None;
            let mut idx = j;
            while idx > lo {
                idx -= 1;
                if matches!(chars[idx], ' ' | '\n' | '\t') {
                    k = Some(idx);
                    break;
                }
            }
            if let Some(k) = k
                && k > i
            {
                j = k + 1;
            }
        }
        out.push(&text[at(i)..at(j)]);
        i = j;
    }
    out
}

/// `re.sub(pattern, repl, text)` with a closure.
/// A regex test that could not complete (fancy-regex's backtrack limit) counts as a MATCH (audit finding 5): the
/// secret scan refuses and the key check treats the key as sensitive, never "no secret".
fn hit(r: Result<bool, fancy_regex::Error>) -> bool {
    r.unwrap_or(true)
}

/// What [`find_secret`] says when a pattern could not finish on the content: refuse rather than pass it.
pub const SCAN_INCOMPLETE: &str =
    "content the secret scan could not finish checking (shorten or simplify it)";

fn sub_with(rx: &Regex, text: &str, mut f: impl FnMut(&Captures<'_, str>) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in rx.captures_iter(text) {
        // a match that could not finish: the whole chunk is redacted (fail closed, audit finding 5)
        let Ok(caps) = caps else {
            return REDACTED.to_owned();
        };
        let Some(m) = caps.get(0) else { continue };
        out.push_str(&text[last..m.start()]);
        out.push_str(&f(&caps));
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Exact prefilters for the look-around patterns (which run fancy-regex's backtracking VM): every possible match
/// of the pattern contains what the prefilter looks for, so skipping a text without it changes nothing. They keep
/// the scan cheap on long secret-free output (Python's `re` needs no help there; an unoptimised Rust build does).
#[allow(clippy::expect_used)]
static KEYVAL_HINT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r#"(?i)(?:api[_\-]?key|secret|token|passw(?:or)?d|authorization)["']?\s*[=:]"#,
    )
    .expect("static regex")
});
const SECRET_PREFIXES: [&str; 9] = [
    "sk-", "nck_", "hf_", "ghp_", "gho_", "xoxb-", "xoxa-", "xoxp-", "AKIA",
];

fn may_keyval(t: &str) -> bool {
    KEYVAL_HINT.is_match(t)
}

fn may_prefixed(t: &str) -> bool {
    SECRET_PREFIXES.iter().any(|p| t.contains(p))
}

fn redact_chunk(text: &str, keep_env_names: bool) -> String {
    let t = if text.contains("://") {
        sub_with(&USERINFO, text, |_| format!("{REDACTED}@"))
    } else {
        text.to_owned()
    };
    let t = sub_with(&PRIVATE_KEY, &t, |_| REDACTED.to_owned());
    let t = sub_with(&BEARER, &t, |c| {
        format!("{} {REDACTED}", c.get(1).map_or("", |m| m.as_str()))
    });
    let t = if may_prefixed(&t) {
        sub_with(&PREFIXED, &t, |_| REDACTED.to_owned())
    } else {
        t
    };
    let t = sub_with(&JWT, &t, |_| REDACTED.to_owned());
    if !may_keyval(&t) {
        return t;
    }
    sub_with(&KEYVAL, &t, |c| {
        let val = c.name("val").map_or("", |m| m.as_str());
        if keep_env_names && is_env_reference(val) {
            return c.get(0).map_or("", |m| m.as_str()).to_owned();
        }
        format!("{}{REDACTED}", c.name("pre").map_or("", |m| m.as_str()))
    })
}

fn redact_impl(text: &str, keep_env_names: bool) -> String {
    let text = KNOWN.scrub(text);
    if char_len(&text) <= CHUNK {
        return redact_chunk(&text, keep_env_names);
    }
    chunks(&text)
        .into_iter()
        .map(|c| redact_chunk(c, keep_env_names))
        .collect()
}

/// Scrub secret-looking substrings and any registered exact secret value from text (linear time).
#[must_use]
pub fn redact(text: &str) -> String {
    redact_impl(text, false)
}

/// `redact(text, keep_env_names=True)`: a `key = ENV_NAME` reference survives.
#[must_use]
pub fn redact_keep_env(text: &str) -> String {
    redact_impl(text, true)
}

#[must_use]
pub fn key_is_sensitive(key: &str) -> bool {
    hit(SENSITIVE_KEY.is_match(key))
}

/// Recursively redact a JSON-like value: sensitive KEY names lose their value, strings are scrubbed.
#[must_use]
pub fn redact_obj(obj: &Value) -> Value {
    redact_obj_depth(obj, 0)
}

fn redact_obj_depth(obj: &Value, depth: usize) -> Value {
    if depth > 8 {
        return Value::String(REDACTED.to_owned());
    }
    match obj {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, v) in m.iter().take(200) {
                let nv = if key_is_sensitive(k) {
                    Value::String(REDACTED.to_owned())
                } else {
                    redact_obj_depth(v, depth + 1)
                };
                out.insert(k.clone(), nv);
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .take(200)
                .map(|v| redact_obj_depth(v, depth + 1))
                .collect(),
        ),
        Value::String(s) => Value::String(redact(char_prefix(s, 2000))),
        other => other.clone(),
    }
}

/// A short reason if content looks like it embeds a literal secret (env-var NAMES are fine).
#[must_use]
pub fn find_secret(content: &str) -> Option<String> {
    chunks(content).into_iter().find_map(find_secret_chunk)
}

fn find_secret_chunk(content: &str) -> Option<String> {
    if hit(PRIVATE_KEY.is_match(content)) {
        return Some("a private key block".to_owned());
    }
    if hit(BEARER.is_match(content)) {
        return Some("a 'Bearer <token>' literal".to_owned());
    }
    if may_prefixed(content) && hit(PREFIXED.is_match(content)) {
        return Some("a token with a well-known secret prefix (sk-, hf_, ghp_, ...)".to_owned());
    }
    if hit(JWT.is_match(content)) {
        return Some("a JWT".to_owned());
    }
    if !may_keyval(content) {
        return None;
    }
    keyval_secret(&KEYVAL, content)
}

/// The key=value part of [`find_secret`] (a separate function so tests can drive it with a failing regex).
fn keyval_secret(rx: &Regex, content: &str) -> Option<String> {
    for caps in rx.captures_iter(content) {
        let Ok(caps) = caps else {
            return Some(SCAN_INCOMPLETE.to_owned());
        };
        let val = caps.name("val").map_or("", |m| m.as_str());
        if !is_env_reference(val) && char_len(unquote(val)) >= 8 {
            let pre = caps.name("pre").map_or("", |m| m.as_str());
            let key = pre
                .split('=')
                .next()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            let key = key.trim_matches(|c| c == ' ' || c == '"' || c == '\'');
            return Some(format!(
                "a literal value for '{}' (reference an env-var NAME instead)",
                char_prefix(key, 40)
            ));
        }
    }
    None
}

#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// `cap(text, limit)`: `text[:limit] + "...[truncated]"` when longer (code points).
#[must_use]
pub fn cap(text: &str, limit: usize) -> String {
    if char_len(text) <= limit {
        text.to_owned()
    } else {
        format!("{}...[truncated]", char_prefix(text, limit))
    }
}

/// `cap(text)` with Python's default limit of 8000.
#[must_use]
pub fn cap_default(text: &str) -> String {
    cap(text, 8000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A look-around pattern that always hits fancy-regex's backtrack limit on a long run of `a`.
    #[allow(clippy::unwrap_used)]
    fn exploding() -> Regex {
        fancy_regex::RegexBuilder::new(r"(?=(a+)+b)a")
            .backtrack_limit(10)
            .build()
            .unwrap()
    }

    /// Audit finding 5: a regex error never reads as "nothing to redact" / "no secret".
    #[test]
    fn regex_errors_fail_closed() {
        let rx = exploding();
        let text = format!("{} key=sk-should-not-survive", "a".repeat(64));
        assert!(rx.is_match(&text).is_err(), "the test regex must fail");
        assert_eq!(sub_with(&rx, &text, |_| "X".to_owned()), REDACTED);
        assert!(hit(rx.is_match(&text)));
        assert_eq!(keyval_secret(&rx, &text).as_deref(), Some(SCAN_INCOMPLETE));
    }

    #[test]
    fn chunking_matches_python_edges() {
        let text = format!("{}{}", "a".repeat(3900), " b".repeat(300));
        let parts = chunks(&text);
        assert_eq!(parts.concat(), text);
        assert!(char_len(parts[0]) <= CHUNK);
    }

    #[test]
    fn keyval_matches_python_on_one_line_json() {
        // CPython: redact('["HF_TOKEN=", "X=1"]') == '["HF_TOKEN=[redacted]X=1"]' (the quoted alternative spans
        // `", "`); on indented JSON (one entry per line) the value cannot cross the newline: see the core tests.
        assert_eq!(
            redact(r#"["HF_TOKEN=", "X=1"]"#),
            r#"["HF_TOKEN=[redacted]X=1"]"#
        );
    }
}
