//! Shared helpers for the `services::pyyaml` golden tests: the typed JSON encoding of parity/gen/yaml_codec.py <->
//! PyValue, and the `DumpOpts` behind each golden option-set name. Copied from a sibling project, Agent Hub's Rust port (commit
//! a214dd6, hub-core/tests/yaml_support/mod.rs); Nemo's changes: the crate paths, Nemo's option-set names
//! (parity/gen/gen_yaml_golden.py), the goldens directory, and the safe loader only (Nemo has no strict loader).
//! Since A-7 the loader is the core engine (workstream C's port): documents are compared as `safe_load_value` trees, tuples
//! as tuples, cycles as "loads, cannot become a tree", errors by `str(e)`, class and marks.
// Test-support code: helpers outside `#[test]` functions that report through panics and stderr by design.
#![allow(dead_code, unreachable_pub, clippy::panic, clippy::print_stderr)]

use base64::Engine as _;
use nemo_console::services::pyyaml::py::float;
use nemo_console::services::pyyaml::{self as yaml, DateTime, DumpOpts, PyValue};
use serde_json::{Map, Value, json};

pub fn opts_by_name(name: &str) -> Option<DumpOpts> {
    Some(match name {
        "safe_dump" => DumpOpts::SAFE_DUMP,
        "unsorted" => DumpOpts::SAFE_DUMP_UNSORTED,
        "flow" => DumpOpts {
            sort_keys: true,
            allow_unicode: false,
            default_flow_style: true,
            width: 80,
        },
        "unicode" => DumpOpts {
            sort_keys: false,
            allow_unicode: true,
            default_flow_style: false,
            width: 100,
        },
        _ => return None,
    })
}

fn num<T: TryFrom<i64> + Default>(v: &Value) -> T {
    v.as_i64()
        .and_then(|i| T::try_from(i).ok())
        .unwrap_or_default()
}

/// yaml_codec encoding -> PyValue (tuples become lists).
pub fn dec(j: &Value) -> PyValue {
    match j {
        Value::Null | Value::Number(_) | Value::Array(_) => PyValue::None,
        Value::Bool(b) => PyValue::Bool(*b),
        Value::String(s) => PyValue::Str(s.clone()),
        Value::Object(o) => {
            let Some((k, v)) = o.iter().next() else {
                return PyValue::None;
            };
            match k.as_str() {
                "int" => {
                    let s = v.as_str().unwrap_or("0");
                    s.parse::<i64>()
                        .map_or_else(|_| PyValue::BigInt(s.to_string()), PyValue::Int)
                }
                "float" => PyValue::Float(match v.as_str().unwrap_or("nan") {
                    "nan" => f64::NAN,
                    "inf" => f64::INFINITY,
                    "-inf" => f64::NEG_INFINITY,
                    s => s.parse().unwrap_or(f64::NAN),
                }),
                "bytes" => PyValue::Bytes(
                    base64::engine::general_purpose::STANDARD
                        .decode(v.as_str().unwrap_or_default())
                        .unwrap_or_default(),
                ),
                "date" => PyValue::Date {
                    year: num(&v[0]),
                    month: num(&v[1]),
                    day: num(&v[2]),
                },
                "datetime" => PyValue::DateTime(DateTime {
                    year: num(&v[0]),
                    month: num(&v[1]),
                    day: num(&v[2]),
                    hour: num(&v[3]),
                    minute: num(&v[4]),
                    second: num(&v[5]),
                    microsecond: num(&v[6]),
                    offset_s: v[7].as_i64().and_then(|i| i32::try_from(i).ok()),
                }),
                "list" => PyValue::List(
                    v.as_array()
                        .map(|a| a.iter().map(dec).collect())
                        .unwrap_or_default(),
                ),
                "tuple" => PyValue::Tuple(
                    v.as_array()
                        .map(|a| a.iter().map(dec).collect())
                        .unwrap_or_default(),
                ),
                "set" => PyValue::Set(
                    v.as_array()
                        .map(|a| a.iter().map(dec).collect())
                        .unwrap_or_default(),
                ),
                "dict" => PyValue::Dict(
                    v.as_array()
                        .map(|a| a.iter().map(|p| (dec(&p[0]), dec(&p[1]))).collect())
                        .unwrap_or_default(),
                ),
                _ => PyValue::None,
            }
        }
    }
}

/// PyValue -> yaml_codec encoding (as Python's `enc` would write it).
pub fn enc(v: &PyValue) -> Value {
    match v {
        PyValue::None => Value::Null,
        PyValue::Bool(b) => Value::Bool(*b),
        PyValue::Int(i) => json!({ "int": i.to_string() }),
        PyValue::BigInt(s) => json!({ "int": s }),
        PyValue::Float(f) => json!({ "float": float::repr(*f) }),
        PyValue::Str(s) => Value::String(s.clone()),
        PyValue::Bytes(b) => {
            json!({ "bytes": base64::engine::general_purpose::STANDARD.encode(b) })
        }
        PyValue::Date { year, month, day } => json!({ "date": [year, month, day] }),
        PyValue::DateTime(d) => {
            json!({ "datetime": [d.year, d.month, d.day, d.hour, d.minute, d.second, d.microsecond, d.offset_s] })
        }
        PyValue::List(l) => json!({ "list": l.iter().map(enc).collect::<Vec<_>>() }),
        PyValue::Set(l) => json!({ "set": l.iter().map(enc).collect::<Vec<_>>() }),
        PyValue::Tuple(l) => json!({ "tuple": l.iter().map(enc).collect::<Vec<_>>() }),
        PyValue::Dict(d) => {
            json!({ "dict": d.iter().map(|(k, v)| json!([enc(k), enc(v)])).collect::<Vec<_>>() })
        }
    }
}

/// A golden value normalised for comparison with [`enc`]: every `{"float": repr}` is re-formatted from its parsed
/// value, so floats compare by value (tuples stay tuples since A-7).
pub fn tuples_as_lists(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(tuples_as_lists).collect()),
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, x) in o {
                if k == "float"
                    && let Some(PyValue::Float(f)) = Some(dec(&json!({ "float": x })))
                {
                    m.insert(k.clone(), json!(float::repr(f)));
                    continue;
                }
                m.insert(k.clone(), tuples_as_lists(x));
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

/// Nemo: a Python `set` iterates in hash order (it varies with PYTHONHASHSEED); the loader keeps document order. Sets
/// compare as sets: their elements sorted by their encoding, on both sides.
pub fn sets_sorted(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(sets_sorted).collect()),
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, x) in o {
                let mut x = sets_sorted(x);
                if k == "set"
                    && let Value::Array(items) = &mut x
                {
                    items.sort_by_key(std::string::ToString::to_string);
                }
                m.insert(k.clone(), x);
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

// ---- comparisons against PyYAML outcomes (shared by the golden and the property tests) ----

pub fn golden(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/pyyaml/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Compare one `yaml.safe_load` outcome; returns a failure description.
pub fn check_load(text: &str, want: &Value) -> Option<String> {
    if want.get("unencodable").is_some() {
        return None; // a lone surrogate (DEVIATION D-YAML-SURROGATE): not comparable
    }
    let got = yaml::safe_load(text);
    if want.get("cyclic").is_some() {
        // Python builds a self-containing object; so does the arena engine, but it cannot become a tree
        return match got {
            Ok(doc) if yaml::to_value(&doc, &doc.root).is_err() => None,
            other => Some(format!("{text:?}: want a cyclic object, got {other:?}")),
        };
    }
    let got = got.and_then(|d| yaml::to_value(&d, &d.root).map_err(yaml::YamlError::Py));
    if let (Some(j), Ok(g)) = (want.get("ok_json"), &got) {
        let ge = serde_json::to_string(&enc(g)).unwrap_or_default();
        return (j.as_str() != Some(ge.as_str())).then(|| format!("{text:?}: deep value differs"));
    }
    match (want.get("ok"), want.get("err"), got) {
        (Some(v), _, Ok(g)) => {
            let ge = sets_sorted(&enc(&g));
            let want = sets_sorted(&tuples_as_lists(v));
            (ge != want).then(|| format!("{text:?}:\n  got  {ge}\n  want {want}"))
        }
        (_, Some(e), Err(g)) => {
            let mark = g
                .problem_mark()
                .map(|m| json!({ "line": m.line, "column": m.column, "index": m.index }));
            let full = g.full();
            // the hub corpus records `problem` as the first line of str(e) (its own convention; PyYAML's real
            // attribute is pinned by tests/evaluator_yaml_golden.rs); for an escaping Python exception, str(e)
            let problem = if g.is_yaml() {
                full.lines().next().unwrap_or_default().to_owned()
            } else {
                g.text()
            };
            let got = json!({
                "problem": problem, "full": full, "custom": false,
                "py_exc": (!g.is_yaml()).then(|| g.class()),
                "problem_mark": mark, "class": g.class(),
            });
            (&got != e).then(|| format!("{text:?}:\n  got  {got}\n  want {e}"))
        }
        (_, _, got) => Some(format!("{text:?}: want {want}, got {got:?}")),
    }
}

pub fn run_loads(cases: &[Value], failures: &mut Vec<String>) -> usize {
    let mut n = 0;
    for case in cases {
        let text = case["text"].as_str().unwrap_or_default();
        failures.extend(check_load(text, &case["safe"]));
        n += 1;
    }
    n
}

pub fn run_dumps(cases: &[Value], failures: &mut Vec<String>) -> usize {
    let mut n = 0;
    for case in cases {
        let value = match case.get("construct") {
            Some(c) => construct(
                c[0].as_str().unwrap_or_default(),
                c[1].as_u64().unwrap_or(0),
            ),
            None => dec(&case["value"]),
        };
        let Some(outs) = case["out"].as_object() else {
            continue;
        };
        for (name, want) in outs {
            let Some(opts) = opts_by_name(name) else {
                continue;
            };
            let Some(want) = want.as_str() else {
                failures.push(format!("{name} {}: Python raised {want}", case["value"]));
                continue;
            };
            n += 1;
            let got = yaml::dump(&value, &opts);
            if got != want {
                failures.push(format!(
                    "dump[{name}] {}:\n  got  {got:?}\n  want {want:?}",
                    case["value"]
                ));
            }
        }
    }
    n
}

/// The deep values the generator builds programmatically (`"leaf"` wrapped `depth` times).
pub fn construct(kind: &str, depth: u64) -> PyValue {
    let mut v = PyValue::str("leaf");
    for i in 0..depth {
        v = if kind == "list" || (kind == "mixed" && i % 2 == 1) {
            PyValue::List(vec![v])
        } else {
            PyValue::Dict(vec![(PyValue::str("k"), v)])
        };
    }
    v
}

/// The comparisons recurse over deeply nested goldens (the harness, not the engine: the engine is iterative and
/// proven on a 2 MiB stack in services/pyyaml/mod.rs), so they run on a thread with a large stack.
pub fn big_stack(f: impl FnOnce() + Send + 'static) {
    let h = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(f)
        .unwrap_or_else(|e| panic!("spawn: {e}"));
    if let Err(e) = h.join() {
        std::panic::resume_unwind(e);
    }
}

pub fn report(what: &str, checked: usize, failures: &[String]) {
    for f in failures.iter().take(40) {
        eprintln!("FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{what}: {} of {checked} checks differ from PyYAML",
        failures.len()
    );
    eprintln!("{what}: {checked} checks equal to PyYAML");
}
