//! The shared test vectors of ADR-0022 (tests/vectors/check.json): every case runs here on the native build, and the
//! same file runs through the WebAssembly build in Node (frontend/tests/cfg-wasm.test.mjs). Both must produce each
//! case's `expect` exactly, so the browser's verdicts are the backend's, including the regex engines (Unicode case
//! folding, look-arounds), the nesting budget at its edges and the big-int limits.
//!
//! `NEMO_CORE_UPDATE_VECTORS=1 cargo test -p nemo-console-core --test vectors` rewrites the expectations from the
//! native build (review the diff before committing it).

use nemo_console_core::check;
use serde_json::{Value, json};

const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/check.json");

fn text_of(case: &Value) -> String {
    if let Some(t) = case["text"].as_str() {
        return t.to_owned();
    }
    let mut out = String::new();
    for part in case["build"].as_array().into_iter().flatten() {
        let s = part[0].as_str().unwrap_or("");
        let n = usize::try_from(part[1].as_u64().unwrap_or(0)).unwrap_or(0);
        out.push_str(&s.repeat(n));
    }
    out
}

fn arg(case: &Value, i: usize) -> &str {
    case["args"][i].as_str().unwrap_or("")
}

fn run(case: &Value) -> Value {
    match case["fn"].as_str().unwrap_or("") {
        "check" => check::check(arg(case, 0), arg(case, 1), arg(case, 2), &text_of(case)),
        "yaml_plain_type" => check::yaml_plain_type(arg(case, 0)),
        "schema_constants" => check::schema_constants(),
        "roundtrip" => check::roundtrip(
            arg(case, 0),
            arg(case, 1),
            arg(case, 2),
            arg(case, 3),
            arg(case, 4),
        ),
        other => json!({ "unknown fn": other }),
    }
}

#[test]
fn the_shared_vectors_give_the_recorded_answers() {
    let raw = std::fs::read_to_string(FILE).unwrap_or_default();
    let mut doc: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let update = std::env::var_os("NEMO_CORE_UPDATE_VECTORS").is_some();
    let cases = doc["cases"]
        .as_array_mut()
        .map(std::mem::take)
        .unwrap_or_default();
    assert!(
        cases.len() > 100,
        "the vector file is missing or short: {FILE}"
    );
    let mut out = Vec::with_capacity(cases.len());
    let mut wrong = Vec::new();
    for mut case in cases {
        let got = run(&case);
        if update {
            case["expect"] = got;
        } else if case["expect"] != got {
            wrong.push(format!(
                "{}: expected {} got {}",
                case["name"], case["expect"], got
            ));
        }
        out.push(case);
    }
    if update {
        doc["cases"] = Value::Array(out);
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
        assert!(
            std::fs::write(FILE, format!("{text}\n")).is_ok(),
            "cannot write {FILE}"
        );
    }
    assert!(
        wrong.is_empty(),
        "{} vectors differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// A few answers the vectors pin, spelled out so a regenerated file cannot quietly change them.
#[test]
fn the_vectors_pin_the_behaviour_that_matters() {
    let v = |system: &str, file: &str, format: &str, text: &str| {
        check::check(system, file, format, text)["diagnostics"].clone()
    };
    // the drift the JS copy had: an inner quote does not end the value, so the backend refuses it
    assert_eq!(
        v("dcgm", "counters", "text", "token: abc\"defghijk\n")[0]["source"],
        "secret"
    );
    // Unicode case folding: `ſ` (long s) matches `s` under (?i), natively and in the browser
    assert_eq!(
        v("dcgm", "counters", "text", "\u{17f}ecret: abcdefghijkl\n")[0]["source"],
        "secret"
    );
    // the look-behind: a prefix glued to a letter is not a token
    assert_eq!(
        v("dcgm", "counters", "text", "a: xsk-abcdefghijk\n"),
        json!([])
    );
    // the nesting budget: 496 arrays load, 497 are Python's RecursionError
    let deep = |n: usize| format!("a = {}{}\n", "[".repeat(n), "]".repeat(n));
    assert_eq!(
        check::check("relay", "plugins", "toml", &deep(496))["syntax"],
        "ok"
    );
    assert_eq!(
        check::check("relay", "plugins", "toml", &deep(497))["syntax"],
        "error"
    );
}
