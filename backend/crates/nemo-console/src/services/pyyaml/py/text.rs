//! Python `str` semantics that differ from Rust's: `isspace`, `split()`, `strip()`, `splitlines()`, `isprintable` and
//! `repr()` of a string. Messages the API returns embed `repr()` (`{name!r}`), so it must be exact.

/// `str.isspace()` for one character (bidi WS/B/S or category Zs), CPython 3.13.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '\u{1c}'..='\u{1f}' | ' ' | '\u{85}' | '\u{a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// `s.split()` (no separator): runs of whitespace separate, leading/trailing whitespace ignored.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

/// `s.strip()` with the default whitespace set.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.lstrip()`.
pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `s.rstrip()`.
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `" ".join(text.split())`.
pub fn collapse_ws(s: &str) -> String {
    split_ws(s).join(" ")
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

/// `s.splitlines(keepends)`: splits on every Python line boundary (`\r\n` counts as one).
pub fn splitlines(s: &str, keepends: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if !is_line_break(c) {
            continue;
        }
        let mut end = i + c.len_utf8();
        if c == '\r'
            && let Some(&(j, '\n')) = it.peek()
        {
            end = j + 1;
            it.next();
        }
        out.push(if keepends {
            &s[start..end]
        } else {
            &s[start..i]
        });
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `str.isprintable()` for one character. CPython: everything except categories Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs
/// other than the ASCII space. Rust has no category tables in std, so Cf/Co/Zs/Zl/Zp are listed explicitly and Cn
/// (unassigned) is approximated by the noncharacters; a code point unassigned in Unicode 15.1 is treated as printable
/// (DEVIATION D-REPR in PORTING.md: affects only repr() of unassigned code points).
pub fn is_printable(c: char) -> bool {
    let cp = c as u32;
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false; // Cc
    }
    // Zs (except space), Zl, Zp
    if matches!(
        cp,
        0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000
    ) {
        return false;
    }
    // Cf (format)
    if matches!(
        cp,
        0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890 | 0x891 | 0x8e2 | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e
            | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb | 0x110bd | 0x110cd | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f
    ) {
        return false;
    }
    // Co (private use) and Cn noncharacters
    if matches!(cp, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd | 0xfdd0..=0xfdef) {
        return false;
    }
    if cp & 0xfffe == 0xfffe {
        return false;
    }
    true
}

/// `repr(s)` of a Python `str`.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let cp = c as u32;
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// `s[:n]` counted in code points (Python slices count characters, not bytes).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `len(s)` in code points.
pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// `str.isalnum()` on a whole string (non-empty, every char alphanumeric).
pub fn is_alnum(s: &str) -> bool {
    !s.is_empty() && s.chars().all(char::is_alphanumeric)
}

/// `int(s)` for a str (whitespace stripped, optional sign, `_` only between digits); None where Python raises.
pub fn parse_int(s: &str) -> Option<i64> {
    let t = strip(s);
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if t.is_empty()
        || t.starts_with('_')
        || t.ends_with('_')
        || t.contains("__")
        || !t.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let digits: String = t.chars().filter(|c| *c != '_').collect();
    let v = digits.parse::<i64>().ok()?;
    Some(if neg { -v } else { v })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_quotes_and_escapes() {
        assert_eq!(repr("abc"), "'abc'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr("t\tn\nr\r\\"), "'t\\tn\\nr\\r\\\\'");
        assert_eq!(
            repr("\u{0}\u{7f}é\u{a0}\u{2028}😀"),
            "'\\x00\\x7fé\\xa0\\u2028😀'"
        );
    }

    #[test]
    fn splitlines_like_python() {
        assert_eq!(splitlines("a\r\nb\rc\nd", false), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\nb\n", true), vec!["a\n", "b\n"]);
        assert_eq!(splitlines("a\u{2028}b\u{0c}", false), vec!["a", "b"]);
        assert!(splitlines("", false).is_empty());
    }

    #[test]
    fn whitespace_set() {
        assert_eq!(split_ws(" a\u{1f}b\u{a0}c  "), vec!["a", "b", "c"]);
        assert_eq!(strip("\u{85} x \u{3000}"), "x");
        assert_eq!(take_chars("héllo", 2), "hé");
    }
}
