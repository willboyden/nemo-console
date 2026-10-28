//! Python `str` methods the workstream B adapters (dcgm, aiperf, guardrails, openshell) depend on, with CPython's exact
//! semantics. Rust's similar-looking std functions differ: `str::trim` does not treat U+001C..U+001F as whitespace,
//! `str::lines` only breaks on `\n`, and slicing counts bytes instead of code points.
//!
//! Shared by the DCGM, AIPerf, Guardrails and OpenShell adapters; folded into a shared `services::pystr` (PORTING.md section 9 note).

/// `ch.isspace()` (CPython: bidirectional class WS, B or S, or category Zs).
#[must_use]
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0b}'
            | '\u{0c}'
            | '\r'
            | '\u{1c}'..='\u{1f}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `s.strip()`.
#[must_use]
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.lstrip()`.
#[must_use]
pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `s.rstrip()`.
#[must_use]
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `s.split()` (no separator: runs of whitespace, no empty parts).
#[must_use]
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// `s.splitlines()` (no `keepends`): every Python line boundary, `\r\n` counts as one.
#[must_use]
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if is_line_break(c) {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r'
                && let Some(&(j, '\n')) = it.peek()
            {
                it.next();
                end = j + 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `s.splitlines(keepends=True)`.
#[must_use]
pub fn splitlines_keepends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if is_line_break(c) {
            let mut end = i + c.len_utf8();
            if c == '\r'
                && let Some(&(j, '\n')) = it.peek()
            {
                it.next();
                end = j + 1;
            }
            out.push(&s[start..end]);
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `s[:n]` (code points).
#[must_use]
pub fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `s[-n:]` (code points; `n == 0` gives the whole string, like Python's `s[-0:]`).
#[must_use]
pub fn tail(s: &str, n: usize) -> &str {
    if n == 0 {
        return s;
    }
    let len = s.chars().count();
    if len <= n {
        return s;
    }
    match s.char_indices().nth(len - n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

/// `s.isdigit()` restricted to what the adapters can meet (ASCII digits; non-empty).
#[must_use]
pub fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `f"{s:<width}"` (pads with spaces to `width` code points).
#[must_use]
pub fn ljust(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_owned()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

/// `str(x)` for a Python float (`repr`).
#[must_use]
pub fn float_str(x: f64) -> String {
    crate::services::pyfmt::float_repr(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_and_split_follow_python() {
        assert_eq!(strip("\u{1c} a \u{85}"), "a");
        assert_eq!(split_ws(" a\tb\u{a0}c  "), vec!["a", "b", "c"]);
        assert_eq!(splitlines("a\r\nb\rc\u{0b}d\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n"), vec![""]);
        assert_eq!(splitlines_keepends("a\r\nb"), vec!["a\r\n", "b"]);
        assert_eq!(head("héllo", 2), "hé");
        assert_eq!(tail("héllo", 4), "éllo");
        assert_eq!(tail("ab", 5), "ab");
        assert_eq!(ljust("ab", 4), "ab  ");
    }
}
