# Nemo Console: Python -> Rust backend port (contract)

Status: **fixed 2026-10-27** (phase 1). Base: `main` aa2f810, branch `rust-backend`.
The general porting rules apply. This file adds the app-specific
contract. **Changes after lock are numbered, additive addenda only** (section 15); never rename or retype a locked
item.

The Python backend (`backend/src/nemo_console`, FastAPI, 14.7k lines) is the reference. The Rust crate
(`backend/crates/nemo-console`, lib `nemo_console` + bin `nemo-console`) must be a drop-in replacement: same HTTP API,
same CLI, same files, same metrics, same argv. The frontend (`frontend/`) is the API contract and does not change.

## 0. How to build and check

```sh
cd backend
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=8   # workstreams: also CARGO_TARGET_DIR=$PWD/target/<ws>
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
cargo deny check && cargo audit
node tools/gen-test-map.mjs                 # regenerate TEST-MAP.md (never edit it by hand)
node parity/check-test-map.mjs              # unmapped = 0 is the bar (--allow-unmapped during the port;
                                            #   --no-pending: no `#[ignore = "pending WS-X"]` left, the merge gate)
node parity/run.mjs --a python --b rust     # differential harness (parity/README.md; --only <regex>)
node parity/xstate.mjs                      # each implementation reads the state the other wrote (addendum A-2)
node parity/launcher.mjs                    # open/stop across implementations (addendum A-2)
node parity/startup.mjs                     # bad settings: same exit codes and messages (addendum A-2)
cd ../frontend && E2E_BACKEND=rust node e2e/run.mjs   # browser suite against the Rust binary (default: python)
```

## 1. Decision table: Python mechanism -> Rust

| Python | Rust (crate) | Where | Notes |
|---|---|---|---|
| FastAPI + Starlette + uvicorn | axum 0.8 (one `fallback` handler) + hyper, tokio | `src/api/mod.rs` | Routing is done by hand (`ROUTES` table, Starlette order, decoded path) so 404/405/HEAD/route-label semantics match exactly. |
| `SecurityMiddleware` (pure ASGI) | `api::handle` (pre-check, auth, headers, metrics, audit around every request) | `src/api/mod.rs` | Header set/order and messages pinned by tests/fixtures/api_golden.json. |
| FastAPI dependencies (`Depends`) | explicit calls in each handler, in Python's declaration order (`dep_adapter`, `dep_id`, `dep_viewer`, `dep_admin`) | `src/api/{observe,configure,act,admin}.rs` | Order matters: body JSON parse -> deps (raise at once) -> query/body validation (collected) -> handler. |
| pydantic request models + `RequestValidationError` | hand-written validators with pydantic's texts/locs (`api::bodies`), lax coercions (`services::pyvalidate`) | `src/api/bodies.rs` | `"; ".join(f"{loc}: {msg}" for err in errors[:5])`, 422 `invalid_request`. Golden: 126 cases. |
| `json.loads` of the body (NaN/Infinity, char positions, 400 on non-text) | `api::pyjson::loads` | `src/api/pyjson.rs` | `body.<pos>: JSON decode error`; FastAPI 0.141 `strict_content_type`: no Content-Type = raw bytes. |
| pydantic response models (`model_dump`) | serde structs, `Option` -> `null`, `f64` for every Python float | `src/domain/models.rs` | serde_json with `preserve_order` + `float_roundtrip`. |
| pydantic-settings (`NEMO_CONSOLE_*`, dotenv, `__` nesting, JSON for complex fields) | `config::Settings::from_sources` (hand-written, same precedence, lax coercion, field validators) | `src/config.rs` | Startup error -> stderr + exit 1. |
| `services/common.redact` (`re` with look-arounds) | fancy-regex 0.19 (same patterns), char-based chunking | `src/services/common.rs` | Linear: 4000-code-point chunks. |
| `asyncio.create_subprocess_exec` runner | tokio::process + nix (killpg) | `src/services/runner.rs` | Process GROUP instead of new session (D-3). |
| httpx.AsyncClient (trust_env=False, no redirects, 10 s / 3 s) | reqwest 0.13 rustls, `no_proxy`, redirect none, `services::http::Http` | `src/services/http.rs` | Errors carry the httpx class name (`ConnectError`, `ReadTimeout`, ...). |
| sqlite3 (WAL, one connection + lock) | rusqlite 0.40 bundled, `Mutex<Connection>` | `src/services/store.rs` | Same file, same schema, migrations embedded. |
| `StreamingResponse` (SSE) | `Body::from_stream(async_stream)` | `src/services/events.rs` | Same frames, heartbeats, idle timeout, shutdown event. |
| asyncio background task (metrics poller, capable probe, adapter runs) | `tokio::spawn` | `server.rs`, `engines.rs`, adapters | Adapters own their run tasks (aiperf/dd/evaluator/skilleval). |
| prometheus_client `generate_latest` | hand-written exposition (byte-identical incl. `_created`, `le` first, Go floats) | `src/services/telemetry.rs` | |
| `os.environ` (+ monkeypatch in tests) | `services::environ::Environ` (process env, or a fixed map in tests) | `src/services/environ.rs` | `std::env::set_var` is unsafe in edition 2024 and `unsafe_code = forbid`. |
| `logging` JSON formatter | tracing + a JSON layer `{"ts","level","logger","msg"}` (redacted) | `src/server.rs` | Logger = module path with `.`. |
| `desktop.py` (`open`/`stop`) | `desktop.rs`, same messages/exit codes; server child = `<exe> -I -m nemo_console.main` | `src/desktop.rs` | See D-4. |
| CLI `nemo-console [mint-key|open|stop]` | manual argv dispatch (unknown args = serve, like Python) | `src/main.rs` | clap is not used: it would reject what Python accepts. |
| `tomllib` | `services::pytoml`: a hand port of CPython 3.13's `tomllib` (A-6) | `src/services/pytoml.rs` | At lock this row said the `toml` crate (TOML 1.1, risk R-3); A-6 replaced it and the crate left Cargo.toml (A-7). |
| PyYAML `safe_load`/`safe_dump` | `services::pyyaml`: an exact port of PyYAML 6.0.3 (loader, resolver, constructors, safe dumper; A-1, A-4, A-7) | `src/services/pyyaml/` | At lock this row said `yaml-rust2` (risk R-2); replaced, and the crate left Cargo.toml (A-7). |
| `shlex.split` | shlex 2.0 | workstream A (dynamo) | verify against Python on the dynamo fixtures. |
| `difflib.get_close_matches` | hand port of SequenceMatcher (no crate) | workstream B (dcgm), workstream B (aiperf `_close`) | Risk R-4. |
| `hashlib.md5` (Starlette ETag) / sha256 | md-5 0.11 / sha2 0.11 | static files / everywhere | |
| `secrets.token_urlsafe`, `uuid4().hex` | getrandom 0.4 + base64 url-safe no pad, uuid 1.27 | `services/auth.rs` | |
| `email.utils.formatdate(usegmt)` | httpdate | static files | |
| Python `repr(float)`, `repr(str)`, `json.dumps` | `services::pyfmt` | `src/services/pyfmt.rs` | Use these for every text Python builds with `{x}` / `{x!r}` / `json.dumps`. |

Crates (declared in `backend/Cargo.toml` + the crate manifest; versions obey the 14-day cooldown, A-7): anyhow,
async-stream, async-trait, axum, base64, bytes, chrono (clock; A-2), fancy-regex, futures, getrandom, hex, http,
http-body-util, httpdate, hyper, hyper-util (tokio, server-graceful; A-9), indexmap, md-5, nix, percent-encoding, regex,
reqwest (json, rustls, stream, query), rusqlite (bundled), serde, serde_json (float_roundtrip, preserve_order), sha2,
shlex, signal-hook (A-2), subtle, tempfile, thiserror, time, tokio, tokio-stream, tokio-util, tower, tracing,
tracing-subscriber, url, uuid; dev: proptest, tempfile, wiremock. The lock-time list also had `toml` and `yaml-rust2`
(removed in A-7) and `clap` (declared, never used; removed in A-9). **No workstream edits Cargo.toml.** A crate that is
truly missing is requested from the integrator (addendum).

## 2. Module map (every Python file -> Rust file, ONE owner)

| Python | Rust | Owner |
|---|---|---|
| `main.py` (app factory, static files, `run`, `mint_key`, ConsoleServer) | `src/api/mod.rs` (`app`, `handle`, ROUTES), `src/api/static_files.rs`, `src/server.rs`, `src/main.rs` | core |
| `config.py` | `src/config.rs` | core |
| `container.py` | `src/container.rs` | core |
| `desktop.py` | `src/desktop.rs` | core |
| `metrics_keys.py` | `src/api/observe.rs` (`KEY_RE`) | core |
| `api/errors.py` | `src/api/mod.rs` (`problem`, `validation_error`), `src/services/errors.rs` | core |
| `api/deps.py` | `src/api/mod.rs` (`dep_*`, `exclusive`) | core |
| `api/security.py` | `src/api/mod.rs` (`handle`, `Precheck`, headers, audit) | core |
| `api/routers/observe.py` | `src/api/observe.rs` | core |
| `api/routers/configure.py` | `src/api/configure.rs` | core |
| `api/routers/act.py` | `src/api/act.rs` | core |
| `api/routers/admin.py` | `src/api/admin.rs` | core |
| (FastAPI body/query validation) | `src/api/bodies.rs`, `src/api/pyjson.rs`, `src/services/pyvalidate.rs` | core |
| `domain/models.py` | `src/domain/models.rs` | core |
| `adapters/__init__.py` | `src/adapters/mod.rs` | core |
| `adapters/base.py` | `src/adapters/base.rs` | core |
| `services/audit.py` | `src/services/audit.rs` | core |
| `services/auth.py` | `src/services/auth.rs` | core |
| `services/common.py` | `src/services/common.rs` | core |
| `services/config_store.py` | `src/services/config_store.rs` | core |
| `services/engines.py` | `src/services/engines.rs` (+ `src/adapters/switchyard_capable.rs` for `capable_models`) | core |
| `services/errors.py` | `src/services/errors.rs` | core |
| `services/events.py` | `src/services/events.rs` | core |
| `services/metrics.py` | `src/services/metrics.rs` | core |
| `services/missing.py` | `src/services/missing.rs` | core |
| `services/params.py` | `src/services/params.rs` | core |
| `services/playground.py` | `src/services/playground.rs` | core |
| `services/routing.py` | `src/services/routing.rs` | core |
| `services/runner.py` | `src/services/runner.rs` | core |
| `services/settings.py` | `src/services/settings.rs` | core |
| `services/status.py` | `src/services/status.rs` | core |
| `services/store.py` + `migrations/0001_init.sql` | `src/services/store.rs` + `migrations/0001_init.sql` (copy) | core |
| `services/telemetry.py` | `src/services/telemetry.rs`, `src/server.rs` (logging) | core |
| `services/topology.py` | `src/services/topology.rs` | core |
| (new) | `src/services/{http,environ,pyfmt}.rs` (and, until A-9, `src/adapters/stub.rs`) | core |
| `adapters/switchyard.py` | `src/adapters/switchyard.rs` (+ `switchyard/`) | workstream A |
| `adapters/relay.py` | `src/adapters/relay.rs` (+ `relay/`) | workstream A |
| `adapters/dynamo.py` | `src/adapters/dynamo.rs` (+ `dynamo/`) | workstream A |
| `adapters/dcgm.py` | `src/adapters/dcgm.rs` (+ `dcgm/`) | workstream B |
| `adapters/aiperf.py` | `src/adapters/aiperf.rs` (+ `aiperf/`) | workstream B |
| `adapters/guardrails.py` | `src/adapters/guardrails.rs` (+ `guardrails/`) | workstream B |
| `adapters/openshell.py` | `src/adapters/openshell.rs` (+ `openshell/`) | workstream B |
| `adapters/datadesigner.py` | `src/adapters/datadesigner.rs` (+ `datadesigner/`) | workstream C |
| `adapters/evaluator.py` | `src/adapters/evaluator.rs` (+ `evaluator/`) | workstream C |
| `adapters/skillevaluator.py` | `src/adapters/skillevaluator.rs` (+ `skillevaluator/`) | workstream C |
| `backend/tests/core/*` | `tests/core_*.rs`, `tests/common/mod.rs`, `tests/core_api_golden.rs`, `tests/core_routes_table.rs` | core |
| `backend/tests/<sys>/*` (+ fixtures) | `tests/<sys>_*.rs`, `tests/fixtures/<sys>/**` | that system's WS |

A workstream owns ONLY: `src/adapters/<mod>.rs`, `src/adapters/<mod>/**`, `tests/<mod>_*.rs`,
`tests/fixtures/<mod>/**`, and its `tests/<ws>_core_*.rs` (section 10). Everything else is core (the integrator).

## 3. Routes (32 rows; generated table: `parity/routes.python.json`, checked by `tests/core_routes_table.rs`)

All under `/api/v1` except `/metrics` and the static catch-all. Middleware guard = bearer required on `/api/*` and
`/metrics` except `/api/v1/health`. Matching order = this order (Starlette); a path that matches with another
method answers 405 `{"error":{"code":"method_not_allowed","message":"Method Not Allowed"}}` with no Allow header;
because the GET catch-all matches every path, every non-GET to an unknown path is a 405, never a 404.

| Method | Path | Role dep | Body model | Python handler | Metric `route` label |
|---|---|---|---|---|---|
| GET, HEAD | /api/v1/openapi.json | (none; bearer by middleware) | | FastAPI openapi (served from `assets/openapi.json`, `tools/gen_openapi.py`) | `unmatched` |
| GET | /api/v1/health | public | | observe.health | /health |
| GET | /api/v1/overview | viewer | | observe.overview | /overview |
| GET | /api/v1/topology | viewer | | observe.topology | /topology |
| GET | /api/v1/systems | viewer | | observe.systems | /systems |
| GET | /api/v1/systems/{system_id}/status | viewer | | observe.system_status | /systems/{system_id}/status |
| GET | .../metrics | viewer | | observe.system_metrics | (router-relative path) |
| GET | .../metrics/history `?window=15m&key=` | viewer | | observe.metrics_history | |
| GET | .../events `?limit=1..500 (100)` | viewer | | observe.events | |
| GET | .../events/stream (SSE) | viewer | | observe.events_stream | |
| GET | .../runs/{run_id}/records `?limit=1..200 (50)&offset>=0` | viewer | | observe.dd_run_records | |
| GET | .../runs/{run_id}/files | viewer | | observe.dd_run_files | |
| GET | .../config | viewer | | configure.config_list | |
| GET | .../config/{file_id} | viewer (viewers get `redact(keep_env_names)` content) | | configure.config_get | |
| POST | .../config/{file_id}/validate | admin | ValidateBody | configure.config_validate | |
| POST | .../config/{file_id}/apply | admin | ApplyBody | configure.config_apply | |
| GET | .../config/{file_id}/history | viewer | | configure.config_history | |
| POST | .../config/{file_id}/rollback | admin | RollbackBody | configure.config_rollback | |
| GET | .../params | viewer | | configure.params_get | |
| POST | .../params/preview | admin | ParamsBody | configure.params_preview | |
| PUT | .../params | admin | ParamsPutBody | configure.params_put | |
| GET | .../actions | viewer | | act.actions_list | |
| POST | .../actions/{action} | viewer (+ ActionSpec.role) | ActionBody | act.action_run | |
| GET | .../playground | viewer | | act.playground_spec | |
| POST | .../playground/chat | admin | ChatBody | act.playground_chat | |
| GET | /api/v1/routing/summary `?window=1h` | viewer | | admin.routing | /routing/summary |
| GET | /api/v1/audit `?limit=1..500&system=` | admin | | admin.audit | /audit |
| GET | /api/v1/settings | viewer (state_dir hidden from non-admins) | | admin.settings_get | /settings |
| PUT | /api/v1/settings | admin | SettingsBody | admin.settings_put | /settings |
| GET | /metrics | viewer | | admin.console_metrics (`text/plain; version=0.0.4; charset=utf-8`) | /metrics |
| GET | /{path:path} | public | | main.static | /{path:path} |

`route` label when nothing matched: `unmatched` for guarded paths, `static` otherwise. Methods outside
GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS are labelled `OTHER`. HEAD answers carry headers and no body.

Error envelope (every failure, including framework ones): `{"error":{"code":..., "message": redact(msg)[:1200]}}`.
Codes by status for framework errors: 400 bad_request, 401 unauthorized, 403 forbidden, 404 not_found, 405
method_not_allowed, 409 conflict, 413 payload_too_large, 421 misdirected_request, 422 invalid_request, 429
too_many_requests, 502 bad_gateway, 503 unavailable. Middleware denials prefix the message with the code in words
(`unauthorized: a valid bearer API key is required`). The frontend reads `error.message` / `error.code`
(`frontend/js/api.js`, `sse.js`) and shows the message for 401/403/409/413/415/421/429/502/503 (`errors.js`).

## 4. Settings (env `NEMO_CONSOLE_*`, file `~/.config/nemo-console/env`)

Every field of the Python `Settings` class, same name/type/default (src/config.rs `FIELDS` lists them in order):
host `127.0.0.1`, port 8793, state_dir `~/.local/share/nemo-console`, repo_root (walk up from the executable to a
dir holding `inference/dynamo/docker-compose.yml`, else CWD), frontend_dir (checkout's `frontend/`), allowed_hosts [],
urls (8 defaults, merged), allowed_url_hosts [127.0.0.1, localhost, ::1], playground_key_env {switchyard, relay:
LITELLM_API_KEY} (merged), engine_probes (5, merged), engine_start (5, merged), topology_engines (5), dynamo_opt_in
true, dynamo_start, default_model_start, dynamo_exclusive_engines/coexisting/ignored/dark (lists REPLACE), trust_loopback
false, background_tasks true, poll_interval_s 2.0, ring_seconds 900, status_ttl_s 2.0, adapter_timeout_s 8.0,
action_timeout_s 900.0, action_timeouts {}, body_cap_bytes 1 MiB, config_cap_bytes 256 KiB, sse_max_connections 8,
sse_idle_timeout_s 300.0, sse_heartbeat_s 15.0, shutdown_grace_s 5 (1..60), playground_max_tokens 4096,
revisions_keep 50, otlp_endpoint null, extra_bin_dirs [~/.local/bin], dd_scripts_dir, dd_datasets_root,
dd_config_dir, dcgm_exporter_container, dcgm_diag_image, dcgm_prometheus_jobs, openshell_containers,
guardrails_container, switchyard_container/compose_project/compose_service/image/log_path, relay_unit,
dynamo_compose_project, dynamo_services/containers (merged, validated, roles frontend/worker/mocker/sv_frontend),
guardrails_compose_project, dcgm_compose_project, layout (12 keys, merged, validated), capable_probe false,
capable_probe_interval_s 300 (300..86400), capable_probe_timeout_s 60 (2..300), capable_probe_retry_s 30 (0..300),
capable_probe_key_env LITELLM_API_KEY (credential-name rule).

Sources: defaults < env file (dotenv: `export`, quotes, ` #` comments; skipped when `NEMO_CONSOLE_NO_USER_ENV` is set)
< process env < init values (tests). Names case-insensitive. list/dict/model fields take JSON
(`error parsing value for field "<f>" from source "EnvSettingsSource"` otherwise). `NEMO_CONSOLE_<FIELD>__<KEY>`
sets one key of a dict field, deep-merged across sources. Lax coercion (`yes/on/t/1`, `8_000`, ` 80 `). Field
validator messages are Python's (`Value error, not a container name: '-v'`). Invalid settings: the pydantic-style
report on stderr, exit 1 (D-1). `resolved_paths()` gives the adapters the same 37 named absolute paths. Only `urls`,
`poll_interval_s` and `playground_key_env` are mutable at runtime (`PUT /settings`, stored as overrides).

## 5. Persisted state (Rust reads what Python wrote, writes the same)

5.1 `<state>/console.db` (0600, dir 0700), SQLite WAL, `busy_timeout=5000`, `foreign_keys=ON`; tables `keys(id, role,
sha256, created REAL, note)`, `audit(id AUTOINCREMENT, ts REAL, actor, role, method, path, system, status, params
TEXT)` with append-only triggers, `revisions(id, system, file_id, ts, actor, sha256, note, content BLOB)` (keep
`revisions_keep` newest per file, ordered `ts DESC, rowid DESC`), `rollup(system, series, bucket, avg, max, n)` (1-min
buckets, 8 days), `settings(k, v)` (`overrides` = Python `json.dumps` of {urls, playground_key_env, poll_interval_s
(float)}; `params:<system>` = saved param values), `schema_version`.
5.2 Keys: `nck_` + `token_urlsafe(32)`; id = first 12 hex of uuid4; stored as hex sha256 only; verification compares
every row in constant time. First start writes `<state>/bootstrap-admin.key` (0600, O_EXCL + fchmod).
`nemo-console mint-key [admin|viewer]` writes `<state>/minted-<role>-<kid>.key` (0600) and prints `<role> key written
to <path> (0600); it is not shown here`.
5.3 Audit `params`: Python `json.dumps` text (`", "`/`": "`, ensure_ascii) of `redact_obj(summarize_body(body))`; over
8000 chars -> `{"_truncated": true, "bytes": N, "keys": [...20 sorted]}`; unparseable body -> `"[unparseable json, N
bytes]"`; non-JSON -> `"[N bytes, non-json]"`; empty -> null. Only mutating `/api/*` calls by a non-public principal
are rows; denials are counted (`nemo_console_denied_requests_total`) and logged once a minute.
5.4 Config writes: atomic (`.<name>.<random>.tmp` in the same dir, fchmod to the old mode or 0600, fsync, rename, fsync
dir); a new file's parent is created 0700; rollback to "absent" deletes the file.
5.5 Launcher: `<state>/console.pid` and `<state>/console.log` (0600, O_NOFOLLOW, rotated at 10 MiB to `.1`).
5.6 Adapter state (owned by the workstreams, same layout as Python): `<state>/aiperf/runs/<run>/run.json`,
`<state>/dcgm/runs/<run>/run.json`, `<state>/datadesigner/{runs,tmp}`, `<state>/evaluator/{runs,tmp}`,
`<state>/skillevaluator/runs`, `<state>/guardrails/checks.jsonl` (+ validate temp dirs), `<state>/openshell/tmp`,
`<state>/dynamo/override.yml` (0600). `run.json` = `json.dumps(meta, indent=1)` written via `.run.json.tmp` + chmod 0600
+ rename: use `pyfmt::json_dumps_with(.., JsonOpts{indent: Some(1), ..})`.
5.7 User dirs created at start (0700): `dd_scripts_dir`, `dd_datasets_root` (never `~/.data-designer`).

## 6. fixed shared interfaces (implemented in the skeleton; workstreams use, never change)

6.1 `container::AppState` (+ `build_container(Settings, Overrides)`, `Overrides{adapters, runner, http, registry,
environ}`), `domain::models::*` (field-for-field the pydantic models).
6.2 Errors: `services::errors::ApiError{status, code, message}`, `api::problem`, `api::validation_error`,
`ApiError::not_ported` (skeleton only).
6.3 Adapter seam: `adapters::base::{SystemAdapter, AdapterContext, AdapterError, AdapterRef, EventStream,
CommandRunner, CommandResult, RunOpts, RunnerError}`; registry `adapters::build_adapter`; each module's
`pub const LABEL` and `pub fn build(ctx: &AdapterContext, cfg: &Settings) -> Result<AdapterRef, AdapterError>`
(Python's `build_adapter` + `container._configure_<sys>`: read your deployment names from `cfg`). Python exception ->
`AdapterError`: KeyError -> `UnknownId`, FileNotFoundError -> `FileNotFound`, NotImplementedError -> `NotImplemented`,
ValueError -> `Invalid`, TimeoutError -> `Timeout`, other -> `Failed{class, message}` (class = Python class name: the
core prints `f"{type(e).__name__}: {e}"`). Optional extras with defaults: `param_catalog`, `preview_params`,
`apply_params`, `param_warnings` (Dynamo), `playground`, `playground_url`, `records_page`/`run_files` (DataDesigner;
others answer `NoSuchEndpoint` -> 404). `follow_events(self: Arc<Self>, since)` returns a `'static` stream.
6.4 HTTP: `services::http::{Http, HttpResponse, HttpError}` (`ctx.http`); use `e.class` where Python prints
`type(e).__name__`; `e.is_timeout()` = `httpx.TimeoutException`.
6.5 Runner: `services::runner::{SubprocessRunner, ALLOWED_BINARIES, max_timeout, which}`; adapters call
`ctx.runner.run(&argv, RunOpts::timeout(t).max_bytes(n)[.stdin(..)][.cwd(..)])`; argv[0] is a bare allowlisted name.
6.6 SSE: `services::events::{event_stream, SseLimiter, Shutdown}`; frames `: connected`, `: keep-alive`,
`event: idle-timeout`, `event: shutdown`, `event: error`, `data: <json.dumps(redact_obj(event))>`.
6.7 Text: `services::common::{redact, redact_keep_env, redact_obj, find_secret, key_is_sensitive, sha256_hex, cap,
cap_default, KNOWN}`; `services::pyfmt::{float_repr, str_repr, list_repr, go_float, json_dumps, json_dumps_with,
JsonOpts, char_len, char_prefix, char_suffix_from}`; `services::pyvalidate`; `services::params::validate_values`.
6.8 Config files: `services::config_store::{ConfigStore, atomic_write, mtime_f64}` (the adapter never writes a config
file; `config_path` must return the allowlisted absolute path).
6.9 Metrics / registry: `services::telemetry::Telemetry`; `services::metrics::MetricsPoller`.
6.10 Settings: `config::Settings` (+ `settings_for_test`, `from_sources`), `services::environ::Environ` (never read
`std::env` in an adapter: use `ctx.environ`).
6.11 Test fixtures: `tests/common/mod.rs` (`Env`, `FakeAdapter`, `FakeRunner`, `TestReq`, `assert_secure`,
`env_with_router`, `LogCapture`) and `tests/support_b/mod.rs` (`HookRunner`, `HookAdapter`, `state_with`): use them
from any test file with `mod common;` / `mod support_b;`; changes go through the integrator. Every upstream URL of `Env`
defaults to a CLOSED port (127.0.0.1:9): a test must never reach a live lab service (the Python conftest relies on
respx intercepting the real :4000/:4010 URLs; Rust tests use wiremock).
6.12 Test seams in core (Python tests monkeypatch module globals): `desktop::Launcher` (check, start_background,
open_browser, notify, pid_cmdline, signal, timings) + `open_console_with`/`stop_console_with`;
`config::default_repo_root_from`, `config::user_env_file_in`; `SubprocessRunner::with_environ` + `stop_grace`;
`ConfigStore::set_keep`; `StatusService::clear`; `CapableProbe::{set_result, has_task, join}`;
`server::{graceful_shutdown_s, on_exit_signal}`; `container::Overrides{registry, environ}`.

## 7. Prometheus (`GET /metrics`, viewer)

`nemo_console_http_requests_total{method,route,status}` counter (+ `_created`), `nemo_console_http_request_seconds
{method,route}` histogram (default buckets, `le` first, + `_created`), `nemo_console_denied_requests_total{status}`
counter (401/403/421/429; + `_created`), `nemo_console_sse_connections` gauge. HELP texts as Python. No lab Prometheus
job scrapes it today (grep of the lab's observability config, 2026-10-27), but the names are part of the contract.

## 8. Subprocess argv (per adapter; exact argv = the Python source; the harness diffs the fake CLIs' argv logs)

Allowlist: docker, systemctl, nemo-relay, skillevaluator, skillevaluator-local, data-designer-local,
data-designer-export, openshell, aiperf-local, guardrails-local, evaluator-local. Child env = PATH (+extra bin dirs),
HOME, XDG_RUNTIME_DIR, DBUS_SESSION_BUS_ADDRESS, DOCKER_HOST/CONTEXT/CONFIG, LANG, LC_ALL (+ SKILL_EVAL_*/SKILLEVAL_*,
LOCAL_AI_LAB, LITELLM_URL for the SkillEvaluator binaries only). Caps: 600 s; 4800 s for `skillevaluator[-local]
tier3` and `evaluator-local run`; 3600 s for `data-designer-local create` and `aiperf-local profile`. SIGINT + 5 s grace
then SIGKILL of the whole group for the SkillEvaluator/DD/AIPerf/Evaluator wrappers; SIGKILL otherwise. Output capped
per stream (`max_bytes`), a runaway producer is killed. stdout/stderr are UTF-8-decoded with replacement and redacted.

| Adapter | Binaries (argv shapes; see the `.py` for every flag) |
|---|---|
| core | `docker inspect -f {{.State.Status}} <container>` (engine probe, 5 s, 200 B) |
| switchyard | `docker inspect --format ... <ctr>`, `docker exec <ctr> tail -n N <log>`, `docker run --rm -i --pull never --network none --read-only --cap-drop ALL ... --entrypoint switchyard-server <image> --config /dev/stdin --dry-run` (validate, candidate on stdin), `docker compose -f <dir>/docker-compose.yml up -d --force-recreate --no-build` |
| relay | `systemctl --user --timestamp=unix show <unit> -p ...`, `systemctl --user restart|status ...`, `nemo-relay --version|plugins list --json|agents --json|doctor --offline --json|plugins validate --json -- <id>` |
| dynamo | `docker inspect <names...>`, `docker exec <worker> python3 -c <script>`, `docker logs --timestamps ...`, `docker ps --format {{.Names}}`, `docker compose --project-directory <dir> -f ... up/down/config -q ...` |
| dcgm | `docker inspect --format ... <exporter>`, `docker image inspect --format {{.Id}} <image>`, `docker logs --since ... <exporter>`, `docker restart <exporter>`, `docker run --rm --pull never --name ... --gpus all --cap-drop ALL ... <diag image> sh -c <fixed script>`, `docker rm -f <name>` |
| aiperf | `aiperf-local --version`, `aiperf-local profile ...` (long, background task) |
| guardrails | `docker inspect ...`, `docker network inspect --format {{.Internal}} <net>`, `docker restart --time 10 <ctr>`, `docker compose -f <compose> up -d --no-build|stop`, `guardrails-local validate ...` |
| openshell | `openshell --version`, `openshell -g openshell <args>`, `openshell doctor check`, `openshell policy prove --policy <p> --credentials <tmp>`, `docker inspect ...`, `docker network inspect|connect|disconnect ...`, `docker logs --tail N <gw>`, `docker compose -f <compose> up -d|stop` |
| datadesigner | `data-designer-local --version`, `... preview <script> -n N --save-results -o <run_dir>`, `... create <script> --num-records N --dataset-name <name> ...`, validate / check-models, `data-designer-export ...` |
| evaluator | `evaluator-local --version|cached --json|suites --file <tmp>|run --suite <s> --output-dir <dir>` |
| skillevaluator | `skillevaluator-local|skillevaluator --version|tier1|tier2|tier3 ...` |

## 9. Workstreams (each in its own worktree `rust-backend-<ws>` branched from the lock commit)

| WS | Owned Rust files | Python sources (lines) | pytest ids | Core APIs used |
|---|---|---|---|---|
| core (integration) | everything not listed below | main, config, container, desktop, api/*, services/*, domain/*, adapters/{__init__,base}, metrics_keys (~4,000) | 234 core + 3 integration | - |
| workstream A | `src/adapters/{switchyard,relay,dynamo}.rs` (+ dirs), `tests/{switchyard,relay,dynamo}_*.rs`, `tests/wsa_core_*.rs`, `tests/fixtures/{switchyard,relay,dynamo}/` | adapters/switchyard.py (841), relay.py (1275), dynamo.py (1866) | 251 (switchyard 76, relay 79, dynamo 91, core 5) | base, http, runner, pyfmt, common (redact, sha256), config (names, DEFAULT_DYNAMO_*), switchyard_capable, config_store (`atomic_write` for dynamo's override), Environ |
| workstream B | `src/adapters/{dcgm,aiperf,guardrails,openshell}.rs` (+ dirs), `tests/{dcgm,aiperf,guardrails,openshell}_*.rs`, `tests/wsb_core_*.rs`, `tests/fixtures/{dcgm,aiperf,guardrails,openshell}/` | dcgm.py (1034), aiperf.py (873), guardrails.py (808), openshell.py (957) | 300 (aiperf 125, dcgm 72, openshell 59, guardrails 43, core 1) | base, http, runner, pyfmt, common, config (DCGM_* consts, regexes), Environ (see the YAML note below) |
| workstream C | `src/adapters/{datadesigner,evaluator,skillevaluator}.rs` (+ dirs), `tests/{datadesigner,evaluator,skillevaluator}_*.rs`, `tests/fixtures/{datadesigner,evaluator,skillevaluator}/` | datadesigner.py (1102), evaluator.py (810), skillevaluator.py (1008) | 148 (dd 53, evaluator 51, skilleval 44) | base, http, runner, pyfmt, common, Environ; `records_page`/`run_files` trait extras (DataDesigner) |

Note on YAML (used by workstream A dynamo, workstream B aiperf/guardrails/openshell, workstream C datadesigner/evaluator): a shared
PyYAML-compatible loader would be a shared file. Rule: each workstream keeps its YAML handling inside its own files
(a private module per adapter, built on `yaml-rust2`'s event parser with YAML 1.1 implicit resolution as PyYAML's
SafeLoader: `yes/no/on/off/y/n/true/false` bools, `~`/`null`, ints with `0x`/`0o`/`_`/sexagesimal, floats,
timestamps kept as text unless the adapter needs a datetime). After the merge the integrator folds the copies into one
`services::pyyaml` (addendum). Same for TOML error texts (`tomllib.TOMLDecodeError` "... (at line L, column C)") and
`difflib`. Python behaviour wins; record any unreachable difference as a deviation in your report.

workstream A follow-up from the core port: `tests/core/test_dynamo_policy.py::test_defaults_describe_the_lab_and_lists_replace`
also asserts that the Dynamo adapter's default `CardPolicy()` equals the policy built from the Settings defaults; the
core Rust test covers the Settings half only. workstream A adds the CardPolicy half as
`tests/wsa_core_dynamo_policy.rs::test_card_policy_defaults_equal_the_settings_defaults`.

Per-workstream definition of done: every owned pytest id `mapped` (or justified `N/A`) in TEST-MAP.md; the parity
harness shows 0 diffs on the workstream's routes (`node parity/run.mjs --a python --b rust`, both scenarios);
fmt/clippy/test clean; the e2e pages of its systems render (`E2E_BACKEND=rust`). Do not edit shared files; a
missing core API is requested from the integrator (it becomes an addendum).

## 10. Tests and TEST-MAP

`backend/TEST-MAP.md` is generated (`node backend/tools/gen-test-map.mjs`). Placement (fixed):
`tests/<dir>/test_<stem>.py::<name>[params]` -> `backend/crates/nemo-console/tests/<dir>_<stem>.rs`, `fn <name>`; a
workstream-owned test of `tests/core/` -> `tests/<wsa|wsb|wsc>_core_<stem>.rs`; integration tests (written after the merges) ->
`tests/integration_core_<stem>.rs`. One Rust test covers all parametrisations. Exceptions in the Rust file:
`// pytest-map: <id> -> <file>::<fn>` and `// pytest-na: <id> -- <reason>` (Python-only mechanics only). Fixtures
are COPIED into `tests/fixtures/<sys>/` (Python will be removed). Extra Rust-native tests are welcome (proptest for
parsers, one explicit test per security control).

## 11. Security controls (Python -> Rust -> test)

| Control | Python | Rust | Test |
|---|---|---|---|
| Host allowlist (421) | api/security.py:60 | api/mod.rs `Precheck::check` | core_security::test_bad_host_is_421, golden |
| Origin check on non-GET (403 cross_origin) | api/security.py:64 | api/mod.rs `Precheck::check`, `netloc` | core_security::test_cross_origin_post_is_403, netloc_like_urlsplit |
| CSRF header `X-Console: 1` (403) | api/security.py:66 | api/mod.rs | core_security::test_missing_csrf_header_is_403_even_with_bearer |
| Body cap: Content-Length (413) and streamed | api/security.py:69,103 | api/mod.rs `Precheck`, `read_body` | core_security::test_body_cap_content_length_and_chunked |
| Security headers on every answer; `no-store` on API | api/security.py:26,113-117 | api/mod.rs `add_security_headers` | core_security::test_headers_on_401_and_404_and_405 (+ 500, 422) |
| 500 never leaks (redacted, headers kept) | api/security.py:126 | api/mod.rs `catch_unwind` + `problem` | core_security::test_headers_on_validation_errors_and_500 |
| Bearer keys sha256 + constant time | services/auth.py:67 | services/auth.rs `authenticate` (subtle) | core_security::test_bearer_keys_roles_and_wrong_key |
| Loopback trust opt-in, off with proxy headers / non-loopback host / allowed_hosts | services/auth.py:77, config.py:413 | auth.rs `principal_for`, config.rs `effective_loopback_trust` | core_security::test_x_forwarded_for_disables_loopback_trust, core_audit_fixes::test_loopback_trust_refused_when_exposure_plausible |
| Role deps (viewer/admin, 401/403) | api/deps.py | api/mod.rs `dep_viewer`/`dep_admin` | core_roles::*, core_security::test_bearer_keys_roles_and_wrong_key |
| Key files 0600 exclusive | services/auth.py:29 | auth.rs `write_private_file` | core_security::test_keys_are_stored_hashed_and_bootstrap_file_is_0600 |
| State dir 0700, db 0600 | services/store.py:14,23 | store.rs | same |
| Static allowlist, traversal judged before resolve, symlink escape | main.py:86,109 | api/static_files.rs | core_security::test_static_allowlist_and_traversal, core_audit2::test_static_rejects_dotdot_before_resolving, core_audit_fixes::test_nul_in_static_path_is_404_not_500 |
| `Cache-Control: no-cache` + ETag/304 on static | main.py:117-130 | static_files.rs `file_response` | core_security::test_static_files_are_revalidated_on_every_load |
| OpenAPI behind auth, no Swagger/ReDoc | main.py:74 | ROUTES (openapi guarded), no /docs | core_roles::test_openapi_json_is_behind_auth, core_security::test_no_swagger_or_redoc |
| Redaction of every output (patterns + KNOWN values) | services/common.py:82 | services/common.rs | core_redaction::* |
| Secret refusal in config content and rollback targets | services/common.py:123, config_store.py:130,179 | common.rs `find_secret`, config_store.rs | core_config_store::test_secret_looking_content_is_refused, core_audit_fixes::test_rollback_validates_and_scans_the_target_revision |
| Viewer config reads redacted (env names kept) | api/routers/configure.py:63 | api/configure.rs `config_get` | core_audit2::test_viewer_gets_redacted_config_admin_gets_raw |
| Config CAS (409), symlink refusal, path-changed refusal, atomic write, auto-rollback | services/config_store.py:89-197 | config_store.rs | core_config_store::* |
| One mutation per system (409 busy) | api/deps.py:85 | api/mod.rs `exclusive` | core_actions_params::test_second_mutation_while_busy_is_409 |
| Confirm for disruptive/destructive actions and apply/rollback/params | act.py:43, configure.py:126, config_store.py:153,168 | act.rs, configure.rs, config_store.rs | core_actions_params::test_confirm_enforced_for_disruptive_and_destructive, core_config_store::test_confirm_is_required |
| Param validation against specs | services/params.py | services/params.rs | core_actions_params::test_param_validation_against_action_spec |
| Credential env-name allowlist (never values) | config.py:159, settings.py | config.rs `credential_env_allowed`, settings.rs | core_audit2::test_playground_key_env_must_be_allowlisted, core_audit2::test_env_supplied_disallowed_key_name_is_ignored |
| Loopback-only URL settings, no userinfo | services/settings.py:26 | settings.rs `validate_url` | core_observe::test_settings_roundtrip_and_validation |
| Playground: model id regex, max_tokens clamp, server-side credential, identity encoding, secret scrubbed from stream, never persisted | act.py:21, playground.py | act.rs, playground.rs | core_playground::*, core_audit_fixes::test_playground_requests_ask_for_identity_encoding |
| SSE connection cap (429) and slot release | observe.py:80, events.py | observe.rs, events.rs (`Slot` drop) | core_sse::*, core_audit_fixes::test_sse_slots_do_not_leak_on_aborted_streams |
| Audit append-only, content summarised, denials not rows | migrations, audit.py, security.py | migrations, audit.rs, api/mod.rs | core_observe::test_audit_is_append_only_and_records_mutations, core_audit_fixes::test_denied_requests_are_not_audit_rows_but_are_counted |
| Runner: allowlist, argv only, NUL/length checks, minimal env, timeouts, output cap, group kill | services/runner.py | runner.rs | core_runner::* |
| Settings identifiers reaching argv refuse `-x` | config.py:292-391 | config.rs validators | core_runner::test_addendum_98_container_and_image_settings_refuse_option_like_values, core_deployment_settings::test_invalid_identifiers_are_refused |
| Launcher: loopback URL only, all listeners ours, O_NOFOLLOW pid/log, recycled pid never signalled | desktop.py:44,81,115,226,241 | desktop.rs | core_desktop::* |
| HTTP client: no proxies from env, no redirects | container.py:37 | services/http.rs | core_audit_fixes (playground), adapters' tests |

## 12. Skeleton behaviour (phase 1) and parity status

**Historical (phase 1).** Every adapter is ported; the stub adapter, `AdapterError::NotPorted` and the 501
`not_ported` envelope were removed in A-9, and parity is the one D-6 difference (A-8). At lock, every adapter module was a stub (`adapters::stub::NotPortedAdapter`): every per-system route answers **501**
`{"error":{"code":"not_ported","message":"not ported: <module>"},"detail":"not ported: <module>"}` (the brief's
`detail` next to the app's own envelope so the UI shows the message), and the aggregate views (/health, /overview,
/systems, /topology) show the system as `unknown` with problem `console.adapter.error` (`NotPortedError`). Everything
else is implemented.

Parity at lock (2026-10-27, `node parity/run.mjs --a python --b rust --allow-diff-routes
parity/allow-diffs.txt`): 1301 cases (full 940, empty 346, loopback 15), 309 equal, 993 differing, all of
them on the routes in `allow-diffs.txt` (per-system routes answering 501, plus their knock-on effects on /health,
/overview, /systems, /topology, /routing/summary, /audit, /metrics and the whole-run argv set). Routes with 0 diffs:
GET/PUT /settings, GET/HEAD /openapi.json, every static file case, and every middleware refusal (Host, Origin, CSRF,
auth, body caps, methods). Python vs Python: 1301/1301 equal. A workstream removes its systems' effect from the
allow-list as it lands (the aggregate lines go when all ten are ported).
E2E at lock (`E2E_BACKEND=rust`): the overview, audit and settings pages render; the ten system pages fail only on 501
"not ported" answers (183 findings, all of them 501/not-ported).

## 13. Deviations (deliberate differences; each with its reason)

- **D-1 Settings error text.** Python dies with a pydantic traceback ending in `N validation error(s) for Settings`;
  Rust prints the same header and per-field `loc` + message + `[type=...]` lines without the traceback, the
  `input_value=` echo (could be a secret) and the docs URL. Exit code 1 in both.
- **D-2 OTLP.** Python only exports when optional opentelemetry packages are installed (they are not dependencies);
  Rust never exports and logs the same one warning when `otlp_endpoint` is set.
- **D-3 Child session.** Python starts children with `start_new_session=True` (setsid); Rust uses a new process group
  (`process_group(0)`): setsid between fork and exec needs `unsafe`. Group signals are identical; the console has no
  controlling terminal under the launcher, so nothing observable changes.
- **D-4 Launcher child argv.** `open` starts `<exe> -I -m nemo_console.main` (Python: `<python> -I -m
  nemo_console.main`). The Rust binary treats those args as "serve" (Python ignores unknown args too), so `stop` keeps
  Python's exact `argv[1:] == SERVER_ARGS` check and either implementation stops a console started by the other.
- **D-5 Timing of the audit row / latency metric for streamed answers.** Python's middleware finishes after the
  stream ends; Rust records when the response head is ready (status is the same; `ts`/latency are volatile anyway).
- **D-6 Non-text JSON values.** NaN/Infinity/-Infinity literals in request bodies parse (for pydantic's typed-field
  messages), but inside free-form objects (`params`, `values`) they become `null`; lone UTF-16 surrogates in strings
  become U+FFFD (Rust strings are UTF-8). Python would 500 later on such values (Starlette's JSONResponse refuses NaN).
  Params (A-6, workstream A's request): a NaN for a float param reaches the adapter in Python, whose validator refuses it
  (422 `params_invalid` "gpu_memory_utilization: nan outside [0.05, 1.0]"); in Rust the core check refuses the `null`
  first (422 `invalid_param` "param 'gpu_memory_utilization': expected float"). Keeping NaN would mean changing the
  fixed adapter value type (serde_json cannot hold a non-finite number) for one edge case; both refuse with 422, so
  the parity case `params.dynamo.preview.nan` is allowed permanently (parity/allow-diffs.txt).
- **D-7 Static multi-range requests.** Single `Range` requests, 400 and 416 match Starlette; a request for several
  ranges gets the whole file (200) instead of `multipart/byteranges` (no browser sends it for these files).
- **D-8 Invalid UTF-8 from children.** Python's `decode("utf-8", "replace")` emits one U+FFFD per bad byte; Rust's
  `from_utf8_lossy` one per maximal invalid sequence.
- **D-9 Transport layer.** uvicorn (h11) answers some malformed requests itself; Rust reproduces the one a client can
  plausibly send (HTTP/1.1 without Host, or several Host headers: 400 `text/plain; charset=utf-8` "Invalid HTTP request
  received.", no app headers, not counted). Other malformed requests get hyper's own 400. Rust answers carry hyper's
  `date` header and no `server: uvicorn` header (neither is security-relevant or read by a client).
- **D-10 Naive timestamps (workstream A).** Python's `datetime.fromisoformat(ts).timestamp()` reads an offset-less timestamp
  (Switchyard routing records, Relay ATOF events, docker times) in the host's LOCAL time zone; Rust reads it as UTC (no
  time-zone database). Same result on a UTC host; ages and ordering of offset-less records shift by the UTC offset
  elsewhere. Records with an offset or `Z` are exact.
- **D-11 Corrupt gzip rotation (workstream A, Relay).** A `.jsonl.N.gz` ATOF rotation that is not gzip, or is truncated, reads
  as an OSError "<name>: not a readable gzip file"; corrupt deflate data inside a valid member surfaces as Python's
  `zlib.error` text ("Error -3 while decompressing data: <reason>"), but the `<reason>` wording comes from the
  hand-written inflater (services/gzip.rs), not zlib's.
- **D-12 Parser limits (integration, A-6).** Python's limits depend on the caller's stack; Rust's are fixed and never crash
  the process (unbounded recursion used to abort it with a stack overflow): JSON bodies nested deeper than 512 levels
  get 400 "There was an error parsing the body" (Python: about 9,998 levels, then the same 400; neither writes an
  audit row: Python's audit middleware catches only ValueError, so its RecursionError escapes); a TOML document
  deeper than Python accepts from a shallow stack (496 arrays, 330 inline tables) or a key with more than 1,000 parts
  fails with Python's RecursionError text and `TomlDecodeError::class()` "RecursionError" (OpenShell propagates it
  like Python; Switchyard, Relay and the capable-tier check treat it as a decode error, where Python would answer
  500 on a validate route); a decimal TOML int over 4,300 digits fails with Python's ValueError (class "ValueError");
  a 0x/0o/0b int (TOML or YAML) over 14,300 bits keeps the text `<int with more than 4300 digits>` (Python loads it
  but cannot `str()` it). JSON ints over 4,300 digits are a 400 in both; YAML ints over 4,300 digits are Python's
  ValueError in both. YAML nesting past 490 levels is RecursionError (Python: 489-492, by its stack); a YAML cycle
  or an alias bomb loads (as in Python) but cannot become a `PyValue` tree (RecursionError / MemoryError for the
  adapters that read trees: Dynamo, Guardrails, OpenShell, AIPerf).
- **D-13 Big ints (workstream A).** Python ints are unbounded. JSON (request bodies, docker inspect, upstream answers) keeps
  ints within i128 exactly and turns larger ones into floats; TOML keeps any size as exact decimal text (D-12 aside);
  YAML keeps ints beyond i64 as exact decimal text (`PyValue::BigInt`).
- **D-14 Rejected Relay route fields (workstream A).** Python reports the rejected plugin-route fields in set-iteration order
  (`_PLUGIN_REJECTED & set(r)`, which varies with PYTHONHASHSEED); Rust reports them in a fixed order.
- **D-15 Malformed `docker inspect` entries (workstream A, Dynamo).** An entry of the inspect array that is not an object is
  skipped; Python's `d.get(...)` raises AttributeError out of the status call.
- **D-16 Live smoke tests (workstream A).** `test_live_smoke.py` (Switchyard, Relay, Dynamo) skip unless the live stack
  answers, and Dynamo's probes :8020 at collection time; the Rust ports run only with `NEMO_LIVE=1` (Switchyard,
  Dynamo) or `NEMO_CONSOLE_LIVE=1` (Relay) and check that before touching anything, so a hermetic `cargo test` never
  reaches the lab.
- **D-17 Data Designer copies (workstream C).** `shutil.copy2` copies extended attributes; the Rust copy keeps contents, mode
  and access/modification times only. `int(total)` beyond u64 becomes a JSON float.
- **D-18 YAML sexagesimal ints beyond i128 (workstream C).** `1:30:...` ints whose value leaves i128 are exact only when every
  part is non-negative (YAML does not produce other cases from plain scalars).
- **D-19 Digits (workstream B).** Only ASCII digits are accepted where Python's `int()` / `str.isdigit()` / `strptime` also take
  other Unicode decimal digits: the DCGM GPU index, the version tuples of the CLI probes, `strptime` fields, label
  `isdigit` checks. (YAML `!!int` / `!!float` DO take them since A-7, as PyYAML does.)
- **D-20 JSON from tools and upstreams (workstream B, workstream C).** Where an adapter parses JSON with serde (Prometheus answers,
  workstream B's run files), NaN / Infinity literals are refused (Python's `json` accepts them); integers beyond u64 become
  floats; UTF-16 / UTF-32 JSON bytes are not auto-detected (Python's `json.loads(bytes)` detects them). Tool-written
  JSON nested deeper than 512 levels (pyjson) or 900 (`support::json_depth_ok`) is treated as unreadable.
- **D-21 Guardrails validate copy (workstream B).** The temp copy is made with `std::fs::copy`, which keeps the permission bits;
  `shutil.copyfile` does not (the copy lives in a 0700 directory either way).
- **D-22 Lax parsing of `checks.jsonl` (workstream B).** pydantic's lax coercions when reading Guardrails' `checks.jsonl`
  (e.g. a `ts` given as a string) are not reproduced: such a value is not coerced.
- **D-23 `_default_config` (workstream B, Guardrails).** A config file that is not UTF-8 is read lossily (D-8) instead of
  raising UnicodeDecodeError.
- **D-24 Python exceptions from the YAML loader (workstream B).** A Python exception PyYAML lets escape (ValueError for an
  impossible date, KeyError for `!!bool maybe`, ...) becomes `AdapterError::Failed` with the same class name and
  text, answered as an adapter failure where Python's unhandled exception is a bare 500.
- **D-25 YAML values (workstream C).** `!!set` keeps document order (Python: hash order); `repr()` / `str()` of values nested
  deeper than 490 through alias chains raise RecursionError (Python manages a few thousand frames); non-UTF-8 file
  names in the Data Designer run-file listing are shown lossily.
- **D-26 Transport limits (A-9).** uvicorn has no header-read timeout, no body-read timeout and no connection cap
  (`limit_concurrency` is unset); it closes an idle keep-alive connection after 5 s. Rust: a connection that has not
  sent a complete request head within 5 s is closed (this also closes idle keep-alive connections after 5 s, as
  uvicorn does); a request body that has not fully arrived within 30 s is answered **408**
  `{"error":{"code":"request_timeout",...}}`; at most 256 connections are open at once (the 257th is closed on
  accept; an open SSE stream counts). A body cut short by the client is answered 400 "There was an error parsing the
  body", as FastAPI answers a `ClientDisconnect` (parity, not a deviation; nobody reads that answer).
- **D-27 File reads in run directories (A-9).** Data Designer reads `records.jsonl` and the batch files with
  O_NOFOLLOW (a symlink planted in a run directory is an error; Python's `open` follows it), stops a file at a line
  over 16 MiB or after 1 GiB in all, and its `copy2` copies a symlink as a symlink (Python's `shutil.copy2` copies
  what it points to). AIPerf reads at most 16 MiB of `profile_export.jsonl` (it uses the first 500 lines only).
  `/`, `/index.html` and `/favicon.*` are served only when they resolve inside the frontend directory (Python serves
  those two through `FileResponse`, which follows a symlink anywhere); the static directories were already contained.
  The private files the core writes (`plumbing::write_private`, the support-run metadata) are written to a random
  O_EXCL temp file, mode 0600, then renamed (Python writes them in place).
- **D-28 Upstream URLs are also parsed as the HTTP client parses them (A-9).** A URL Python's checks accept is
  additionally parsed with the `url` crate (the parser reqwest uses); it must be `http`, without user info, query or
  fragment, and its host and port must be the ones the Python checks allowed. A URL the two parsers read differently
  (for example `http://127.1:4010` with `127.1` allow-listed: the client would connect to 127.0.0.1) is refused with
  the same message as a host outside the allow-list.
- **D-29 Background-task panics (A-9).** A panic in one metrics-poller tick, one engines refresh, a support-run start
  or an AIPerf profile run is caught, logged once (redacted, `<task> failed: ...`) and the task carries on with its
  next tick; a lock left poisoned by a panic is recovered rather than failing every later request. Python's asyncio
  tasks log an escaped exception and stop; the poller and the refresher catch `Exception` per tick, so the visible
  behaviour is the same except that Rust survives a bug that would have killed a Python task.
- **D-30 Run ids from one clock reading (A-9).** Python stamps a run id as `strftime(gmtime())` + the milliseconds of a
  SECOND `time.time()`; when the second ends between the two readings the id falls almost a second behind and sorts
  before an older run (the clean-clone parity run caught it: a Data Designer run `…34-000` created after `…34-941`).
  Every Rust run id (`plumbing::run_stamp`) now takes both parts from one reading. The parity harness sends actions
  only between 50 and 700 ms into a second, so both stamps land in the request's second and the order compares.
- **Test-only (workstream B):** in the DCGM `post_apply` restart test the fake upstream answers 503 instead of refusing the
  connection.
- **Moot since A-7 (the private copies are deleted):** workstream B's "alias expansion over 1,000,000 nodes raises
  ConstructorError" (now: the loader shares objects; tree conversion refuses past 1,000,000 values with MemoryError,
  D-12), its tomllib limits (ints beyond i128, nesting past 400: now exact BigInt text and Python's frame budget,
  D-12), "recursive alias as null", the private `safe_dump` folding, and `!!omap` / `!!pairs` (now tuples).
- **Python bugs kept for parity (behaviour reproduced on purpose; fix both sides together after cutover):**
  - Data Designer does not catch KeyError / TypeError raised by YAML constructors (see the minor 500s below).
  - The evaluator does not catch UnicodeDecodeError when reading `suites.yaml`.
  - Data Designer `_export_batches` DATA LOSS: once `records.jsonl` holds `MAX_PREVIEW_RECORDS` (2,000) lines, the
    loop `continue`s before the user-copy step, so every later batch is silently missing from the user's csv /
    jsonl / json copy (parquet batches are still copied). Reproduced in `DataDesignerAdapter::export_batches`.
  - YAML dates: an impossible date (`2026-02-30`) makes PyYAML's constructor raise ValueError, which escapes
    `except yaml.YAMLError`: Guardrails, AIPerf and OpenShell validate answer 500 (workstream B parity cases, equal).
  - Minor uncaught 500s (workstream C goldens, reproduced with Python's class and text): SkillEvaluator reports of stored
    runs with odd run.json values (ValueError "Unknown format code 'f' for object of type 'str'", TypeError
    "sequence item 0: expected str instance, int found" / "can only join an iterable" / "unsupported format string
    passed to dict.__format__" / "can only concatenate str (not "int") to str", AttributeError "'str' object has no
    attribute 'get'" in tier-1 summaries); Evaluator validate / suites and Data Designer providers / aliases /
    show_config on YAML that PyYAML's constructor rejects with a Python exception (`!!int x` ValueError, `!!bool
    maybe` KeyError, `!!timestamp foo` AttributeError "'NoneType' object has no attribute 'groupdict'", month 13
    ValueError, a huge int as float OverflowError, NaN to int ValueError).
- YAML error wording (workstream A's yaml-rust2 copy) is no longer a deviation: the Dynamo adapter uses `services::pyyaml`, the
  exact engine (A-6).

## 14. Risks and open points (owners)

- R-1 Python adapters' exact strings embed `repr()`, `str(float)`, `json.dumps`, `%` formats: use `pyfmt` (all WS).
- R-2 PyYAML 1.1 semantics (bools, timestamps, error texts with marks) have no Rust equivalent; write the resolver and
  pin it with goldens generated from Python (workstream A/B/C, merged later at integration). **Closed** by
  `services::pyyaml` (A-7).
- R-3 `tomllib` is TOML 1.0, the toml crate parses TOML 1.1 (accepts multi-line inline tables, `\e`, optional
  seconds); error messages differ. Validators must reject what tomllib rejects (workstream A switchyard/relay, workstream B openshell).
  **Closed** by `services::pytoml` (A-6).
- R-4 `difflib.get_close_matches` / SequenceMatcher (workstream B dcgm, aiperf) and `shlex.split` (workstream A dynamo) must be
  ported exactly; pin with goldens.
- R-5 `datetime.fromisoformat` / `strptime` / `time.strftime` used for timestamps (workstream A relay/dynamo/switchyard, workstream B
  openshell/dcgm): Python 3.13 accepts more ISO forms than RFC 3339 parsers.
- R-6 Range requests, HEAD on static files and percent-decoding edge cases are covered by the harness corpus, not by
  Python tests.

## 15. Addenda (numbered, additive only)

### A-1 (2026-10-27) Shared YAML and TOML engines: the merge plan (R-2, R-3)

During phase 2 each workstream keeps a PRIVATE YAML/TOML module inside its own files (section 9). After the merges
the integrator folds them into two core modules with a fixed API:

- `services::pyyaml` = PyYAML 6.0.3 `yaml.safe_load` / `yaml.safe_dump` (the backend venv pins 6.0.3, pure-Python
  SafeLoader). API: `PyValue` (None, Bool, Int(i64), BigInt(String), Float, Str, Bytes, Date, DateTime, List,
  Dict(Vec<(PyValue, PyValue)>), Set; Python equality), `safe_load(&str) -> Result<PyValue, YamlError>`,
  `YamlError { problem /* e.problem */, full /* str(e) */, problem_mark: Option<Mark{line, column}> /* 0-based */ }`,
  `DumpOpts { sort_keys, allow_unicode, default_flow_style, width }` with `DumpOpts::SAFE_DUMP` = PyYAML's defaults
  (sort_keys true, allow_unicode false, block style, width 80) and `safe_dump(&PyValue, &DumpOpts) -> String`.
  Users: dynamo (workstream A), aiperf / guardrails / openshell (`safe_dump(sort_keys=False)`) (workstream B), datadesigner /
  evaluator (workstream C). aiperf, guardrails, dynamo and evaluator read `e.problem_mark.line`; others `str(e)` or
  `e.problem`.
- `services::pytoml` = an exact port of CPython 3.13 `Lib/tomllib/_parser.py` (TOML 1.0, not 1.1): `loads(&str) ->
  Result<TomlValue, TomlDecodeError>`, `TomlDecodeError { msg, line, column }` whose `Display` is tomllib's `str(e)`
  (`"<msg> (at line L, column C)"` / `"... (at end of document)"`), `TomlValue` keeping insertion order and the
  date/time/datetime kinds, plus `py_str()` for Python's `str()` of a value. Users: switchyard and relay (workstream A,
  `str(e)` and the line number parsed from it), openshell (workstream B), and core's `capable_models`. When it lands, the
  `toml` crate leaves Cargo.toml (Nemo never writes TOML).

Source of the YAML engine: the exact PyYAML 6.0.3 port of a sibling project (Agent Hub's Rust port:
`hub-core/src/yaml/` + `value.rs`). Its public API already matches (`yaml::load(text, Loader::Safe)`,
`yaml::dump(&PyValue, &DumpOpts)`, `YamlError { problem, full }`); what Nemo adds in its copy is `problem_mark`
(requested from the hub as an addendum so the two copies stay identical). The copy is taken (separate repos, no
shared crate) at a pinned hub commit once its goldens pass, with that commit in the module header. Until then the
engine is the best of the three workstream copies (the one passing the most goldens, fixed until all pass). No
equivalent exists for TOML: the integrator ports tomllib (about 700 lines of Python).

Procedure (at each workstream merge, then once at the end):
1. Collect every workstream's YAML/TOML goldens into `tests/fixtures/pyyaml/` and `tests/fixtures/pytoml/` with their
   generators in `parity/gen/` (Python 3.13 + PyYAML 6.0.3 from `backend/.venv`); add a corpus of the edge cases
   no adapter test pins: YAML 1.1 bools/null/octal/sexagesimal/timestamps, merge keys, anchors, duplicate keys,
   error documents (problem, context, marks); TOML 1.1-only constructs that tomllib REJECTS (multi-line inline
   tables, `\e`, `\xHH`, times without seconds) and one document per tomllib error message.
   `tests/core_pyyaml.rs` and `tests/core_pytoml.rs` replay them all against the shared modules.
2. Point each adapter's private calls at `services::pyyaml` / `services::pytoml` (thin wrappers at first), delete the
   private copies, and keep the adapters' own tests green: they are the behavioural check of the swap.
3. Remove `yaml-rust2` / `toml` from Cargo.toml when nothing references them; `cargo deny check`.
4. The parity harness's config validate/apply cases (every YAML/TOML file of the seeded repo, plus refusals) must stay
   at 0 diffs.

### A-2 (2026-10-27) Phase-2 core changes (integration)

- Startup follows uvicorn's order: the lifespan (user dirs, bootstrap key, secret registration, poller) runs BEFORE
  the bind, so a failed bind (exit 3) leaves the same state. The `uvicorn.error` log lines are reproduced ("Started
  server process [pid]", "Waiting for application startup.", "Application startup complete.", "Uvicorn running on
  http://host:port (Press CTRL+C to quit)", "Shutting down", "Waiting for application shutdown.", "Application
  shutdown complete.", "Finished server process [pid]") and the bind error is uvicorn's exact text.
- After a graceful SIGTERM shutdown the process re-raises SIGTERM (it ends KILLED BY SIGTERM, like uvicorn); SIGINT
  ends with exit 0 (Python suppresses the re-raised KeyboardInterrupt). New dependency: `signal-hook 0.4.5`
  (`emulate_default_handler`: our crate forbids `unsafe`).
- Log timestamps are local time with the numeric offset (`logging.Formatter.formatTime` with `%z`). New dependency:
  `chrono 0.4.45` (clock).
- pydantic-core prints a FLOAT field bound with Rust's `Display` (`ge=300.0` reads "300"); config.rs does the same.
- Pinned by new checks: `node parity/xstate.mjs` (Rust reads a Python-written `console.db` and vice versa, at the
  services level with every table and at the HTTP level with keys/audit/settings, both orders; fixture
  `tests/fixtures/xstate/` from `parity/gen/gen_xstate_fixture.py`), `node parity/launcher.mjs` (open/stop, incl.
  each implementation stopping a console the other started), `node parity/startup.mjs` (26 bad settings + a
  taken port), harness scenarios `lifecycle` and `probe-broken` (background poller and rollups, the capable probe,
  SSE heartbeat / idle timeout / SIGTERM shutdown and the exit status), `tests/core_lifecycle.rs`.
- Harness: `--only <regex>`; whole-run argv / upstream sets are built from the complete logs (calls between cases are
  counted); `check-test-map.mjs` reports `pending WS-` ignores (`--no-pending` fails on any).

### A-3 (2026-10-27) Integration gate (integration, after each workstream merge)

`cargo fmt --check`, clippy `-D warnings`, `cargo test`, `cargo deny check`, `cargo audit`; `node
parity/check-test-map.mjs` (`--no-pending` once every dependency of the remaining ignores has landed);
`node parity/run.mjs --a python --b rust --allow-diff-routes parity/allow-diffs.txt` (the merged
workstream has removed its lines; the aggregate lines go when all ten adapters are ported); `xstate.mjs`,
`launcher.mjs`, `startup.mjs`; the frontend unit tests; `E2E_BACKEND=rust node frontend/e2e/run.mjs` (the merged
systems' pages must render).

### A-4 (2026-10-27) `services::pyyaml` landed; Python float repr and str repr made exact (integration)

- `services::pyyaml` is the Agent Hub's exact PyYAML 6.0.3 port, copied read-only from Agent Hub's Rust port at commit
  a214dd6 (`hub-core/src/yaml/` + `value.rs` + `py/{text,float}.rs`; the commit is in the module
  header). Nemo's changes, all marked "Nemo": module paths; `py::float::repr` delegates to `services::pyfmt::float_repr`;
  entry points `safe_load(&str) -> Result<PyValue, YamlError>` and `safe_dump(&PyValue, &DumpOpts)` with
  `DumpOpts::SAFE_DUMP` (PyYAML defaults) and `DumpOpts::SAFE_DUMP_UNSORTED` (`sort_keys=False`); `pyunicode` is
  crate-visible. `YamlError.problem_mark: Option<Mark>` (0-based `index`/`line`/`column`) is what aiperf, guardrails,
  dynamo and evaluator read as `e.problem_mark.line`. To refresh: copy a newer hub commit, re-apply the "Nemo" edits,
  regenerate the goldens.
- Goldens: `tests/core_pyyaml.rs` replays `tests/fixtures/pyyaml/yaml_{load,dump,random}.json` (2,089 loads, 13,238
  dump checks over 4 option sets, 4,600 random checks), generated by `parity/gen/gen_yaml_golden.py` with the
  backend venv's PyYAML from: every text `yaml.load` and value `yaml.safe_dump` saw in Nemo's pytest suite
  (`yaml_capture.py` -> `yaml_captured.json`), the YAML files under `backend/tests` (the parity harness's fixture copies were the same files; A-9), the
  hub's scalar/string matrix, malformed corpus and nesting cases, and a seeded random batch. `yaml_codec.py` is the
  typed JSON encoding; `gen_unicode_tables.py` regenerates `pyunicode.rs`.
- `pyfmt::float_repr` (CORE fix; workstreams get it on merge): CPython's repr picks the nearest shortest candidate
  with ties to even; Rust's `{:e}` could pick the other on a decimal tie (`-1133092563735992.25` printed `...2.3`, Python
  `...2.2`). Now: shortest digit count from `{:e}`, then the correctly rounded `{:.(n-1)e}` when it round-trips. Pinned
  by `tests/fixtures/pyfmt/float_repr.txt` (9,018 values from `parity/gen/gen_float_repr_golden.py`).
- `pyfmt::str_repr` uses the exact CPython 3.13 `str.isprintable()` table (Unicode 15.1.0) instead of an
  approximation, so `repr()` in messages escapes exactly what Python escapes.
- TOML: no core copy yet. workstream A's `adapters/switchyard/tomllib.rs` (a CPython 3.13 tomllib port) is the candidate for
  `services::pytoml`; it is folded into core at the workstream A merge, with every workstream's TOML goldens replayed.

### A-5 (2026-10-27) The e2e runner is hermetic by default (workstream A addendum 2)

`frontend/e2e/run.mjs` used the caller's PATH and the backend's default URLs, so every run, Python's `make e2e`
included, sent unauthenticated GETs from the status/metrics pollers to the live router :4000, Switchyard :4010, Relay
:4040, Dynamo :8020/:8081, DCGM :9400, Prometheus :9090 and Guardrails :8806, and ran the host's `docker`,
`systemctl` and `nemo-relay` (allowlisted CLIs found on PATH; no keys were passed: the child env is built from
scratch and `NEMO_CONSOLE_NO_USER_ENV=1`). Now, for both backends alike: `NEMO_CONSOLE_URLS` maps every
`DEFAULT_URLS` key to the closed `http://127.0.0.1:9`, PATH is an empty directory (every CLI is "not found"), and the
runner fails before opening a page unless `GET /api/v1/settings` shows every effective URL closed (a new
`DEFAULT_URLS` key cannot slip through). `E2E_LIVE=1` restores the old behaviour for a deliberate live run.

### A-6 (2026-10-27) workstream A merged; its helpers folded into core; parser limits (integration)

- Merged `rust-backend-a` 58fa91e (Switchyard, Relay, Dynamo) with `git merge`, no conflicts.
- Core modules now (fixed; every workstream uses these, not private copies):
  - `services::pytoml` = workstream A's CPython 3.13 tomllib port (was `adapters/switchyard/tomllib.rs`; 162 goldens in
    tests/switchyard_tomllib.rs). API: `loads(&str) -> Result<Table, TomlDecodeError>`, `Table` = `IndexMap<String,
    TomlVal>`, `TomlDecodeError(pub String)` = `str(e)`, `TomlVal::{repr, py_str, as_str, as_table, as_array}`. It
    supersedes A-1's sketch. The core's `capable_models` / `capable_models_text` read it (the `toml` crate is no
    longer used by Nemo's code; it and `yaml-rust2` leave Cargo.toml once workstream B/workstream C have dropped their private
    copies).
  - `services::pyutil` (was `adapters/switchyard/pyutil.rs`): Python `str` helpers, `float(str)`,
    `datetime.fromisoformat`, `PyJson` repr/str/truthiness.
  - `services::shlex` (was `adapters/dynamo/shlex.rs`), `services::gzip` (was `adapters/relay/gzip.rs`).
  - Dynamo's yaml-rust2 copy (`adapters/dynamo/pyyaml.rs`) is deleted: Dynamo reads YAML through `services::pyyaml`
    (`PyValue`). Nemo additions to the engine: `YamlError.exc_class` (`type(exc).__name__`: ScannerError /
    ParserError / ComposerError / ConstructorError / ReaderError, or the escaping Python exception; Dynamo puts it in
    "cannot read the repo compose: <class>"), pinned by a new `class` field in the goldens; `PyValue::truthy`,
    `PyValue::is_map`.
- workstream B / workstream C: replace your private PyYAML and tomllib copies with `services::pyyaml` / `services::pytoml` when you
  next merge `rust-backend` (or the integrator does it at your merge). Keep your goldens: they move to
  tests/fixtures/{pyyaml,pytoml}/ or stay with your adapter tests, replayed against the core modules.
- Parser limits (D-12): `services::pytoml` bounds nesting by Python's frame budget (2 frames per array level, 3 per
  inline table, 992 in all) and parses a document with more than 32 `[`/`{` on a scoped thread with a 16 MiB stack;
  `api::pyjson` bounds nesting at 512 and applies Python's 4,300-digit int limit. Both used to recurse without bound:
  a deeply nested TOML file or JSON body aborted the whole process with a stack overflow. Pinned by
  tests/core_pytoml.rs, the pyjson unit tests and parity cases `sec.body.json-*`.
- Integration tests reach adapter internals through `from_settings` constructors or behaviour through the
  `SystemAdapter` trait (no downcasting; the trait is unchanged).
- Parity allow-list (parity/allow-diffs.txt): per-system routes are no longer allowed wholesale; a
  `stub:<system_id>` line allows only cases whose request path names that system. workstream A's systems have no line, so
  every Switchyard / Relay / Dynamo case must be equal. Each workstream deletes its own `stub:` lines at its merge.

### A-7 (2026-10-28) workstream B and workstream C merged; one YAML engine, one tomllib; cooldown gate (integration)

- Merged `rust-backend-b` 582cfd2 and `rust-backend-c` 16ae36d with `git merge` (only TEST-MAP.md conflicted; it
  is generated: `node backend/tools/gen-test-map.mjs`).
- `services::pyyaml` is ONE engine (fixed API, supersedes A-1/A-4's): loading = workstream C's port
  (`services/pyyaml/load.rs` + the arena object model `services::pyobj`): `safe_load(&str) -> Result<Doc,
  YamlError>`, `compose(&str) -> Result<Composed, YamlError>` (nodes with tags and start marks, for AIPerf's
  `_line_map`), `YamlError::{class, text, problem, context, problem_or_str, problem_mark, is_yaml, full}` (the REAL
  `problem` attribute: `getattr(e, 'problem', e)` is `problem_or_str`). Tree consumers use `safe_load_value` /
  `to_value` (`PyValue`, now with `Tuple`). Dumping = the Agent Hub's exact SafeRepresenter/emitter (`safe_dump`,
  `DumpOpts::SAFE_DUMP*`). The hub's loader half, workstream B's `adapters/aiperf/pyyaml/` and workstream C's private copy are
  deleted. Every golden set passes: the hub corpus (core_pyyaml.rs), workstream B's 76 + 1,000 (aiperf_pyyaml_golden.rs,
  incl. compose key marks, `problem` and dump bytes), workstream C's 2,695 (evaluator_yaml_golden.rs), Dynamo's. Found on the
  way: Unicode decimal digits in `!!int`/`!!float` (`int('١٢٣')`), Python's 4,300-digit int limit, Dynamo's override
  diagnostic now shows the real `problem`. Reported to the Agent Hub: its engine's `YamlError::problem` is the first
  line of `str(e)` (not PyYAML's attribute), it has no tuples for `!!omap`/`!!pairs` and no cycles.
- `services::pytoml` is the one tomllib (workstream B's `adapters/openshell/tomllib.rs` deleted): passes workstream A's 162 and
  workstream B's 67 + 1,500 fuzz goldens (fixed: year 0 is an invalid date). `TomlDecodeError::{class, is_decode}`;
  `TomlVal::get`.
- Shared services folded from the workstreams: `pystr` (workstream B; `pyutil`'s and `pyobj`'s str helpers now delegate to
  it), `plumbing` (workstream B's `aiperf/wsb.rs`), `difflib` (workstream B), `support` and `pyobj` (workstream C), with `pyutil`, `shlex`,
  `gzip` (workstream A, A-6). Adapter-specific helpers stay with their adapter (`datadesigner/files.rs`).
- Security (workstream C's report): the request-body JSON parser was already bounded at A-6; Python answers the 400 KB
  attack body (200,000 nested arrays) with 400 on every route, not 500 (checked against a hermetic Python backend),
  and so does Rust; parity cases `sec.body.json-attack-400k*`. Too-deep bodies write no audit row (as Python).
  Recursive parsers and serializers audited: JSON (bounded 512; `support::json_depth_ok` pre-scan), TOML (frame
  budget + big-stack thread), YAML (iterative composer, depth-guarded construction and traversals, iterative dumper),
  `to_value` (bounded by depth and value count).
- `tests/fixtures/xstate/console.db` is committed (the root `*.db` ignore hid it; `.gitignore` now excepts
  `backend/crates/nemo-console/tests/fixtures/**/*.db`).
- Lead integration tests: `tests/integration_core_deployment_settings.rs`, `tests/integration_core_observe.rs`;
  `DcgmAdapter`, `GuardrailsAdapter`, `OpenShellAdapter` gained `from_settings` like the workstream A adapters (every
  registry `build` is one constructor). TEST-MAP: 936 of 936 mapped, `check-test-map --no-pending` PASS.
- Poller gap (workstream C): in the lifecycle and probe-broken scenarios the whole-run upstream sets are now equal (Relay
  /health and Dynamo /metrics included), with all ten adapters ported.
- Parity: the allow-list holds only `params.dynamo.preview.nan` (D-6). Caveat: `--filter` / `--only` on the
  lifecycle and probe-broken scenarios still compares the WHOLE-RUN upstream set (background tasks call between and
  after cases), so a filtered run can show a `global:<scenario>` diff a full run does not; judge those scenarios on
  full runs.
- Crate cooldown (owner's policy: every crates.io package at least 14 days old; security fixes bypass it, nothing
  else does): `python3 -I backend/tools/crate-age.py` (copied from Engine Console; `--suggest` prints the downgrade),
  exceptions in `backend/tools/crate-age-exceptions.toml` (rustls 0.23.46, rustls-webpki 0.103.16, webpki-root-certs
  1.0.10: RustSec fixes). 21 packages were downgraded. Part of the A-3 gate from now on. `toml` and `yaml-rust2` left
  Cargo.toml (A-1 step 3).


### A-8 (2026-10-28) Cutover: the Python backend leaves the checkout; version 0.4.0 (integration, branch `rust-cutover`)

The plan, and what this branch does (each step is done unless it says otherwise):

1. **Layout.** The Python backend (`backend/src`, `backend/tests`, `pyproject.toml`, `uv.lock`) is removed. The Cargo
   workspace moves from `rust/` to `backend/`, so the e2e gate's `backend/` + `frontend/` tree pair (scripts/
   e2e-gate.sh, .githooks/pre-push) still covers all server code. The parity harness is a development tool, not
   server code: it moves to the top-level `parity/` (`skeleton-allow.txt` becomes `allow-diffs.txt`), with the
   generators that need the Python implementation (`gen_api_golden.py`, `gen_openapi.py`, `gen_xstate_fixture.py`)
   in `parity/gen/`. `crate-age.py`, its exceptions and `gen-test-map.mjs` stay with the workspace in `backend/tools/`.
   PORTING.md and TEST-MAP.md move with it (`backend/`).
2. **The Python reference.** `parity/ref.mjs`: `python@<tag or commit>` extracts that revision's `backend/` with
   `git archive` into `parity/.ref/<rev>/` (gitignored; never a git worktree) and builds its venv once with
   `uv sync --frozen`, offline from the uv cache (`PARITY_UV_ONLINE=1` allows PyPI on a new machine). Plain `python` is
   `DEFAULT_REF` = `aa2f810` (main's last Python commit: 0.3.1 plus two unreleased commits, the code the port was
   checked against). Tag `v0.3.1` is two commits older: parity against it differs on the deployment-name settings and
   the capable-tier probe. **Owner:** tag `aa2f810` (for example `v0.3.2` or `python-final`) and set `DEFAULT_REF` to
   the tag. Every check takes the reference: `run.mjs --a python@<rev>`, `xstate.mjs` / `launcher.mjs` /
   `startup.mjs --python python@<rev>`; both backends run from the same cwd (the fake CLIs record it), and each side's
   own release in `version` fields is normalised to `<version>`. The generators' docstrings run them with the
   reference's venv (`REF=$(node parity/ref.mjs)`); `gen_yaml_golden.py` reproduces the committed goldens from it.
3. **Everything that named the Python backend.** Makefile (`setup` = `cargo build --locked` + hooks; `build`,
   `release`, `run`, `test`, `lint` = rustfmt, `typecheck` = clippy, `deny`, `audit`, `crate-age`, `frontend-test`,
   `e2e` (builds first), `parity`, `all`, `install-desktop` = the release binary); `scripts/e2e-gate.sh` (builds the
   backend before the suite); `.githooks/pre-push` (message); CI `ci.yml` (backend job: the pinned toolchain through the
   runner's rustup, fmt, clippy, test, cargo-deny and cargo-audit installed with `cargo install --locked` at exact
   versions, crate-age; frontend job unchanged; e2e job builds and runs the suite; actions still pinned by commit, no
   new third-party action); `deploy/nemo-console.service` (ExecStart = the release binary); the desktop entry (through
   `make install-desktop`); `frontend/e2e/run.mjs` (Rust only, `E2E_BIN`). Docs: README, OPERATIONS, ARCHITECTURE (a
   0.4.0 mapping note; historical sections keep their Python names), FEATURE-MATRIX (every "verified live" is marked as
   observed with the Python backend; the Rust backend is implemented and parity-checked, not yet run live), THREAT-MODEL
   (file references, supply-chain note), PUBLISHING-CHECKLIST, SECURITY (supported versions), RELEASE-NOTES, CHANGELOG,
   ADR 0016 (new) and ADR 0002 (amended). ADRs written for the Python backend keep their text (records of decisions).
4. **Version 0.4.0** (`backend/Cargo.toml`; the served OpenAPI document; the API golden replays the reference's 0.3.1 as
   the backend's own version). CHANGELOG: "backend rewritten in Rust", the deviations summary, Python failures not
   reproduced, Python bugs kept.
5. **Clean-clone gate** (results below): a literal `git clone` of `rust-cutover` into a temp directory outside this
   repository, `make setup`, every gate of `make all`, parity against the reference, the cross checks.
6. **Not done here (owner / release-verifier):** push, tags (`v0.4.0`; the Python reference tag of step 2), the GitHub
   release, CI on GitHub, and the first run of the Rust binary against the real systems (to turn "parity-checked" into
   "verified live" in FEATURE-MATRIX).

**Clean-clone gate, 2026-10-28 (step 5): PASS.** I cloned `rust-cutover` at d51f7df with a literal `git clone` into a
temp directory outside this repository and ran, with no keys in the environment: `make setup`, then `make all`. That
covers:
- rustfmt and clippy `-D warnings`: clean;
- `cargo test --locked`: 822 passed, 0 failed;
- cargo-deny (advisories, bans, licenses, sources) and cargo-audit (266 crates): ok;
- crate-age: 265 packages, 0 too new, the 3 security exceptions;
- frontend `node --test`: 122 passed;
- browser e2e, hermetic: 13 routes, 46 pages and tabs, no error state; the pass was recorded.

After `make all`, from the same clone:
- `make parity`: the reference was extracted and its venv built offline in the clone; 1,357 cases, 1,356 equal, only
  D-6.
- `xstate.mjs`, `launcher.mjs`, `startup.mjs`: PASS.
- `check-test-map --no-pending`: PASS (936 of 936).
- `check-routes`: PASS (32 routes).
- The harness self-tests: 6 passed.

### A-9 (2026-10-28) Audit fixes before release (integration, branch `rust-cutover`)

An independent pre-release audit returned SHIP-WITH-FIXES; each fix has a test that fails without it.

1. **Roles enforced structurally.** `api::route_guard` checks every route's table role before its handler runs, after
   the JSON body is parsed and in FastAPI's dependency order (`{system_id}` 404, `{file_id}` / `{action}` 404, then
   401 / 403); the handlers keep their own checks. `tests/core_route_guard.rs` sends every route of the table as an
   unauthenticated non-loopback client and as a viewer with a valid `{}` body, and proves the guard on a table whose
   handler has no role check.
2. **CI runs the parity gates** (`parity` job: harness self-tests, test map, route coverage, the differential run
   against `python-final`, xstate, startup, launcher). `docs/RELEASING.md` (named by `rust-toolchain.toml`) written.
3. **Background tasks are panic-contained** (`services::common::contained`, D-29); a panic hook logs panics through
   `redact`; `RecoverLock::lock_recovering` replaces every `Mutex::lock` in the services. `tests/core_panics.rs`.
4. **Transport limits** (`server::serve_connections`, `ServeLimits`, `api::BODY_READ_TIMEOUT_MS`; D-26).
   `tests/core_transport.rs` boots the real listener on an ephemeral port: dispatch, the h11-style rejections, the
   real peer address for loopback trust, idle and slow-header close, the connection cap, a truncated body (400) and a
   slow body (408).
5. **Redaction and the secret scan fail closed** on a regex engine error (backtrack limit): `sub_with` redacts the
   whole chunk, `find_secret` / `key_is_sensitive` count the error as a match.
6. **URL cross-check** with the `url` crate (D-28).
7. **Publication hygiene.** Personal paths, private repository and host names, process wording and lock markers
   removed from docs, comments and generators; DCGM fixtures use `node1..5` and 192.0.2.0/24; the xstate fixture was
   regenerated with the Python reference using fixed, obviously fake keys (`nck_TEST-ONLY-not-a-secret-*`). Product
   defaults that name the original deployment (the Prometheus job names `dcgm-sparks` / `dcgm-aitop100`, the model
   alias `aitop/nemotron-lightning`) are unchanged: they are settings defaults shared with the Python release, and
   changing them is a behaviour change for existing users. Python test ids in `TEST-MAP.md` / `parity/pytest-ids.txt`
   keep their names (they are the frozen reference's ids).
8. **Release plumbing.** The stub adapter, `AdapterError::NotPorted` and the 501 `not_ported` envelope are removed;
   `clap` (declared, never used) left the workspace; sections 1, 2, 12 and 14 corrected; `deploy/nemo-console.service`
   has `MemoryMax` / `TasksMax`; THREAT-MODEL and SECURITY name the Rust files and 0.4.x.
9. **File hardening** (D-27): O_NOFOLLOW and caps in Data Designer and AIPerf reads, containment for the listed static
   files, atomic 0600 writes, `copy2` without following a symlink source; static file bodies are read with `tokio::fs`.
   `tests/core_file_hardening.rs`.
10. **Smaller items.** Header values that are not ASCII are decoded as latin-1, as Starlette does (a non-ASCII `Origin`
    is no longer read as absent; `tests/core_header_latin1.rs`); `check-test-map` accepts a mapped Rust fn only when it
    carries `#[test]` / `#[tokio::test]` (a helper of the same name no longer counts).
11. **Reference tag.** `parity/ref.mjs` defaults to tag `python-final`, which must name aa2f810 (a moved tag is
    refused; without the tag the pinned commit is used).
12. **Found while gating:** the run-id race of D-30 (an intermittent order difference in the full scenario); the
    parity role sweep (36 cases: every guarded route without a key, every admin route as a viewer) runs in the full
    scenario; the harness reads the crate's fixtures, so its duplicate `parity/fixtures/` copy is gone.


**Clean-clone gate for A-9, 2026-10-28: PASS.** A literal `git clone` of `rust-cutover` at 1616fda into a temp
directory outside this repository, no keys in the environment: `make setup`, then `make all`: rustfmt and clippy
`-D warnings` clean; `cargo test --locked` 842 passed, 0 failed; cargo-deny (advisories, bans, licenses, sources) and
cargo-audit (266 crates) ok; crate-age 265 packages, 0 too new, 2 security exceptions; frontend `node --test` 122
passed; browser e2e, hermetic: 13 routes, 46 pages and tabs, no error state (pass recorded). Then, from the same
clone: `make parity` 1,393 cases, 1,392 equal, only D-6 (the reference built offline in the clone, from commit
aa2f810 since the `python-final` tag is not in the clone yet), and the full scenario five more times with no other
difference; `xstate.mjs`, `startup.mjs`, `launcher.mjs` PASS; `check-test-map --no-pending` PASS (936 of 936);
`check-routes` PASS (32 routes); `gen-test-map --check` up to date; harness self-tests 8 passed. A first clean-clone
run (at 3a47eaf) found the D-30 race and a clippy lint in a new test; both are fixed in 0101aaa and 1616fda. Each
A-9 fix was also checked the other way: with the fix reverted, its test fails (route guard, header timeout,
connection cap, truncated body, body timeout, poller containment, poison recovery, latin-1 header, regex fail-closed,
URL cross-check, O_NOFOLLOW, static containment, `copy2`, AIPerf cap, private writes).
