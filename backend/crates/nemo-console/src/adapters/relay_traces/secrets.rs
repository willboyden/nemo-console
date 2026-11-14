//! The trace explorer's own secret scrubbing of content (on top of the adapter's shared `scrub()`, which is kept as
//! it is because the parity harness compares it with the Python reference).
//!
//! Captured content holds credentials in more shapes than `scrub()` knows: a JSON pair inside a tool result
//! (`"token": "…"`), the same pair inside JSON that is itself a string (`\"token\": \"…\"`), a Python dict
//! (`'password': '…'`), YAML and env lines (`api_key: …`, `export API_TOKEN=…`) and URL queries (`?access_token=…`).
//! Every pair whose key names a credential and whose value is at least 8 characters has its value replaced by
//! `[REDACTED]`; configuration placeholders (`${VAR}`, `{env:…}`, `{file:…}`) and values that are already redacted
//! are left alone. An admin viewer may see too much redacted, never too little.
//!
//! Only the `regex` crate is used here: matching is linear in the length of the text, whatever the text.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::adapters::relay::scrub_count;

const MASK: &str = "[REDACTED]";
/// The shortest value that is redacted.
const MIN_VALUE: usize = 8;

/// The words that make a key a credential's (case-insensitive, anywhere in the key).
const WORDS: &str =
    "api_key|api-key|apikey|secret|password|passwd|token|private_key|access_key|credential|auth";

fn build(pattern: &str) -> Regex {
    #[allow(clippy::expect_used)]
    Regex::new(pattern).expect("static regex")
}

/// `"key": "value"`, also with escaped quotes (`\"key\": \"value\"`, any number of backslashes) as inside JSON that
/// is a string. Group 1: everything up to the value; group 2: the value (up to the next quote or backslash-quote).
static DOUBLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    build(&format!(
        r#"(?i)(\\*"[A-Za-z0-9_.\-]*(?:{WORDS})[A-Za-z0-9_.\-]*\\*"\s*:\s*\\*")((?:[^"\\\r\n]|\\+[^"\\\r\n])*)"#
    ))
});

/// `'key': 'value'` (a Python dict, a JavaScript object with single quotes).
static SINGLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    build(&format!(
        r"(?i)('[A-Za-z0-9_.\-]*(?:{WORDS})[A-Za-z0-9_.\-]*'\s*:\s*')((?:[^'\\\r\n]|\\[^\r\n])*)"
    ))
});

/// An unquoted key: `key: value`, `KEY=value`, `export KEY=value`, `?token=value&`, `Authorization: Basic value`.
/// The key starts a line or follows a separator (also a literal `\n` of an escaped string); the value runs to the
/// next blank, quote, backslash, `&`, `<` or `>`.
static UNQUOTED: LazyLock<Regex> = LazyLock::new(|| {
    build(&format!(
        r#"(?im)((?:^|[\s;,{{(?&"']|\\[nrt])(?:export[ \t]+)?[A-Za-z0-9_.\-]*(?:{WORDS})[A-Za-z0-9_.\-]*[ \t]*[:=][ \t]*(?:(?:bearer|basic|token|digest)[ \t]+)?["']?)([^\s"'\\&<>]+)"#
    ))
});

/// A value that is not a secret: a configuration placeholder, already redacted, or too short.
fn keep(value: &str) -> bool {
    value.chars().count() < MIN_VALUE
        || value.starts_with("${")
        || value.starts_with("{env:")
        || value.starts_with("{file:")
        || value.starts_with("[REDACTED")
}

/// One pass of one pattern; `n` counts the values replaced.
fn pass(rx: &Regex, text: String, n: &mut usize) -> String {
    let out = rx.replace_all(&text, |c: &Captures<'_>| {
        let value = c.get(2).map_or("", |m| m.as_str());
        let pre = c.get(1).map_or("", |m| m.as_str());
        if keep(value) {
            format!("{pre}{value}")
        } else {
            *n += 1;
            format!("{pre}{MASK}")
        }
    });
    match out {
        std::borrow::Cow::Borrowed(_) => text,
        std::borrow::Cow::Owned(s) => s,
    }
}

/// Credential-shaped pairs in a text: the value replaced by `[REDACTED]`, and how many were replaced.
#[must_use]
pub fn scrub_pairs(text: &str) -> (String, usize) {
    let mut n = 0;
    let mut out = text.to_owned();
    for rx in [&*DOUBLE_QUOTED, &*SINGLE_QUOTED, &*UNQUOTED] {
        out = pass(rx, out, &mut n);
    }
    (out, n)
}

/// Everything content goes through before it leaves the console: the adapter's `scrub()` (keys, bearer tokens,
/// `key = "value"` lines), then the credential pairs above. The count is the sum of both.
#[must_use]
pub fn scrub_content(text: &str) -> (String, usize) {
    let (first, a) = scrub_count(text);
    let (second, b) = scrub_pairs(&first);
    (second, a + b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn json_pairs_plain_escaped_and_single_quoted() {
        for (input, want) in [
            (
                format!(r#"{{"token": "{HEX}", "n": 1}}"#),
                r#"{"token": "[REDACTED]", "n": 1}"#.to_owned(),
            ),
            (
                format!(r#"{{"token_id":"{HEX}"}}"#),
                r#"{"token_id":"[REDACTED]"}"#.to_owned(),
            ),
            (
                format!(r#"result: "{{\"token\": \"{HEX}\", \"ok\": true}}""#),
                r#"result: "{\"token\": \"[REDACTED]\", \"ok\": true}""#.to_owned(),
            ),
            (
                format!(r#"{{\\\"Api-Key\\\":\\\"{HEX}\\\"}}"#),
                r#"{\\\"Api-Key\\\":\\\"[REDACTED]\\\"}"#.to_owned(),
            ),
            (
                "{'password': 'correct horse battery'}".to_owned(),
                "{'password': '[REDACTED]'}".to_owned(),
            ),
            (
                r#""Authorization": "Basic dXNlcjpwYXNzd29yZA==""#.to_owned(),
                r#""Authorization": "[REDACTED]""#.to_owned(),
            ),
        ] {
            let (out, n) = scrub_pairs(&input);
            assert_eq!(out, want, "{input}");
            assert_eq!(n, 1, "{input}");
        }
    }

    #[test]
    fn yaml_env_and_query_forms() {
        for (input, want) in [
            (
                format!("db:\n  password: {HEX}\n  host: x"),
                "db:\n  password: [REDACTED]\n  host: x".to_owned(),
            ),
            (
                format!("API_KEY={HEX}\nOTHER=1"),
                "API_KEY=[REDACTED]\nOTHER=1".to_owned(),
            ),
            (
                format!("export GITHUB_TOKEN={HEX}"),
                "export GITHUB_TOKEN=[REDACTED]".to_owned(),
            ),
            (
                format!(r"API_KEY={HEX}\nSECRET_X=abcdefghij"),
                r"API_KEY=[REDACTED]\nSECRET_X=[REDACTED]".to_owned(),
            ),
            (
                format!("GET /cb?access_token={HEX}&state=1"),
                "GET /cb?access_token=[REDACTED]&state=1".to_owned(),
            ),
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZA==".to_owned(),
                "Authorization: Basic [REDACTED]".to_owned(),
            ),
            (
                "client_secret: 'abcdefghijkl'".to_owned(),
                "client_secret: '[REDACTED]'".to_owned(),
            ),
        ] {
            let (out, n) = scrub_pairs(&input);
            assert_eq!(out, want, "{input}");
            assert!(n >= 1, "{input}");
        }
    }

    #[test]
    fn placeholders_short_values_and_other_keys_are_left_alone() {
        for input in [
            r#"{"api_key": "${OPENAI_API_KEY}"}"#,
            r#"{"apiKey": "{env:LITELLM_KEY}"}"#,
            r#"{"token": "{file:~/.config/x.key}"}"#,
            r#"{"token": "[REDACTED]"}"#,
            r#"{\"token\": \"[REDACTED]\"}"#,
            r#"{"password": "short"}"#,
            "password: 1234567",
            "API_KEY=${API_KEY}",
            "Authorization: Bearer [REDACTED]",
            r#"{"model": "local-small-model-name", "path": "/very/long/path/name"}"#,
            "the max_tokens: 512 and nothing else",
        ] {
            let (out, n) = scrub_pairs(input);
            assert_eq!(out, input);
            assert_eq!(n, 0, "{input}");
        }
    }

    #[test]
    fn scrub_content_counts_both_passes() {
        let text = format!("use sk-abcdefghijklmnopqrstuvwx\n{{\"token\": \"{HEX}\"}}");
        let (out, n) = scrub_content(&text);
        assert_eq!(out, "use [REDACTED]\n{\"token\": \"[REDACTED]\"}");
        assert_eq!(n, 2);
    }

    #[test]
    fn hostile_text_stays_linear() {
        // inputs built to make a backtracking matcher blow up; each at n and 4n repetitions
        let shapes: [fn(usize) -> String; 7] = [
            |n| "\"token".repeat(n),
            |n| "\\\"token\\\":\\\"".repeat(n / 2),
            |n| "token=".repeat(n),
            |n| format!("\"token\": \"{}", "\\a".repeat(n)),
            |n| format!("'secret': '{}", "\\x".repeat(n)),
            |n| format!("auth:{}", " ".repeat(4 * n)),
            |n| format!("{}=", "api_key".repeat(n)),
        ];
        let time = |n: usize| {
            let t0 = std::time::Instant::now();
            for f in &shapes {
                let _ = scrub_content(&f(n));
            }
            t0.elapsed()
        };
        let small = time(50_000);
        let large = time(200_000);
        // four times the input: about four times the time (quadratic would be sixteen)
        assert!(
            large < small * 9 + std::time::Duration::from_millis(200),
            "{small:?} -> {large:?}"
        );
        assert!(large < std::time::Duration::from_secs(20), "{large:?}");
    }
}
