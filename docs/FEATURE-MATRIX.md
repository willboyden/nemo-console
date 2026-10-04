# Feature matrix

Two columns are kept apart on purpose.

**Status:** *Implemented* (code exists and was read), *Partial* (exists with a stated gap), *Not planned*.

**Verification:**

- **verified live**: exercised through the console against a real Switchyard 0.3.0, Relay 0.9.2 and Dynamo 1.5.0
  on one workstation. These checks were performed by the project maintainer on 2026-09-24, after the build;
  the documentation author was told the list and did **not** repeat them. Items the documentation author did run
  are marked **(re-run)** with what was run.
- **fakes only**: covered by offline tests with fake runners/HTTP and captured fixtures; never seen working on the
  real system.
- **unverified**: not exercised by tests or by a live run in a way that proves the claim.

Gates re-run for this revision on 2026-09-28: backend `pytest -q` 384 passed / 3 skipped in the author's deployment
checkout, `ruff check` clean, `mypy --strict` clean (41 files), frontend `node --test` 94 passed. From a clean copy of the
release tree outside that checkout (`make setup && make all`): 382 passed / 5 skipped, frontend 94 passed. Passing tests
prove behaviour against fakes, not against production. Fixtures under `backend/tests/*/fixtures` are captured output
from the real systems, which anchors the parsers but not the write paths.

**Live-run list (maintainer-reported, not repeated by me):** Switchyard config validate, stale-sha 409, and apply with
container recreate and health check; Dynamo preflight, `down` (and the confirm gate: 400 without confirm), `up`, then
serving again; playground chat returning 200 (Switchyard route); Relay `doctor_offline`, `gateway_status`,
`agents_list`, `plugins_validate` actions; auth behaviour (public 401, wrong key 401, bootstrap key -> admin); the
capable-tier-dark problem shown live; unauthenticated `POST /v1/stats/reset` -> 200 on Switchyard; status/metrics/events
read paths for all three; security headers/Host/Origin/CSRF/X-Forwarded-For; frontend rendered against the real
backend. **Also verified live (2026-09-24, later pass):** Relay `gateway_restart` (PID changed, three consecutive healthy checks, 400 without `confirm`),
the viewer role (reads 200; audit, validate, playground and every action 403; nothing executed), and a playground chat routed
through Switchyard `auto-local`. **Still not live:** config auto-rollback, Dynamo parameter apply, `stop_sharing_engine`,
Dynamo `restart`, assistive technology, browsers other than headless Firefox.

## Observe

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Per-system status (health, components, endpoints, problems with a fix) | Implemented | Switchyard, Relay, Dynamo: verified live (read path) | Failure is data: a down system is HTTP 200 with `health=down`. Dynamo "loading" is `degraded` with a summary starting "loading". |
| Metrics snapshot with canonical keys (`sy.*`, `relay.*`, `dyn.*`) | Implemented | verified live (read path, all three) | Keys absent when unreadable; nothing fabricated. `sy.route.calls` counts the newest 2000 routing records (labelled with the window), not a lifetime counter. |
| Metric history 15m / 1h / 24h | Implemented | fakes only for rollup logic; live polling ran with the server | 15 min ring in memory; 1 h and 24 h from 1 minute rollups (SQLite). Same-key series with different labels are **summed**. |
| Events list and SSE stream | Implemented | verified live (read path, all three); SSE caps fakes only | Switchyard/Relay: metadata allowlists. **Dynamo: redacted log lines, not an allowlist (ADR-0010 deviation).** The SSE slot is taken inside the generator. |
| Routing summary (by route, model, tier, tokens, errors) | Implemented | event read path verified live; aggregation fakes only | Derived from the last 5000 routing events; windows 15m/1h/24h/7d. |
| Topology | Partial | rendered against the real backend (verified live); edge semantics unverified | Edges are approximations, not observed traffic. The edge label "agent gateway" is partly clipped behind a node. Two engine container names are hard-coded. |
| Switchyard capable-tier-dark detection | Partial | problem shown live (maintainer); logic also fakes (`test_capable_tier.py`) | `switchyard.tier.capable_dark`: alias from `engine_probes` is the `capable_target` model of a `stage_router` route in the parsed `routes.toml` and its container is not running per a shared, cached `docker inspect` (fix: the container's `engine_start` command, else a compose-dir guess); Switchyard becomes `degraded` with summary prefix "capable tier dark". Also the older `/v1/stats` error-count heuristic (`switchyard.capable_tier.upstream_errors`). No request-level probe; a running-but-broken engine is not detected; the default `engine_probes` and `engine_start` entries are deployment-specific. |
| Relay gateway health | Implemented | verified live (read path) | Any HTTP answer < 500 from the gateway's own `/health` = up (404 expected); upstream from LiteLLM's `/health/liveliness`; unit state from `systemctl --user show`. Never polls `/v1/models` (forwarded to LiteLLM, logged to Langfuse). |
| Relay policy findings (attestation, exporters, PII redaction) | Implemented | part of the Relay status read path (verified live) | Read-only analysis of `plugins.toml`. |
| Console self metrics (`/metrics`) and JSON logs | Implemented | fakes only (`test_console_metrics_endpoint`); JSON log lines seen in a local run | Viewer role. |
| OTLP export of console traces | Partial | unverified | Off by default; needs optional `opentelemetry` packages not in the lock file. |

## Configure

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Switchyard `routes.toml`: validate (TOML + semantic + real `--dry-run` in a throwaway container) | Implemented | verified live | Authoritative. Requires the image locally. |
| Switchyard apply: CAS, backup, atomic write, recreate, health check | Implemented | verified live (apply with container recreate and health check) | `--no-build` recreate. Resets Switchyard's in-memory stats. |
| Stale `base_sha256` -> 409 | Implemented | verified live | |
| Auto-rollback when post-apply check fails | Implemented | **fakes only** (`test_auto_rollback_when_unhealthy`, `..._when_post_apply_raises`) | Not live. |
| Manual rollback to a stored revision, history, pruning | Implemented | fakes only (`test_manual_rollback_to_a_revision`, `test_rollback_validates_and_scans_the_target_revision`) | Rollback re-runs the validator and secret scan on the target revision; 422 before any write if it fails. |
| Relay `routes.toml` edit (validate + apply + gateway restart) | Implemented | fakes only | Validator is non-authoritative (mirror of another schema); the gateway restart is the real check. |
| Relay `plugins.toml` / `config.toml` | Implemented as **read-only by design** | fakes only (`test_post_apply_readonly_file_refused`, `test_read_only_file_is_403`) | Apply refused with 403 `read_only`. Not planned to become writable in v1. |
| Dynamo parameter catalog (14 params), preview of the rendered override | Implemented | preview rendering checked against the real compose (live smoke) | The preview response includes the adapter's `warnings` (the router calls `param_warnings`). |
| Dynamo parameter apply (write override, recreate worker, rollback on unhealthy) | Implemented | **unverified live** (fakes only) | |
| Dynamo override / compose through the config API | Partial | fakes only | Backend exposes `compose` (read-only) and `override` (editable). The hand-edited override is a **closed allowlist**: no single-dash or unknown flags; `$` only in tokens byte-identical to the base compose; `--model`/`--served-model-name` equal the base; env keys must exist in the base and carry no `$` or secret-looking value; fail-closed if the base is unreadable. Also rejected: duplicate flags (normalised across `--x=v`, `_`/`-`, `--no-x`), `--speculative-config` other than exactly `{"method":"mtp","num_speculative_tokens":1..8}`, and path/JSON-looking values that differ from the base. **Gap:** enum-valued flags are not checked against catalog choices and numbers are not type-checked (admin-only, argv list). An absent override reads as empty and the first apply creates it (0600). The UI has **no config tab for Dynamo**. |
| Secret-shape refusal in config content | Implemented | fakes only | Applied on apply and on rollback targets. Pattern-based (R-8). Routes validators also error on non-loopback, non-single-label `base_url` hosts and on userinfo (`test_base_url_rejected`, `test_offbox_base_url_is_an_error_diagnostic`). |
| Settings (URLs, poll interval, credential env-var names) | Implemented | (re-run earlier: GET showed `[set, N chars]`; a non-loopback URL was rejected with 422) | URL changes need a restart. `state_dir` is shown `~`-relative to non-admins. |

## Act

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Danger levels; `confirm:true` for disruptive/destructive; per-system busy lock | Implemented | confirm gate live for Dynamo `down` (400 without confirm); rest fakes only | Default action timeout 900 s, per-action overrides via `action_timeouts` (e.g. `dynamo.up`). Validate calls are capped by a semaphore of 2. |
| Switchyard `restart` (recreate), `dry_run_config` | Implemented | recreate verified live via config apply; `dry_run_config` fakes only | |
| Switchyard `reset_stats` | Implemented | unauthenticated `POST /v1/stats/reset` -> 200 live (maintainer); console action itself fakes only | Needs no credential; the former `switchyard.auth` problem code no longer exists. |
| Relay `gateway_status`, `doctor_offline`, `agents_list`, `plugins_validate` | Implemented | verified live (maintainer) | Output carries caveats: passing `plugins validate`/`doctor` does not prove the gateway starts. |
| Relay `gateway_restart` | Implemented | verified live (maintainer, 2026-09-24: PID 2024310 -> 2358531, 3 consecutive healthy checks, ~3 s) | Waits for several consecutive healthy checks and a stable PID. |
| Dynamo `preflight`, `down`, `up` | Implemented | verified live (through the console, then serving again) | Compose-only. Preflight output includes machine-readable `shares_card:` and `router_aliases_dark:` lines. |
| Dynamo `restart`, `up_mock` | Implemented | fakes only | Not live. |
| Dynamo `up` with `stop_sharing_engine=true` | Implemented | **unverified** (fakes only) | Stops exactly one name- and label-verified container. |

## Interact

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Playground chat (streamed proxy, `max_tokens` clamp, server-side credential, not persisted) | Implemented | verified live: 200 through a Switchyard route (maintainer) | Admin only. Sends `Accept-Encoding: identity`. Other systems' playgrounds not live. |
| "Served by" route/tier badges | Partial | unverified | Header names are guessed; **absent from real Switchyard responses**, so badges are normally empty. |
| Relay playground | Partial | unverified | Needs a credential mapping for `relay`; only `switchyard` has a default. |
| Dynamo playground | Implemented | unverified | Direct to the Dynamo frontend, bypassing the router and any output filtering. |

## Security and access

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Security headers on all responses (CSP, nosniff, referrer, frame, COOP/CORP, no-store on `/api`) | Implemented | verified live; (re-run: headers present on `/api/v1/health`) | |
| `Host` allowlist (421) | Implemented | verified live; (re-run: forged Host -> 421) | |
| Origin check and `X-Console: 1` (403) | Implemented | verified live; (re-run: POST without header -> 403) | |
| `X-Forwarded-For` / `Forwarded` disables loopback trust; loopback trust off by default and forced off for non-loopback host or `allowed_hosts` | Implemented | header behaviour verified live; forced-off rule fakes only | `trust_loopback` defaults to false. |
| Bearer keys (hashed), first-start key file 0600, `mint-key` | Implemented | bootstrap key -> admin, wrong key 401, public 401 verified live (maintainer); (re-run earlier: 0600 files, path-only output) | Default: no key -> role `public`, 401 everywhere except `/health`. |
| Roles: admin | Implemented | verified live via the bootstrap key (maintainer) | |
| Roles: viewer | Implemented | verified live (maintainer, 2026-09-24: viewer key reads 200, audit/validate/playground/actions 403, nothing executed); tests: `test_bearer_keys_roles_and_wrong_key`, `test_viewer_may_only_run_viewer_role_actions` | |
| Every route has exactly one role dependency (enumerated test) | Implemented | fakes-only test of the real app | `test_every_route_carries_exactly_one_role_dependency` |
| Redaction of secrets in all outputs | Implemented | fakes only | |
| Append-only audit | Implemented | fakes only | Records authenticated, authorised mutating calls only. Denials are counted (`nemo_console_denied_requests_total{status}`), kept in a 100-entry ring and logged at most once a minute; never table rows. |
| Audit params blob is valid JSON by construction | Implemented | fakes only (`test_token_key_in_body_does_not_poison_audit`, `test_poisoned_row_does_not_break_list`, `test_audit_write_of_pathological_body_is_fast`) | Oversize becomes `{"_truncated":true,"bytes":n,"keys":[...]}`; unparseable legacy rows are tolerated on read; URL userinfo is redacted (`test_url_userinfo_redacted_in_audit`). |
| Redaction / secret scan bounds | Implemented | fakes only (`test_redact_and_find_secret_are_not_quadratic`) | Prefix `{0,40}`, 4000-character chunks, linear time. **A secret straddling a chunk boundary could slip through.** |
| Static file serving hardening | Implemented | fakes only (`test_static_rejects_dotdot_before_resolving`, `test_nul_in_static_path_is_404_not_500`, `test_static_allowlist_and_traversal`); verified live with `curl --path-as-is` (maintainer) | Rejects `..`, `//`, backslash, NUL, `.` segments, once/twice-decoded forms, before `resolve()`; the file must stay inside its own allowlisted directory. |
| Viewer sees config with literal secrets redacted | Implemented | fakes only (`test_viewer_gets_redacted_config_admin_gets_raw`) | Env-var names are kept; admins get raw content; `sha256` is always of the raw file. |
| Playground credential env-name allowlist | Implemented | fakes only (`test_playground_key_env_must_be_allowlisted`, `test_env_supplied_disallowed_key_name_is_ignored`) | Names set through the Settings API must be `LITELLM_*`, `NEMO_CONSOLE_PLAYGROUND_*` or `*_API_KEY`; a stored disallowed name is ignored. Names supplied by the operator's config file or environment (`NEMO_CONSOLE_PLAYGROUND_KEY_ENV`) are unrestricted. |
| Concurrency cap on validation | Implemented | fakes only (`test_parallel_validations_are_bounded`, `test_rollback_validation_takes_the_semaphore`) | One semaphore(2) covers validate, apply and rollback. |
| Switchyard routing events: identifier-shaped fields only | Implemented | fakes only (`test_routing_line_redacts_non_identifier_strings`, `test_routing_line_keeps_identifiers_and_empty_tier`) | `route`, `algorithm`, `model`, `tier`, `session_id` that are not identifier-shaped become `<redacted>`. |
| `/api/v1/health` `role` and `trust_notice` fields | Implemented | auth behaviour live (maintainer) | `role` is `admin`, `viewer` or `public`; `trust_notice` explains why loopback trust was forced off. |

## Frontend

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Views: Overview, Switchyard, Relay, Dynamo, Audit (admin), Settings | Implemented | frontend rendered against the real backend: verified live (headless Firefox) | Dynamo tabs: Status, Params, Lifecycle, Playground. Switchyard: Routing, Config, Playground, Actions. Relay: Status, Config, Diagnostics, Playground (if offered). |
| Pure-logic unit tests | Implemented | 94 passed (re-run) | Router, SSE parser, API client, formatting, diff, metrics, share, topology, param form, static guards. |
| Browsers other than headless Firefox | Implemented (untested) | **unverified** | |
| Assistive technology (screen readers, keyboard-only walk-through) | Partial | **unverified** | ARIA tabs, live regions, skip link, focus styles, reduced-motion CSS exist; none was tested with real AT. |
| Dark/light theme, English only | Implemented | unverified beyond the frontend render | Other locales: Not planned. |
| Mock server for UI development | Implemented | frontend test exists | Canned data only; default port 8796 (the real backend is 8793). |

## Desktop launcher (Linux)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| `nemo-console open`: start in the background if needed, wait for health, open the UI | Implemented | verified live (maintainer, 2026-09-28, GNOME: a cold start answered healthy in under 1 s, a second launch reused the running console, the server ran with `-I` and a 0600 pid file); tests in `test_desktop.py` | Loopback only; a non-loopback host is refused. |
| Refuses a port it cannot trust | Implemented | verified live for a same-user non-console HTTP server on the port (refused, no browser); the other-user case is fakes only (`/proc/net/tcp` parsing verified live: `{uid}` for the running console) | THREAT-MODEL S-6. A same-user impersonator that copies the `/health` shape is not detected. |
| `nemo-console stop` and the menu's **Stop Nemo Console** action | Implemented | verified live (stopped the launcher's console, pid file removed; a second `stop` reports nothing recorded); the recycled-pid and other-user refusals are fakes only | Signals only the recorded pid, and only if its owner and command line match. |
| `make install-desktop` | Implemented | verified live (entry passes `desktop-file-validate`); unsafe checkout paths refused in a scratch test | Paths limited to `[A-Za-z0-9._/+-]`. |

## Not planned for v1

Editing Relay `plugins.toml` / `config.toml`; a fourth system; multi-user or per-user audit identity beyond the key
id; key revocation UI; TLS; remote (non-loopback) deployment; localisation; active probing of the Switchyard
capable tier.
