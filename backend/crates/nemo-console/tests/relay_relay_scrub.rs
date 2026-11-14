//! The Relay adapter's `scrub()` `key = "value"` rule (PORTING.md D-35): linear on any input, and the same output as
//! the original back-reference pattern (kept here as the oracle, run only on inputs it can finish).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nemo_console::adapters::relay::{scrub, scrub_count};

/// The pattern as it was (the Python reference's, with D-32's same-line blanks around `=`), applied the way the
/// Python reference applies it. `None` when fancy-regex gives up.
fn oracle(text: &str) -> Option<String> {
    static RX: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(
            r#"(?im)^(\s*(?:api_key|apikey|secret|password|token|access_key|private_key)[^\S\r\n]*=[^\S\r\n]*)(["'])(?!\[REDACTED)[^"'\n]+\2"#,
        )
        .unwrap()
    });
    let rx = &*RX;
    let mut out = String::new();
    let mut last = 0;
    for c in rx.captures_iter(text) {
        let c = c.ok()?;
        let m = c.get(0).unwrap();
        out.push_str(&text[last..m.start()]);
        out.push_str(c.get(1).unwrap().as_str());
        out.push_str("\"[REDACTED]\"");
        last = m.end();
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// Text without `sk-` keys or bearer tokens, so that only the `key = "value"` rule acts.
fn corpus(seed: u64, lines: usize) -> String {
    const PIECES: [&str; 22] = [
        "password = \"hunter2\"",
        "  token='abc def'",
        "\tsecret=\"x\"",
        "API_KEY = \"zz\"",
        "password = \"[REDACTED]\"",
        "token = '[REDACTED] tail'",
        "password = \"unclosed",
        "password = 'mixed\"",
        "my password = \"no\"",
        "password\t=\t\"tabs\"",
        "password\u{a0}=\u{a0}\"nbsp\"",
        "access_key =\n\"next line\"",
        "private_key = \"a\" trailing",
        "",
        "   ",
        "\r",
        "plain text line",
        "apikey=''",
        "token = \"one\" token = \"two\"",
        "\u{2028}token = \"ls\"",
        "secret = \"é ü\"",
        "key = \"not a secret key\"",
    ];
    let mut x = seed | 1;
    let mut out = String::new();
    for _ in 0..lines {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push_str(PIECES[usize::try_from(x % PIECES.len() as u64).unwrap_or(0)]);
        out.push_str(if x.is_multiple_of(5) { "\r\n" } else { "\n" });
    }
    out
}

#[test]
fn the_linear_rule_equals_the_original_pattern() {
    let mut compared = 0;
    for seed in 1..401 {
        let text = corpus(seed, 40);
        let Some(want) = oracle(&text) else { continue };
        assert_eq!(scrub(&text), want, "seed {seed}: {text:?}");
        compared += 1;
    }
    assert!(compared > 390, "{compared}");
}

#[test]
fn long_blank_runs_without_a_secret_come_back_unchanged() {
    for n in [600, 1000, 5000] {
        for blank in ["\n", "\r\n", "    \n", " \t \r\n"] {
            let text = format!("first line\n{}last line: ordinary text\n", blank.repeat(n));
            assert_eq!(scrub(&text), text, "{n} x {blank:?}");
            let (out, k) = scrub_count(&text);
            assert_eq!((out.as_str(), k), (text.as_str(), 0));
            // with a secret after them: only the value is replaced
            let secret = format!(
                "{}  password = \"correct horse battery staple\"\nafter\n",
                blank.repeat(n)
            );
            let want = format!("{}  password = \"[REDACTED]\"\nafter\n", blank.repeat(n));
            assert_eq!(scrub(&secret), want, "{n} x {blank:?}");
            assert_eq!(scrub_count(&secret), (want, 1));
        }
    }
}

#[test]
fn hostile_input_is_linear() {
    let text = format!(
        "{}password = \"{}",
        "\n".repeat(200_000),
        "x".repeat(200_000)
    );
    let t0 = std::time::Instant::now();
    assert_eq!(scrub(&text), text);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        t0.elapsed()
    );
}
