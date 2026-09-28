# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/). Versions follow SemVer. Claims use the labels
implemented / verified-live / unverified defined in the README.

## [0.1.2] - 2026-09-28

### Added

- `nemo-console open` / `nemo-console stop` and `make install-desktop`: a desktop launcher (Linux). `open` starts the
  console in the background if nothing is serving, waits for `/api/v1/health`, then opens the UI. It uses loopback
  addresses only, and opens a page only when every listener on the port belongs to the current user and the answer
  looks like Nemo Console (a same-user process can still impersonate it: THREAT-MODEL S-6). The server runs with
  `python -I` so the launch directory is not on its import path. `stop` signals only the process it started. The
  launcher never handles keys.
- An app icon, also served as the UI's favicon (`frontend/favicon.svg`).

## [0.1.1] - 2026-09-28

First public release. 0.1.0 was an internal build and was never published; this release starts a fresh
history.

### Added

- `SECURITY.md` (private vulnerability reporting through GitHub) and a CI workflow: ruff, mypy --strict, pytest and
  the frontend `node --test` suite, with actions pinned to commit SHAs.
- Package metadata (licence, author, project URLs) and a non-affiliation notice in the README.

### Changed

- `switchyard.tier.capable_dark` matches only the `capable_target` models of `stage_router` routes in the parsed
  `routes.toml`. It used to match the alias anywhere in the file text, so an engine referenced only by a passthrough
  (`direct-27b`) or a target table kept Switchyard `degraded` while that engine was stopped on purpose. The message
  names the routes; an unparseable file yields no problem.
- The `capable_dark` fix is the container's start command from the new `engine_start` setting (`ENGINE_START`) when
  one is set, else the old compose-directory guess. Defaults for the author's deployment:
  `inference/sglang-flashnext/flashnext.sh up` and `scripts/use-model.sh qwen3.8-27b-1m` (a bare compose up is wrong
  for both).
- Default `engine_probes` adds `qwen3.8-flash-next` -> `ai-lab-sglang-flashnext-flashnext-1` (the deployment's default
  engine and capable tier since 2026-09-26); the 27B entry stays.
- Topology shows the flashnext engine (SGLang TP2, one rank on each card) beside `vllm-qwen38`.
- The Dynamo `dynamo.holds_card` fix restores the 27B with `scripts/use-model.sh qwen3.8-27b-1m` instead of a bare
  `docker compose up -d`.
- Test fixtures captured from the author's workstation are sanitised: home directories, host paths, GPU UUIDs, MAC
  and LAN addresses are neutral placeholders. Parsers are exercised on the same structure.
- Problem fixes and test skips no longer name the author's deployment repository.
- `LICENSE` names the actual copyright holder.

## [0.1.0] - 2026-09-24

Internal build, not published. Built against NeMo Switchyard 0.3.0, NeMo Relay 0.9.2 and NVIDIA Dynamo 1.5.0.

### Added

- Hexagonal core with a locked `SystemAdapter` port and three adapters (Switchyard, Relay, Dynamo).
- Loopback-bound FastAPI service (default `127.0.0.1:8793`) and a zero-build, zero-npm browser UI with a
  strict CSP: Overview, Switchyard, Relay, Dynamo, Audit and Settings views.
- Observe: status with actionable problems, canonical metrics, 15m/1h/24h history (in-memory ring plus SQLite
  rollups), events (list and SSE), routing summary, topology.
- Configure: core-owned config store (SHA-256 compare-and-swap, backup, atomic write, revisions, rollback,
  auto-rollback on failed health check); Switchyard `routes.toml` with authoritative dry-run validation; Relay
  `routes.toml` with a declared non-authoritative validator; Relay `plugins.toml` and `config.toml` read-only;
  Dynamo parameter catalog rendering a generated compose override.
- Act: danger-levelled actions with mandatory confirmation; Dynamo preflight/up/down/restart/up_mock with
  shared-GPU awareness and an opt-in, label-verified stop of the sharing engine.
- Interact: streamed, non-persisted playground with a server-side credential resolved from an env-var name.
- Security: Host allowlist, Origin and `X-Console` CSRF checks, hashed bearer keys with admin/viewer roles,
  loopback trust (switchable), forwarded-header distrust, body/SSE caps, single argv-only command runner with a
  three-binary allowlist, central redaction, append-only audit, security headers on every response.
- `nemo-console mint-key [admin|viewer]`; first-start admin key file (0600).
- Documentation: README, 14 ADRs, threat model, operations guide, feature matrix.
- Opt-in systemd user unit template in `deploy/`.

### Changed during the release cycle (docs updated to match the final code)

- `trust_loopback` defaults to **false**: an unauthenticated caller is role `public` and receives 401 everywhere except
  `GET /api/v1/health`. Loopback trust is forced off for a non-loopback host or when `allowed_hosts` is set
  (`/health.trust_notice` says why). The UI prompts for a bearer.
- Audit records only authenticated, authorised mutating calls. Denials are counted in
  `nemo_console_denied_requests_total{status}`, kept in a 100-entry ring, and logged at most once a minute.
- Rollback re-runs the validator and secret scan on the target revision (422 before writing).
- Dynamo hand-edited override is a closed allowlist (no single-dash/unknown flags, `$` only when byte-identical to a
  base-compose token, model flags equal to the base, env keys must exist in the base, fail-closed without a base).
- Switchyard and Relay routes validators error on `base_url` hosts that are not loopback and not a bare single-label
  service name, and on userinfo.
- New status problem `switchyard.tier.capable_dark` (setting `engine_probes`), making Switchyard `degraded` with summary
  prefix "capable tier dark"; shares one cached `docker inspect` probe with the topology view. Dynamo preflight emits
  `shares_card:` and `router_aliases_dark:` lines.
- Preview `warnings` are wired; default action timeout is 900 s with per-action overrides (`action_timeouts`);
  `reset_stats` needs no credential and the `switchyard.auth` problem code was removed; `/health` adds `role` and
  `trust_notice`; `/settings` shows `state_dir` `~`-relative to non-admins; the playground sends
  `Accept-Encoding: identity`; validate calls are capped by a semaphore of 2; the SSE slot is taken inside the generator;
  `NEMO_CONSOLE_URLS__X` merges over defaults; the repo-root default walks up to the compose files and a missing root is
  a reported problem; frontend stream errors parse the error envelope; the audit params blob is valid JSON by
  construction (oversize -> `{"_truncated":true,...}`); redaction and secret scanning are bounded and linear; static
  serving rejects `..`, `//`, backslash, NUL, `.` segments and encoded forms before resolving; viewers receive config
  content with literal secrets redacted; playground credential names set via the Settings API must be `LITELLM_*`,
  `NEMO_CONSOLE_PLAYGROUND_*` or `*_API_KEY`; URL userinfo is redacted in audit; one semaphore(2) covers validate, apply
  and rollback; Switchyard routing events keep only identifier-shaped route/algorithm/model/tier/session_id; the Dynamo
  override allowlist also rejects duplicate flags and restricts `--speculative-config`; an absent override reads as empty
  and the first apply creates it. See `docs/PUBLISHING-CHECKLIST.md` for what remains before a public release.

### Verification at release

- Offline gates (re-run 2026-09-25 for the final code): backend pytest 340 passed / 3 skipped in the repo checkout
  (a fresh-directory copy without `.git`: 338 passed / 5 skipped; not a `git clone`), ruff clean, mypy --strict clean (40 files), frontend node --test 94 passed.
- Verified live (maintainer-reported, one workstation): Switchyard config validate, stale-sha 409, apply with container
  recreate and health check; Dynamo preflight, `down` (400 without confirm), `up`, then serving again; playground chat 200
  through a Switchyard route; Relay `doctor_offline`, `gateway_status`, `agents_list`, `plugins_validate`,
  `gateway_restart` (PID changed, three consecutive healthy checks); the viewer role (reads 200, audit/validate/playground/actions
  403); auth behaviour
  (public 401, wrong key 401, bootstrap key -> admin); the capable-tier-dark problem; unauthenticated
  `POST /v1/stats/reset` -> 200; status/metrics/events read paths for all three; Host/Origin/CSRF/X-Forwarded-For and
  security headers; frontend rendered against the real backend (headless Firefox).
- Not verified live (fakes only): config auto-rollback, Dynamo parameter apply,
  `stop_sharing_engine`, Dynamo `restart`, assistive technology, browsers other than headless Firefox.

### Known limitations

- Capable-tier-dark detection has no request-level probe: it is a stopped-container check for containers named in
  `engine_probes` plus an error-count heuristic; a running-but-broken engine is not detected.
- Playground served-by headers are guessed and were absent from real Switchyard responses.
- Topology edges are approximations, and the edge label "agent gateway" is partly clipped behind a node.
- Relay `plugins.toml` / `config.toml` are read-only by design. The Dynamo tab has no config-file tab.
- Dynamo events are redacted log lines, not an allowlist of metadata fields (ADR-0010 deviation).
- The adapters embed container, project and unit names; the default `engine_probes` entry and one topology container name
  come from the author's deployment. Set `NEMO_CONSOLE_REPO_ROOT` for a standalone checkout.
- `NEMO_CONSOLE_HOST` can be set to a non-loopback address; nothing forbids it (loopback trust is then forced off).
- Route validators accept single-label hostnames as service names; a hostile resolver search domain could send them
  off-box. Redaction is chunked (4000 chars): a secret straddling a chunk boundary could slip through. The Dynamo
  override allowlist does not check enum-valued flags against catalog choices or type-check numbers. The systemd unit
  template is untested (namespace-based options can fail with 226/NAMESPACE on restricted distributions).
- Only the alias-to-container mapping is configurable (`ENGINE_PROBES`); other adapter names are constants.
- The Relay playground has no default credential mapping.
