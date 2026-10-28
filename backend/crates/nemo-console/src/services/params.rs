//! Validate user-supplied values against ParamSpec descriptors (actions and the params endpoints).
//! Port of `services/params.py`. Contract (PORTING.md section 6.7).

use serde_json::{Map, Value};

use crate::domain::models::{ParamSpec, ParamType};
use crate::services::errors::ApiError;
use crate::services::pyfmt::{char_len, char_prefix, float_repr};

pub const MAX_STR: usize = 1024;
pub const MAX_LIST: usize = 64;

fn bad(key: &str, why: &str) -> ApiError {
    ApiError::new(422, "invalid_param", format!("param '{key}': {why}"))
}

/// Check every value against its spec; unknown keys are refused. Order of `values` is kept.
pub fn validate_values(
    specs: &[ParamSpec],
    values: &Map<String, Value>,
) -> Result<Map<String, Value>, ApiError> {
    let mut out = Map::new();
    for (key, v) in values {
        let Some(s) = specs.iter().rev().find(|s| &s.key == key) else {
            return Err(bad(char_prefix(key, 60), "unknown"));
        };
        out.insert(key.clone(), check(s, v)?);
    }
    Ok(out)
}

/// Python formats the bound with `{s.min}`: a float, so `1.0`, `0.95`.
fn bound(x: f64) -> String {
    float_repr(x)
}

fn check(s: &ParamSpec, v: &Value) -> Result<Value, ApiError> {
    match s.type_ {
        ParamType::Int | ParamType::Float => {
            let t = s.type_.as_str();
            let Value::Number(n) = v else {
                return Err(bad(&s.key, &format!("expected {t}")));
            };
            let as_f = n.as_f64().unwrap_or(f64::NAN);
            if s.type_ == ParamType::Int && n.is_f64() && as_f.fract() != 0.0 {
                return Err(bad(&s.key, &format!("expected {t}")));
            }
            let out: Value = if s.type_ == ParamType::Int {
                if let Some(i) = n.as_i64() {
                    Value::from(i)
                } else {
                    #[allow(clippy::cast_possible_truncation)]
                    Value::from(as_f as i64)
                }
            } else {
                serde_json::Number::from_f64(as_f).map_or(Value::Null, Value::Number)
            };
            if let Some(min) = s.min
                && as_f < min
            {
                return Err(bad(&s.key, &format!("below minimum {}", bound(min))));
            }
            if let Some(max) = s.max
                && as_f > max
            {
                return Err(bad(&s.key, &format!("above maximum {}", bound(max))));
            }
            Ok(out)
        }
        ParamType::Bool => match v {
            Value::Bool(_) => Ok(v.clone()),
            _ => Err(bad(&s.key, "expected bool")),
        },
        ParamType::Enum => match v {
            Value::String(x) if s.choices.as_ref().is_some_and(|c| c.contains(x)) => Ok(v.clone()),
            _ => Err(bad(&s.key, "not one of the allowed choices")),
        },
        ParamType::String => string(&s.key, v).map(Value::String),
        ParamType::StringList => match v {
            Value::Array(a) if a.len() <= MAX_LIST => a
                .iter()
                .map(|x| string(&s.key, x).map(Value::String))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            _ => Err(bad(&s.key, "expected a short list of strings")),
        },
    }
}

fn string(key: &str, v: &Value) -> Result<String, ApiError> {
    match v {
        Value::String(s) if char_len(s) <= MAX_STR && !s.contains(['\0', '\n', '\r']) => {
            Ok(s.clone())
        }
        _ => Err(bad(key, "expected a single-line string")),
    }
}
