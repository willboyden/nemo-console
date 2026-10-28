//! `services::pytoml` limits the CPython 3.13 goldens (tests/switchyard_tomllib.rs) do not reach: nesting is bounded
//! like Python's recursion limit instead of by the Rust stack (an unbounded descent aborted the process), and ints
//! follow Python's 4300-digit `int()`/`str()` limit without quadratic work. PORTING.md A-6, A-7, DEVIATION D-12.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nemo_console::services::pytoml::{TomlVal, loads};

/// Run on a thread with tokio's default worker stack (2 MiB): the size the request handlers parse on.
fn on_worker_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn err(doc: &str) -> Option<String> {
    loads(doc).err().map(|e| e.0)
}

const RECURSION: &str = "maximum recursion depth exceeded";

#[test]
fn nesting_limits_match_python_called_from_a_shallow_stack() {
    on_worker_stack(|| {
        // measured with CPython 3.13.14's tomllib.loads at module level: the deepest accepted documents
        let arrays = |n: usize| format!("a = {}{}", "[".repeat(n), "]".repeat(n));
        let inline = |n: usize| format!("a = {}1{}", "{b = ".repeat(n), "}".repeat(n));
        let mixed = |n: usize| format!("a = {}1{}", "[{b = ".repeat(n), "}]".repeat(n));
        assert_eq!(err(&arrays(496)), None);
        assert_eq!(err(&arrays(497)).as_deref(), Some(RECURSION));
        // what `except tomllib.TOMLDecodeError` would let escape keeps Python's class
        assert_eq!(
            loads(&arrays(497)).err().map(|e| e.class()),
            Some("RecursionError")
        );
        assert_eq!(
            loads("a = [").err().map(|e| e.class()),
            Some("TOMLDecodeError")
        );
        assert_eq!(err(&inline(330)), None);
        assert_eq!(err(&inline(331)).as_deref(), Some(RECURSION));
        assert_eq!(err(&mixed(198)), None);
        assert_eq!(err(&mixed(199)).as_deref(), Some(RECURSION));
    });
}

#[test]
fn absurd_nesting_is_an_error_not_a_stack_overflow() {
    on_worker_stack(|| {
        for doc in [
            format!("a = {}", "[".repeat(200_000)),
            format!("a = {}", "{b = ".repeat(200_000)),
            format!("x = 1\n[t]\nv = {}", "[{a=".repeat(100_000)),
        ] {
            assert_eq!(err(&doc).as_deref(), Some(RECURSION));
        }
        // the budget is released as levels close: many shallow siblings are fine
        let siblings = format!("a = [{}]", vec!["[[1]]"; 5_000].join(", "));
        assert_eq!(err(&siblings), None);
    });
}

#[test]
fn ints_follow_pythons_4300_digit_limit() {
    let digits = |n: usize| "1".repeat(n);
    assert!(
        matches!(&loads(&format!("a = {}", digits(4300))).unwrap()["a"], TomlVal::BigInt(s) if s.len() == 4300)
    );
    let want = "Exceeds the limit (4300 digits) for integer string conversion: value has 4301 digits; \
                use sys.set_int_max_str_digits() to increase the limit";
    assert_eq!(err(&format!("a = {}", digits(4301))).as_deref(), Some(want));
    assert_eq!(
        loads(&format!("a = {}", digits(4301)))
            .err()
            .map(|e| e.class()),
        Some("ValueError")
    );
    // date(0, 1, 1) is a ValueError inside tomllib: "Invalid date or datetime"
    assert_eq!(
        err("bad = 0000-01-01\n").as_deref(),
        Some("Invalid date or datetime (at line 1, column 7)")
    );
    assert_eq!(
        err(&format!("a = -{}", digits(4301))).as_deref(),
        Some(want)
    );
    // underscores are not digits (Python counts 4301 here too)
    assert_eq!(
        err(&format!("a = 1{}", "_1".repeat(4300))).as_deref(),
        Some(want)
    );
    // 0x/0o/0b: Python loads any size; past ~4300 decimal digits only its str() fails, so the text is a placeholder
    let huge = loads(&format!("a = 0x{}", "f".repeat(1_000_000))).unwrap();
    assert!(matches!(&huge["a"], TomlVal::BigInt(s) if s == "<int with more than 4300 digits>"));
    // leading zeros do not count
    let small = loads(&format!("a = 0x{}1", "0".repeat(100_000))).unwrap();
    assert_eq!(small["a"], TomlVal::Int(1));
    // 16**3000 = 2**12000: exact (3613 digits), computed
    let exact = loads(&format!("a = 0x1{}", "0".repeat(3000))).unwrap();
    assert!(
        matches!(&exact["a"], TomlVal::BigInt(s) if s.len() == 3613 && s.starts_with("2290") && s.ends_with("6"))
    );
}
