//! `shlex.split(s)` exactly as CPython 3.13's `shlex` does it (posix=True, whitespace_split=True, no comments)
//! (PORTING.md risk R-4). The `shlex` crate follows POSIX sh instead (backslash escapes `$` and `` ` `` inside double
//! quotes, `#` starts a comment), which differs from Python on the compose commands this adapter reads.
//!
//! Written by workstream A (in its adapter); moved into core at the workstream A merge (PORTING.md A-6).

/// `ValueError` texts of `shlex.split`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShlexError(pub &'static str);

impl std::fmt::Display for ShlexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

const WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];

fn is_ws(c: char) -> bool {
    WHITESPACE.contains(&c)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Space,
    Word,
    Quote(char),
    Escape,
}

/// `shlex.split(s)`.
pub fn split(s: &str) -> Result<Vec<String>, ShlexError> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    loop {
        // read_token
        let mut quoted = false;
        let mut token = String::new();
        let mut state = St::Space;
        let mut escaped_state = St::Word;
        let mut eof = false;
        loop {
            let next = chars.get(i).copied();
            i += 1;
            match state {
                St::Space => match next {
                    None => {
                        eof = true;
                        break;
                    }
                    Some(c) if is_ws(c) => {
                        if !token.is_empty() || quoted {
                            break;
                        }
                    }
                    Some('\\') => {
                        escaped_state = St::Word;
                        state = St::Escape;
                    }
                    Some(c @ ('\'' | '"')) => state = St::Quote(c),
                    Some(c) => {
                        token.push(c);
                        state = St::Word;
                    }
                },
                St::Quote(q) => {
                    quoted = true;
                    match next {
                        None => return Err(ShlexError("No closing quotation")),
                        Some(c) if c == q => state = St::Word,
                        Some('\\') if q == '"' => {
                            escaped_state = state;
                            state = St::Escape;
                        }
                        Some(c) => token.push(c),
                    }
                }
                St::Escape => {
                    let Some(c) = next else {
                        return Err(ShlexError("No escaped character"));
                    };
                    // in posix shells only the quote itself or the escape character may be escaped within quotes
                    if let St::Quote(q) = escaped_state
                        && c != '\\'
                        && c != q
                    {
                        token.push('\\');
                    }
                    token.push(c);
                    state = escaped_state;
                }
                St::Word => match next {
                    None => {
                        eof = true;
                        break;
                    }
                    Some(c) if is_ws(c) => {
                        state = St::Space;
                        if !token.is_empty() || quoted {
                            break;
                        }
                    }
                    Some(c @ ('\'' | '"')) => state = St::Quote(c),
                    Some('\\') => {
                        escaped_state = St::Word;
                        state = St::Escape;
                    }
                    Some(c) => token.push(c),
                },
            }
        }
        if token.is_empty() && !quoted {
            if eof {
                return Ok(out);
            }
            continue;
        }
        out.push(token);
        if eof {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_shlex() {
        let ok = |s: &str| split(s).unwrap_or_default();
        assert_eq!(ok("a  'b c' \"d\\\"e\" f\\ g"), ["a", "b c", "d\"e", "f g"]);
        assert_eq!(ok("x '' y"), ["x", "", "y"]);
        assert_eq!(ok("\"a\\$b\" #c"), ["a\\$b", "#c"]);
        assert_eq!(
            ok("--speculative-config '{\"method\":\"mtp\"}'"),
            ["--speculative-config", "{\"method\":\"mtp\"}"]
        );
        assert_eq!(split("'open"), Err(ShlexError("No closing quotation")));
        assert_eq!(split("end\\"), Err(ShlexError("No escaped character")));
        assert!(ok("  ").is_empty());
    }
}
