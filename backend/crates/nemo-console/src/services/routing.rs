//! /routing/summary: aggregate `routing.decision` events from the Switchyard adapter (metadata only).
//! Port of `services/routing.py`.

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use crate::domain::models::{SystemEvent, now_ts};
use crate::services::pyfmt::{char_prefix, float_repr};

pub const WINDOW_SECONDS: [(&str, f64); 4] = [
    ("15m", 900.0),
    ("1h", 3600.0),
    ("24h", 86400.0),
    ("7d", 604_800.0),
];
pub const MAX_LABELS: usize = 200;

#[must_use]
pub fn window_seconds(w: &str) -> Option<f64> {
    WINDOW_SECONDS
        .iter()
        .find(|(k, _)| *k == w)
        .map(|(_, v)| *v)
}

fn num(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(-1.0);
            if f >= 0.0 {
                #[allow(clippy::cast_possible_truncation)]
                n.as_i64().unwrap_or(f as i64)
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn tokens(d: &Map<String, Value>) -> (i64, i64) {
    if let Some(Value::Object(t)) = d.get("tokens") {
        let (p, c) = (num(t.get("prompt")), num(t.get("completion")));
        if p != 0 || c != 0 {
            return (p, c);
        }
        return (num(t.get("input")), num(t.get("output")));
    }
    let (p, c) = (num(d.get("prompt_tokens")), num(d.get("completion_tokens")));
    if p != 0 || c != 0 {
        return (p, c);
    }
    (num(d.get("input_tokens")), num(d.get("output_tokens")))
}

/// Python truthiness of a JSON value.
#[must_use]
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn is_error(d: &Map<String, Value>) -> bool {
    if d.get("error").is_some_and(truthy) {
        return true;
    }
    if d.get("ok") == Some(&Value::Bool(false)) {
        return true;
    }
    matches!(d.get("status"), Some(Value::Number(n)) if n.as_i64().is_some_and(|s| s >= 400))
}

/// `str(v)[:80]` for a str or an int (bool counts as int in Python: str(True) == "True").
fn label(v: Option<&Value>) -> String {
    let s = match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_owned(),
        _ => String::new(),
    };
    if s.is_empty() {
        "unknown".to_owned()
    } else {
        char_prefix(&s, 80).to_owned()
    }
}

#[must_use]
pub fn summarize(events: &[SystemEvent], window: &str, now: Option<f64>) -> Value {
    let now = now.unwrap_or_else(now_ts);
    let since = now - window_seconds(window).unwrap_or(0.0);
    let dims = ["route", "model", "tier"];
    let mut by: Vec<IndexMap<String, i64>> =
        vec![IndexMap::new(), IndexMap::new(), IndexMap::new()];
    let (mut prompt, mut completion, mut requests, mut errors) = (0i64, 0i64, 0i64, 0i64);
    for e in events {
        if e.kind != "routing.decision" || e.ts < since {
            continue;
        }
        requests += 1;
        errors += i64::from(is_error(&e.data));
        let (p, c) = tokens(&e.data);
        prompt += p;
        completion += c;
        for (i, dim) in dims.iter().enumerate() {
            let l = label(e.data.get(*dim));
            let counter = &mut by[i];
            if counter.contains_key(&l) || counter.len() < MAX_LABELS {
                *counter.entry(l).or_insert(0) += 1;
            } else {
                *counter.entry("other".to_owned()).or_insert(0) += 1;
            }
        }
    }
    let _ = float_repr; // (float formatting is not needed: all counts are ints)
    json!({
        "window": window, "by_route": by[0], "by_model": by[1], "by_tier": by[2],
        "tokens": {"prompt": prompt, "completion": completion, "total": prompt + completion},
        "requests": requests, "errors": errors,
    })
}
