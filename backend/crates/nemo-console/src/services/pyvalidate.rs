//! pydantic v2 "lax python mode" coercions and error texts (core contract). FastAPI validates request bodies and
//! query parameters with these rules and pydantic-settings validates the environment with them; the frontend and
//! the tests read the resulting messages, so the Rust port reproduces them (pinned by tests/fixtures/
//! api_golden.json, generated from the real Python app).

use serde_json::Value;

/// One location step: an object key or a list index (`loc` in pydantic's error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loc {
    Key(String),
    Idx(usize),
}

impl std::fmt::Display for Loc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Loc::Key(k) => f.write_str(k),
            Loc::Idx(i) => write!(f, "{i}"),
        }
    }
}

/// One pydantic error: location, message and type (e.g. `missing`, `int_parsing`).
#[derive(Debug, Clone, PartialEq)]
pub struct PyError {
    pub loc: Vec<Loc>,
    pub msg: String,
    pub kind: &'static str,
}

impl PyError {
    #[must_use]
    pub fn new(loc: Vec<Loc>, kind: &'static str, msg: impl Into<String>) -> Self {
        PyError {
            loc,
            msg: msg.into(),
            kind,
        }
    }
}

/// `loc` builder: `loc(&["body", "content"])`.
#[must_use]
pub fn loc(parts: &[&str]) -> Vec<Loc> {
    parts.iter().map(|p| Loc::Key((*p).to_owned())).collect()
}

/// `"; ".join(f"{'.'.join(loc)}: {msg}" for err in errors[:5])` (api/errors.py `_val`).
#[must_use]
pub fn render_detail(errors: &[PyError]) -> String {
    errors
        .iter()
        .take(5)
        .map(|e| {
            let l: Vec<String> = e.loc.iter().map(ToString::to_string).collect();
            format!("{}: {}", l.join("."), e.msg)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub const MSG_MISSING: &str = "Field required";
pub const MSG_EXTRA: &str = "Extra inputs are not permitted";
pub const MSG_STR: &str = "Input should be a valid string";
pub const MSG_DICT: &str = "Input should be a valid dictionary";
pub const MSG_LIST: &str = "Input should be a valid list";
pub const MSG_MODEL: &str = "Input should be a valid dictionary or object to extract fields from";
pub const MSG_BOOL_PARSE: &str = "Input should be a valid boolean, unable to interpret input";
pub const MSG_BOOL: &str = "Input should be a valid boolean";
pub const MSG_INT_PARSE: &str =
    "Input should be a valid integer, unable to parse string as an integer";
pub const MSG_INT_FRACTION: &str =
    "Input should be a valid integer, got a number with a fractional part";
pub const MSG_INT: &str = "Input should be a valid integer";
pub const MSG_FLOAT_PARSE: &str =
    "Input should be a valid number, unable to parse string as a number";
pub const MSG_FLOAT: &str = "Input should be a valid number";

/// Coercion failure: (pydantic type, message).
pub type CoerceErr = (&'static str, String);

fn err(kind: &'static str, msg: &str) -> CoerceErr {
    (kind, msg.to_owned())
}

/// `bool` in lax python mode.
pub fn lax_bool(v: &Value) -> Result<bool, CoerceErr> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(true),
            _ => Err(err("bool_parsing", MSG_BOOL_PARSE)),
        },
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                match i {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(err("bool_parsing", MSG_BOOL_PARSE)),
                }
            } else if n.as_u64().is_some() {
                Err(err("bool_parsing", MSG_BOOL_PARSE))
            } else {
                // pydantic: 1.0 / 0.0 are accepted, another integral float is bool_parsing, a fraction bool_type
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f == 1.0 {
                    Ok(true)
                } else if f == 0.0 {
                    Ok(false)
                } else if f.fract() == 0.0 {
                    Err(err("bool_parsing", MSG_BOOL_PARSE))
                } else {
                    Err(err("bool_type", MSG_BOOL))
                }
            }
        }
        _ => Err(err("bool_type", MSG_BOOL)),
    }
}

/// Python `int(s)` on a stripped string, plus pydantic's acceptance of `"5.0"`-style integral floats.
fn parse_int_str(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (sign, digits) = match t.as_bytes()[0] {
        b'+' => (1i64, &t[1..]),
        b'-' => (-1i64, &t[1..]),
        _ => (1i64, t),
    };
    let valid_underscores = !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits.chars().all(|c| c.is_ascii_digit() || c == '_');
    if valid_underscores && !digits.is_empty() {
        let clean: String = digits.chars().filter(|c| *c != '_').collect();
        return clean.parse::<i64>().ok().map(|v| v * sign);
    }
    // "5.0" / "5." : a float string with no fractional part
    if let Some((a, b)) = t.split_once('.')
        && !a.is_empty()
        && !b.is_empty()
        && b.chars().all(|c| c == '0')
        && a.trim_start_matches(['+', '-'])
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return a.parse::<i64>().ok();
    }
    None
}

/// `int` in lax python mode.
pub fn lax_int(v: &Value) -> Result<i64, CoerceErr> {
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.2e18 {
                    #[allow(clippy::cast_possible_truncation)]
                    Ok(f as i64)
                } else {
                    Err(err("int_from_float", MSG_INT_FRACTION))
                }
            } else {
                Err(err("int_type", MSG_INT))
            }
        }
        Value::String(s) => parse_int_str(s).ok_or_else(|| err("int_parsing", MSG_INT_PARSE)),
        _ => Err(err("int_type", MSG_INT)),
    }
}

/// `float` in lax python mode (bools and numeric strings are accepted, like pydantic).
pub fn lax_float(v: &Value) -> Result<f64, CoerceErr> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64().ok_or_else(|| err("float_type", MSG_FLOAT)),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            let lower = t.to_ascii_lowercase();
            match lower.as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Ok(f64::INFINITY),
                "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Ok(f64::NAN),
                _ if !t.is_empty()
                    && t.chars().all(|c| {
                        c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')
                    }) =>
                {
                    t.parse::<f64>()
                        .map_err(|_| err("float_parsing", MSG_FLOAT_PARSE))
                }
                _ => Err(err("float_parsing", MSG_FLOAT_PARSE)),
            }
        }
        _ => Err(err("float_type", MSG_FLOAT)),
    }
}

/// `str`: only a string is accepted (no number -> str coercion in pydantic v2).
pub fn strict_str(v: &Value) -> Result<String, CoerceErr> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => Err(err("string_type", MSG_STR)),
    }
}

#[must_use]
pub fn msg_ge(limit: &str) -> String {
    format!("Input should be greater than or equal to {limit}")
}

#[must_use]
pub fn msg_le(limit: &str) -> String {
    format!("Input should be less than or equal to {limit}")
}

#[must_use]
pub fn msg_pattern(pattern: &str) -> String {
    format!("String should match pattern '{pattern}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn int_strings_like_pydantic() {
        for (s, want) in [
            ("+5", Some(5)),
            ("05", Some(5)),
            ("1_0", Some(10)),
            (" 5", Some(5)),
            ("5.0", Some(5)),
            ("1.5", None),
            ("", None),
            ("abc", None),
            ("-3", Some(-3)),
            ("1__0", None),
            ("5.", None),
            ("+_1", None),
            (" 5 ", Some(5)),
            ("1e3", None),
        ] {
            assert_eq!(lax_int(&json!(s)).ok(), want, "{s:?}");
        }
        assert_eq!(lax_int(&json!(1.5)).unwrap_err().0, "int_from_float");
        assert_eq!(lax_int(&json!(true)).ok(), Some(1));
    }

    #[test]
    fn bools_like_pydantic() {
        assert_eq!(lax_bool(&json!("YES")).ok(), Some(true));
        assert_eq!(lax_bool(&json!(2)).unwrap_err().1, MSG_BOOL_PARSE);
        assert_eq!(lax_bool(&json!(0.5)).unwrap_err().1, MSG_BOOL);
        assert_eq!(lax_bool(&json!(1.0)).ok(), Some(true));
        assert_eq!(lax_bool(&json!(2.0)).unwrap_err().1, MSG_BOOL_PARSE);
        assert_eq!(lax_bool(&json!(" true")).unwrap_err().1, MSG_BOOL_PARSE);
        assert_eq!(lax_bool(&json!([])).unwrap_err().1, MSG_BOOL);
        assert_eq!(lax_bool(&json!("maybe")).unwrap_err().1, MSG_BOOL_PARSE);
    }

    #[test]
    fn floats_like_pydantic() {
        assert_eq!(lax_float(&json!("5")).ok(), Some(5.0));
        assert_eq!(lax_float(&json!(true)).ok(), Some(1.0));
        assert_eq!(lax_float(&json!("fast")).unwrap_err().1, MSG_FLOAT_PARSE);
    }
}
