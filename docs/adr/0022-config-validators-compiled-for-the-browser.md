# ADR-0022: The config editor runs the backend's validators, compiled to WebAssembly

Status: Accepted (Unreleased, after 0.10.0). Amends ADR-0013 (one build step for the frontend, and `script-src`
gains `'wasm-unsafe-eval'`) and ADR-0019 (the live checks are no longer a hand-written mirror).

## Context

ADR-0019 gave the config editor live checks that copy the backend's rules into JavaScript: four schemas,
Switchyard's cross-references and `base_url` rules, the save path's secret patterns, PyYAML's typing of plain
scalars, and the Form writer's quoting rules. ADR-0019 said what that costs: two copies of each rule, drift tests
that "cannot prove the copies agree on every input", a test that regex-scraped constants out of the Rust source,
and an e2e mirror that compared only "has errors or not". The copies had already drifted:

- the 0.9.0 audit fixed Form-writer bugs (NEL/LS/PS/DEL escaping, bare `=` and `<<`, `-.5`, shadowed duplicate
  keys), each a place where the JavaScript disagreed with PyYAML;
- `constructor` / `toString` keys hit `Object.prototype` in the JavaScript schema lookup;
- the secret scan's key=value pattern let `token: abc"defghijk` through in the browser while the backend
  refused it (found while planning this change; now a test on both sides).

The backend already has exact ports of tomllib and PyYAML, the schema validators and the secret scan, in pure
Rust.

## Decision

1. **The pure part of the backend becomes its own crate**, `backend/crates/nemo-console-core`: the tomllib and
   PyYAML ports, `api::pyjson`, the secret scan, `Severity` / `Diagnostic` / `AdapterError`, and the config-file
   schemas of AIPerf, Model Optimizer, Evaluator, Switchyard and Relay, moved out of the adapters without
   editing them. It has no async runtime, file system, clock, process or network access. `nemo-console`
   re-exports every item at its old path.
2. **A small `wasm-bindgen` module**, `backend/crates/nemo-console-wasm`, exports four functions: `check`,
   `schema_constants`, `yaml_plain_type` and `roundtrip`. Strings in, one JSON document out, each carrying a
   contract version (`backend/PORTING.md` addendum A-10). No framework, no `web-sys` / `js-sys`, `wasm-bindgen`
   pinned to exactly 0.2.129 (already in `Cargo.lock`, past the 14-day cooldown).
3. **The editor takes its verdicts from the module** (`frontend/js/cfg-wasm.js`, loaded when a Config tab opens):
   syntax from the loader the server runs (PyYAML `safe_load` with its constructors, tomllib), worded as the file's
   server validator words it, the schema findings and the save-path refusals with the server's exact texts.
   JavaScript keeps only what has no server rule (line endings, look-alike and trailing spaces, duplicate and
   boolean-looking keys) and the Form's field descriptions, whose numbers come from `schema_constants`.
4. **Form edits are read back (stage 2).** After each Form edit, `roundtrip` loads the text before and after with
   the real loaders; the edit is written only when the value reads back as typed and nothing else changed. Plain
   YAML values are typed by PyYAML's own resolver (`yaml_plain_type`).
5. **No fallback copy.** While the module first loads the editor says so ("loading live checks"). When it cannot
   load or traps (a Rust panic aborts), the editor shows "live checks unavailable", the YAML Form view waits, Form
   edits are refused with a pointer to the Text view, and a fresh instance is started (and, if that one traps too,
   again on the next edit). The server's validator still runs on Check and before every Apply.
6. **One build step**, `make wasm`: `cargo build --target wasm32-unknown-unknown --profile release-wasm`, then the
   `wasm-bindgen` CLI (`--target web`), into `frontend/js/wasm/`, which is generated and never committed. It
   refuses a CLI of another version than the crate, a build-machine path in the module or its glue (the build
   uses `--remap-path-prefix`), and a module above 1,500,000 bytes. `build`, `release`, `e2e` and
   `frontend-test` depend on it; CI installs `wasm-bindgen-cli@0.2.129` and runs it.
7. **CSP.** `script-src 'self' 'wasm-unsafe-eval'`: the page may compile WebAssembly from its own origin, and
   nothing else changes. `eval`, `new Function` and inline script stay forbidden (`static-guards.test.mjs` also
   scans the generated glue). For the parity harness this is deviation D-34, removed from the Rust side's header
   by one documented normalisation rule.

What stays the same: the API contract v1 (no route, no field), the server as the authority (ADR-0019 section 3),
the static allowlist (the module is served from `js/` as `application/wasm`), zero runtime npm dependencies.

## Consequences

- One copy of each rule. The browser's verdict is the backend's: `frontend/tests/cfg-wasm.test.mjs` runs the
  shared vectors (`backend/crates/nemo-console-core/tests/vectors/check.json`, also checked natively) through the
  built module and needs exactly the native answers, and `frontend/e2e/config-editor.mjs` compares the module with
  the server's validate route diagnostic by diagnostic (severity, line, text, the 422 text for a secret, a 500 as a
  named exception) on six files, one of them without a schema.
- The browser now checks YAML the Form's layout parser cannot model (anchors, aliases, tags, several documents)
  with the real loader, so live syntax and secret checks work for every TOML and YAML file.
- The Form writer's bug class from 0.9.0 is checked on every edit instead of once, offline.
- The frontend has a build step for this one module. A fresh clone needs the `wasm32-unknown-unknown` target
  (`backend/rust-toolchain.toml` installs it) and `cargo install --locked wasm-bindgen-cli@0.2.129`. Without
  `make wasm` the page still works and says the live checks are unavailable; the frontend tests that need the
  module fail rather than skip.
- Size: 1,354,745 bytes (495,098 gzip -9) at the time of writing, loaded once per page over loopback, only when a
  Config tab opens. Byte-identical across clean rebuilds on the same toolchain.
- `script-src` is one token wider. `'wasm-unsafe-eval'` allows compiling WebAssembly, not evaluating script text;
  the module comes from the console's own origin, takes only the text the page already holds, makes no network
  call and holds no key.
- wasm32 has no threads, so the TOML parser parses deep documents in place and the module is linked with an
  8 MiB stack; the nesting limit is Python's frame budget on both builds (vectors at 496 and 497 levels).
- A release stays a tag plus its notes (`docs/RELEASING.md` section 3), with no binary attached: the module is built
  from source on the machine that runs the console, like the backend.

## Alternatives considered

- **Keep the JavaScript mirror and better drift tests.** Rejected: the drift tests cannot cover every input, and
  every rule change still had to be made twice.
- **A Leptos (or other Rust framework) frontend.** Rejected: several MB of bundle, a framework and about a hundred
  crates for a UI that works, and it would not remove the duplication by itself; the validators would still have
  to move into a shared crate, which is this decision.
- **A schema or check endpoint on the backend.** Rejected again for the reasons in ADR-0019: a new route changes
  the contract the parity harness checks, and a POST per keystroke writes audit rows and, for some validators,
  starts processes.
- **WebAssembly for the trace explorer, the diff or the charts.** Not done: their cost is the DOM, not
  computation, and moving trace parsing into the browser would put conversation content there.
