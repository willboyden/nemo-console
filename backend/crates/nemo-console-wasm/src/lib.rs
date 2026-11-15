//! Nemo Console's config validators for the browser (ADR-0022): thin `wasm-bindgen` exports over
//! `nemo_console_core::check`. Strings in, JSON strings out; the contract is `backend/PORTING.md` addendum A-10 and the
//! JSON shapes are documented on each function in `nemo_console_core::check`. Nothing here touches the network, the
//! DOM, a clock or storage. A panic aborts (profile `release-wasm`): the page sees a trap, reports "live checks
//! unavailable" and instantiates the module again.

use nemo_console_core::check as core;
use wasm_bindgen::prelude::wasm_bindgen;

/// The contract version (`api` in every answer).
#[wasm_bindgen]
#[must_use]
pub fn api_version() -> u32 {
    core::API
}

/// Every live check for one draft: `{api, schema, server_adds, syntax, diagnostics}`.
#[wasm_bindgen]
#[must_use]
pub fn check(system: &str, file_id: &str, format: &str, text: &str) -> String {
    core::check_json(system, file_id, format, text)
}

/// The schema constants the Form view describes fields with.
#[wasm_bindgen]
#[must_use]
pub fn schema_constants() -> String {
    core::schema_constants_json()
}

/// How PyYAML types a plain scalar: `{api, type, text}`.
#[wasm_bindgen]
#[must_use]
pub fn yaml_plain_type(raw: &str) -> String {
    core::yaml_plain_type_json(raw)
}

/// After a Form edit: does the value read back as typed, and did nothing else change? `{api, ok, problem, got}`.
#[wasm_bindgen]
#[must_use]
pub fn roundtrip(format: &str, before: &str, after: &str, path: &str, expected: &str) -> String {
    core::roundtrip_json(format, before, after, path, expected)
}
