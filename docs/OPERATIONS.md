# Operations

For the operator running Nemo Console. Paths shown are examples or defaults; every one is configurable unless
stated. Claims are labelled as in the README (implemented / verified-live / unverified). Commands in this
document that were run on 2026-09-24 are marked "(run)"; everything else was read from code.

## 1. Install

Requirements: Linux, `uv`, Python **3.13** (`requires-python = ">=3.13,<3.14"`), and on `PATH` (or in
`NEMO_CONSOLE_EXTRA_BIN_DIRS`, default `~/.local/bin`) whichever of `docker`, `systemctl`, `nemo-relay` the
systems you manage need. Node 22 is needed only for the frontend tests.

```bash
git clone <this repository> nemo-console && cd nemo-console
make setup        # cd backend && uv venv --python 3.13 && uv sync --frozen
make all          # ruff, mypy --strict, pytest, node --test; all offline
```

## 2. Configure

Settings are read from environment variables prefixed `NEMO_CONSOLE_` and from an optional env file
(`~/.config/nemo-console/env`, dotenv syntax, same prefixed names). Set `NEMO_CONSOLE_NO_USER_ENV=1` to ignore
the file. The process environment wins over the file.

Names read by `backend/src/nemo_console/config.py` (each is `NEMO_CONSOLE_<NAME>`):

| Name | Default | Meaning |
|---|---|---|
| `HOST` / `PORT` | `127.0.0.1` / `8793` | Listen address. **Do not change HOST** (see THREAT-MODEL R-1). |
| `STATE_DIR` | `~/.local/share/nemo-console` | SQLite DB, key files, Dynamo override. Created 0700. |
| `REPO_ROOT` | walk-up search | Root under which the adapters look for the Switchyard, Relay and Dynamo directories. The default walks up from the source file to a directory containing `inference/dynamo/docker-compose.yml`, else the current directory. A root without that file yields the status problem `console.repo_root.missing`. **Set it explicitly** for a standalone checkout. |
| `FRONTEND_DIR` | `<checkout>/frontend` | Static UI. |
| `ALLOWED_HOSTS` | `[]` | Extra `Host` header values (JSON list). |
| `URLS` | four loopback URLs (below) | JSON object. Keys: `switchyard`, `relay`, `dynamo`, `dynamo_worker_metrics`, `litellm`. |
| `ALLOWED_URL_HOSTS` | `["127.0.0.1","localhost","::1"]` | Hosts the Settings page may point URLs at. |
| `PLAYGROUND_KEY_ENV` | `{"switchyard":"LITELLM_API_KEY"}` | JSON: logical system -> **name** of the env var holding its playground credential. |
| `TRUST_LOOPBACK` | `false` | Opt in to treating key-less loopback callers as admin. Forced off when `HOST` is not loopback or `ALLOWED_HOSTS` is set (see `trust_notice` in `/health`). |
| `BACKGROUND_TASKS` | `true` | Run the metrics poller. |
| `POLL_INTERVAL_S` | `2.0` | Poll cadence (Settings accepts 0.5 to 60). |
| `RING_SECONDS` | `900` | In-memory metric window. |
| `STATUS_TTL_S`, `ADAPTER_TIMEOUT_S`, `ACTION_TIMEOUT_S` | `2.0`, `8.0`, `900.0` | Status cache, per-probe timeout, default per-action timeout. |
| `ACTION_TIMEOUTS` | `{}` | JSON object of per-action overrides keyed `system.action`, e.g. `{"dynamo.up": 1200}`. |
| `ENGINE_PROBES` | two default aliases to containers | JSON object alias -> engine container name. If the alias is the `capable_target` model of a `stage_router` route in Switchyard's `routes.toml` (parsed; `direct-*` passthroughs do not count) and its container is not running, Switchyard is `degraded` with the problem `switchyard.tier.capable_dark`. The default entries name containers from the author's deployment: replace them for yours. |
| `ENGINE_START` | two default containers to commands | JSON object engine container name -> the command (run from the repo root) shown as the `capable_dark` fix. Display only: the console never runs it. A container without an entry gets `cd <compose dir> && docker compose up -d`. The defaults use the author's start scripts because a bare compose up is wrong for both of those engines. |
| `BODY_CAP_BYTES`, `CONFIG_CAP_BYTES` | `1048576`, `262144` | Request and config size limits. |
| `SSE_MAX_CONNECTIONS`, `SSE_IDLE_TIMEOUT_S`, `SSE_HEARTBEAT_S` | `8`, `300`, `15` | Event stream limits. |
| `PLAYGROUND_MAX_TOKENS` | `4096` | Clamp for chat `max_tokens`. |
| `REVISIONS_KEEP` | `50` | Config revisions kept per file. |
| `OTLP_ENDPOINT` | unset | e.g. `http://localhost:4317`; needs the optional `opentelemetry` packages (not installed by default; otherwise one warning is logged). Unverified. |
| `EXTRA_BIN_DIRS` | `["~/.local/bin"]` | Extra directories searched for the three allowed binaries. |

Default URLs: `switchyard http://127.0.0.1:4010`, `relay http://127.0.0.1:4040`, `dynamo http://127.0.0.1:8020`,
`dynamo_worker_metrics http://127.0.0.1:8081`, `litellm http://127.0.0.1:4000`.

Other environment names: `NEMO_CONSOLE_NO_USER_ENV` (skip the env file) and **whichever variable you named** in
`PLAYGROUND_KEY_ENV` (default `LITELLM_API_KEY`). The credential must be in the **process** environment. The console's
own env-file loader only reads `NEMO_CONSOLE_*` names, so `LITELLM_API_KEY` there is not enough when you start it by hand;
the systemd template's `EnvironmentFile=-%h/.config/nemo-console/env` does put every variable in that file into the
process environment. Names set through the Settings page or API must be `LITELLM_*`, `NEMO_CONSOLE_PLAYGROUND_*` or
`*_API_KEY`; names from `NEMO_CONSOLE_PLAYGROUND_KEY_ENV` or the env file are not restricted.

Nested variables such as `NEMO_CONSOLE_URLS__SWITCHYARD=http://127.0.0.1:14010` are merged over the built-in
defaults (a validator in `config.py`; an earlier revision replaced the whole object, so re-check if you use an old
checkout). The same merge applies to `ENGINE_PROBES`, `ENGINE_START` and `PLAYGROUND_KEY_ENV`.

URLs changed at runtime on the Settings page are stored in the database, validated (plain `http`, host in
`ALLOWED_URL_HOSTS`, no credentials or query) and take effect for adapters only **after a restart**
(the page shows "restart required").

Adapter constants that are not settings: container names, compose project and service names, the systemd unit name
and the router liveness path (`/health/liveliness`). They live at the top of `adapters/*.py` and
`services/topology.py`. Edit them if your deployment differs.

## 3. Run

```bash
make run                                  # foreground, http://127.0.0.1:8793   (run: starts, /api/v1/health answers)
# or
cd backend && uv run nemo-console
```

Logs are JSON lines on stderr (secrets and tracebacks scrubbed). The console's own Prometheus metrics are at
`/metrics` (viewer role). The port answers `GET /api/v1/health` without a key:

```bash
curl -s http://127.0.0.1:8793/api/v1/health
# {"status":"degraded","version":"0.1.2","trust":"bearer","role":"public","trust_notice":null,"systems":{...}}   (run, unauthenticated)
```

`status` is `ok` only when all three systems are `up`. A system that is down is reported, not an error.

### From the desktop (opt-in, Linux)

```bash
make install-desktop      # writes $XDG_DATA_HOME/applications/nemo-console.desktop; `make uninstall-desktop` removes it
```

The menu entry runs `nemo-console open`; its right-click action **Stop Nemo Console** runs `nemo-console stop`.

- `open` only uses a loopback address (a non-loopback `NEMO_CONSOLE_HOST` is refused). If the console already
  answers, it opens the UI with `xdg-open`. If nothing answers, it starts the console in the background
  (`python -I -m nemo_console.main`, so the launch directory is not on its import path; log in
  `<state dir>/console.log`, mode 0600, rotated past 10 MiB; pid in `<state dir>/console.pid`), waits up to 30 s for
  health, then opens the UI.
- It opens the page only if **every socket listening on the port belongs to your user** (read from `/proc/net/tcp`
  and `tcp6`) **and** the answer looks like Nemo Console. This stops another user's process that holds the port from
  collecting your key. A process running as your own user, including a rootless-Docker published port, can still
  impersonate it (THREAT-MODEL S-6).
- It never touches keys: each new window asks for one.
- `stop` signals only the process recorded in `console.pid`, and only if it is yours and its command line is exactly
  the launcher's. A console started this way keeps running after logout when lingering is enabled
  (`loginctl show-user $USER -p Linger`).
- Failures also show as a desktop notification when `notify-send` exists.

### As a systemd user unit (opt-in)

`deploy/nemo-console.service` is a template; nothing installs it for you. It runs as **your** user on purpose
(the console drives your rootless Docker and `systemctl --user`).

```bash
mkdir -p ~/.config/systemd/user && cp deploy/nemo-console.service ~/.config/systemd/user/
# edit WorkingDirectory / ExecStart to your checkout (template assumes %h/nemo-console/backend)
systemctl --user daemon-reload && systemctl --user enable --now nemo-console.service
journalctl --user -u nemo-console -f
```

Unverified: the unit has not been started under systemd, and hardening options that rely on user namespaces (for example
`PrivateTmp`, `ProtectSystem`, `PrivateUsers`) can fail with status `226/NAMESPACE` on distributions that restrict
unprivileged user namespaces; if the unit fails that way, comment out such lines (they are optional). Things you will
probably need to add: `Environment=`
lines for `NEMO_CONSOLE_REPO_ROOT` and any `DOCKER_HOST` your rootless Docker needs (the runner passes only a
short allowlist of variables to children), and `EnvironmentFile=` (mode 0600) or `Environment=` for the
playground credential variable.

## 4. Keys and roles

- **First run, step by step** (a key is now required; there is no key-less mode by default):
  1. `make run` (or start the unit). The log line `first start: admin API key written to <path> (0600); copy it, then
     delete the file` names the file `<STATE_DIR>/bootstrap-admin.key`.
  2. Open `http://127.0.0.1:8793`. `/api/v1/health` is open, reports role `public`, and the UI opens a Bearer dialog
     because of it (any 401 also opens it).
  3. Paste the key into the Bearer dialog (or later: key icon in the top bar, or Settings). It is kept in the tab's
     `sessionStorage`, so closing the tab forgets it.
  4. Delete `bootstrap-admin.key` once it is stored somewhere safe (a password manager).
  If you lost it: `nemo-console mint-key admin` prints **only the file path** (`admin key written to <path> (0600); it is
  not shown here`); read the key from that file. It works while the server runs.
- **First start** with an empty database creates an admin key and writes it to
  `<STATE_DIR>/bootstrap-admin.key` (mode 0600, created exclusively); the log line
  `first start: admin API key written to <path> (0600); copy it, then delete the file` gives the path (run).
  The key itself is never logged. Only its SHA-256 is stored.
- **Mint more:** `nemo-console mint-key admin` or `nemo-console mint-key viewer` (default `viewer`). It writes
  `<STATE_DIR>/minted-<role>-<id>.key` (0600) and prints only the path. It can run while the server is up (run).
  From `backend/`: `uv run nemo-console mint-key viewer`.
- **Use a key:** `Authorization: Bearer <key>`; non-GET requests also need `X-Console: 1`.

```bash
KEY=$(cat ~/.local/share/nemo-console/bootstrap-admin.key)
curl -s -H "Authorization: Bearer $KEY" http://127.0.0.1:8793/api/v1/systems
curl -s -X POST -H "Authorization: Bearer $KEY" -H 'X-Console: 1' -H 'Content-Type: application/json' \
     -d '{"params":{}}' http://127.0.0.1:8793/api/v1/systems/dynamo/actions/preflight
```

- **Browser:** paste the key into the Bearer dialog (first-run steps above). Only if you set
  `NEMO_CONSOLE_TRUST_LOOPBACK=true` (and the host is loopback and `ALLOWED_HOSTS` is empty) is a key-less local
  browser admin. `GET /api/v1/health` returns `role` (`admin`, `viewer`, `public`) and a `trust_notice` that is `null` unless you
  opted in to loopback trust and it was forced off (then it says why).
- **Roles:** `viewer` may `GET`. Everything else, including "safe" actions such as `preflight` (they are POSTs) and
  the playground, needs `admin`. `GET /api/v1/audit` is admin-only.
- **Revoking a key:** there is no command in v0.1.0. Stop the console and delete the row from the `keys` table in
  `<STATE_DIR>/console.db` with `sqlite3`.
- **Audit and denials:** the audit table (`GET /api/v1/audit`, admin) records authenticated, authorised mutating calls
  only. Denied requests (401/403/421/429, viewer 403s) are counted in `nemo_console_denied_requests_total{status}` on
  `/metrics`, kept in a 100-entry in-memory ring, and logged as at most one aggregated line per minute
  (`denied requests in the last 60s: 401=..`). A burst of 401s in that line means someone is probing or a client has no key.

## 5. State, backup and rollback

State directory contents: `console.db` (+ `-wal`, `-shm`): keys (hashed), append-only audit, config revisions
(full previous file bytes), 1-minute metric rollups (8 days), settings and saved Dynamo parameters;
`bootstrap-admin.key` / `minted-*.key`; `dynamo/override.yml` (0600) once parameters are applied.

**Back up the console:** stop it, copy the directory (or, running, `sqlite3 console.db ".backup /path/backup.db"`).
Treat the copy as sensitive (it holds config revisions, key hashes and audit paths). Losing it loses rollback
history and keys; managed systems are unaffected.

**Roll back a configuration change:**

1. Automatic: if the post-apply check fails the console restores the previous bytes, re-runs the check and returns
   `rolled_back: true` (the UI shows an outcome banner). Read the `detail`; if it ends with
   "still unhealthy after restoring the backup - investigate manually" the previous file is also failing.
   (Mechanism: implemented and tested with fakes; **not observed live**.)
2. Manual: open the file's history in the config editor and roll back to a revision, or
   `GET /api/v1/systems/<id>/config/<file>/history` then
   `POST .../rollback` with `{"revision_id":"<id>","confirm":true}`. Each apply and each rollback stores a
   "pre-apply/pre-rollback backup", so a rollback is itself reversible. A rollback first runs the target revision
   through the same validator and secret scan as an apply (422 before any write if it fails). Up to `REVISIONS_KEEP` (50) are kept.
3. Outside the console: the live file is an ordinary file; restore it from version control and recreate/restart
   the system yourself.

Dynamo parameters: delete `<STATE_DIR>/dynamo/override.yml` and run the `restart` action to fall back to the
reviewed compose file.

**Roll back the console itself:** `git checkout <tag>` then `make setup`. Database migrations are forward-only and
only `0001` exists at v0.1.0, so no downgrade path has been exercised.

## 6. What the console needs from each system

| System | Needs | Console reads |
|---|---|---|
| Switchyard | container running as the compose service the adapter expects, HTTP on the configured URL; `docker` on PATH | `/health`, `/v1/models`, `/v1/stats`, `/metrics`, `routing.jsonl` via `docker exec ... tail`, `routes.toml` |
| Relay | user unit `nemo-relay-gateway.service`, `systemctl` and `nemo-relay` on PATH, gateway on the configured URL | `systemctl --user show`, `GET /v1/models` (any HTTP answer under 500 means up; 401 is expected), `plugins list --json`, ATOF directory listing and tail, `plugins.toml`, `config.toml`, `routes.toml` |
| Dynamo | compose project with `dynamo-frontend`, `dynamo-worker` (and optional `--profile mock` mocker); `docker` on PATH | frontend `/health`, `/v1/models`, `/metrics`; worker metrics read through `docker exec` (the worker port is not reachable from the host under rootless Docker); `docker logs` |

## 7. Troubleshooting

Keyed on what you see. "Code" is the `error.code` in the JSON body `{"error":{"code","message"}}`; the message the UI
shows is prefixed with the code words for security rejections.

### Rejections from the security layer

| You see | Cause | Fix |
|---|---|---|
| 421 `misdirected request: unrecognised Host header` | The `Host` header is not `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>` (or `HOST:PORT`, or an `ALLOWED_HOSTS` entry). Typical: reverse proxy, a different port than configured, an IP alias. | Browse `http://127.0.0.1:8793`; if you changed `PORT`, use it. Add the exact `host:port` to `NEMO_CONSOLE_ALLOWED_HOSTS` only if you know why. |
| 403 `cross origin: cross-origin requests are not allowed` | An unsafe request carried an `Origin` (or `Origin: null`) that is not the console's own host. | Use the console UI from its own origin; strip `Origin` in scripts. |
| 403 `csrf header required: send the header 'X-Console: 1' on non-GET requests` | Missing `X-Console: 1`. Required even with a bearer. | `curl -H 'X-Console: 1' ...` |
| 401 `unauthorized: a valid bearer API key is required` | No or wrong key. Unless you opted in with `TRUST_LOOPBACK=true` every caller needs one, and loopback trust is refused when `X-Forwarded-For`/`Forwarded` is present, the host is non-loopback or `ALLOWED_HOSTS` is set. A wrong key never falls back to loopback trust. Only `/health` is open. | Send `Authorization: Bearer <key>`; mint one (section 4). |
| 403 `forbidden` "this key has the viewer role (read-only)" / "this action needs the admin role" | A viewer key on a POST/PUT, on the playground, or on the audit list. | Use an admin key. |
| 413 `payload_too_large` | Body over 1 MiB (also detected while streaming a chunked body). | Send less. |
| 404 `not_found` "no such file" | Path outside the static allowlist (including `/docs`; Swagger is disabled). | Use the UI at `/`. |
| Browser shows a blank page, console errors mention CSP | A local edit added an inline script/style or external origin. | The `static-guards` frontend test finds these: `make frontend-test`. |

### Configuration and actions

| You see | Cause | Fix |
|---|---|---|
| 409 `stale_base` "the file changed since you loaded it; reload and re-apply" | Someone (or a previous apply) changed the file after you loaded it. | Reload the editor, merge, re-apply. |
| 409 `busy` "another change to <system> is in progress" | A restart/apply/action on the same system is running. | Wait; Dynamo `up` can take minutes. |
| 400 `confirm_required` | Disruptive/destructive action, apply, rollback or params PUT without `confirm:true`. | Confirm in the UI or add `"confirm":true`. |
| 403 `read_only` | You tried to apply a Relay `plugins`/`config` file or the Dynamo compose file. By design. | Edit the file by hand. |
| 403 `symlink_refused` / `path_changed` | The config path is a symlink, or now resolves somewhere other than at startup. | Make the path a regular file; restart the console. |
| 413 `config_too_large` | File or content over 256 KiB. | Reduce it. |
| 422 `validation_failed` `<validator>: <message>` | The validator returned `ok=false`; for Switchyard this includes the real `--dry-run` message. Also returned for non-authoritative validators. | Fix the reported line; use "validate" first. |
| 422 `validation_failed` "revision no longer validates (...)", 422 `secret_in_content` "refusing to restore a revision that contains ...", or 422 `invalid_revision` | A **rollback** target is re-validated and re-scanned before anything is written; the old revision fails today's rules (for example an off-box `base_url`, now an error) or is not UTF-8. Nothing was changed. | Pick another revision, or fix the live file by hand. |
| 422 `validation_failed` with "base_url ... is neither loopback nor a bare service name" / "must not contain userinfo" | Switchyard and Relay routes validators reject any `llm_client` `base_url` whose host is not loopback (127.0.0.0/8, `::1`, `localhost`) or a bare single-label service name (no dots), and any `user:pass@host`. | Point at a local or in-network service name. |
| 422 `validation_failed` on the Dynamo `override` file: "flag ... is not on the console allowlist", "`$` in ... is refused", "`--model` must equal the base compose's value", "environment X is not defined in the base compose" | The hand-edited override is a closed allowlist (no single-dash or unknown flags; `$` only in tokens byte-identical to the base compose; model flags fixed; env keys must already exist in the base; fail-closed if the base compose cannot be read). | Use the Params tab, or edit the reviewed compose file in version control. |
| 422 `secret_in_content` "refusing content that contains ..." | The scan found a key-shaped or `key = "literal"` value. | Reference an env-var name instead (`api_key_env = "NAME"`). |
| 422 `invalid_setting` "credential settings take an ENV VAR NAME ...; allowed: LITELLM_*, NEMO_CONSOLE_PLAYGROUND_*, *_API_KEY" | The Settings API refuses other environment-variable names for the playground credential. | Use an allowed name, or set `NEMO_CONSOLE_PLAYGROUND_KEY_ENV` in the environment/env file. |
| Config editor shows `[redacted]` in place of a value while the file has a literal | Viewers get config content with literal secrets redacted (env-var names kept); admins see raw. The SHA-256 is of the raw file either way. | Use an admin key. |
| `GET /` or an asset returns 404 for a path with `..`, `//`, a backslash, NUL or a `.` segment (also when percent-encoded) | Static serving rejects such paths before resolving them. | Request the canonical path. |
| 422 `invalid_param` `param '<k>': ...` | Value outside the catalog's type, bounds or choices, or an unknown key. | Use the form; check bounds. |
| 404 `unsupported` | Params endpoints on a system with no catalog (only Dynamo has one). | n/a |
| Apply result `post_check: "unhealthy"`, `rolled_back: true` | Container or unit did not become healthy; previous file restored. | Read `detail`; fix the config; the old one is running again. |
| Switchyard `dry-run could not start (image ... missing?)` | Validation runs `docker run --pull never` on the Switchyard image; it is not present locally. The image name is taken from the running container, or a built-in fallback name if that container is absent. | Start the service once (or build/load the image) so the image exists locally. |
| Action result `console.action.failed` "timed out" | The action exceeded its timeout: `ACTION_TIMEOUT_S` (default **900 s**) or an `ACTION_TIMEOUTS` entry such as `dynamo.up`. Dynamo `up` waits up to 600 s for health, so a normal cold start (about 4 minutes) fits. | Check the Dynamo tab status ("loading" is `degraded`); raise the timeout if your model loads slower. |
| Dynamo `refused: the Workstation card is in use by ...` (`dynamo.up.card_in_use`) | Another container is running on the worker's GPU. | Re-run `up` with "stop the sharing engine" ticked (stops one label-verified container), or stop it yourself. If the message lists other containers, the console never stops those. |
| Dynamo `refused: the CPU mocker is running` / `refused: the real worker is running (same model name as the mocker)` | Mocker and worker cannot coexist. | Run `down` first. |
| Dynamo `cannot determine the worker's GPU UUID from the compose or the container` (`dynamo.preflight.failed`) | Compose path wrong (`REPO_ROOT`) or the worker has no device pin. | Fix `REPO_ROOT`; check the compose pins a UUID. |
| `dynamo.up.unhealthy` | Worker recreated but `/v1/models` stayed empty for 600 s. | `docker logs --tail 80 <worker>`; check VRAM. |

### Playground

| You see | Cause | Fix |
|---|---|---|
| 503 `credential_missing` "env var <NAME> is not set in the console process" | The named variable is not in the console's environment. | Export it before starting (or `Environment=` in the unit); Settings shows `[unset]`. |
| 502 `upstream_unreachable` "could not reach the system: <ExceptionName>" | System down, wrong URL, or stopped engine. | Check that system's tab. |
| 502 `upstream_error` "system answered 401: ..." | The credential is wrong, or (Relay) no credential is configured for that system: only `switchyard` has a default mapping. | Add a `relay` entry to `PLAYGROUND_KEY_ENV` / Settings pointing at a variable that holds a valid key. |
| 502 `upstream_error` "system answered 4xx/5xx" | The system rejected the request (e.g. a model alias whose engine is stopped, a "capable" tier that is dark). | Choose a `direct-*` route or start the engine. |
| Playground/event stream fails with an HTTP error | The frontend now parses the `{error:{code,message}}` envelope (`parseErrorBody` in `frontend/js/sse.js`); 4xx is final, no auto-retry (the UI offers Retry). Not exercised against every error in a browser. | If the message is unclear, reproduce with `curl` to read the body. |
| 429 `too_many_streams` "at most 8 concurrent event streams" | Too many open event panels/tabs. | Close tabs; idle streams end after 5 minutes. |
| "Served by" badges empty | The system did not send a recognised served-by header (real Switchyard did not). Not an error. | None. |

### Status problems (shown on the system tabs; each has an exact fix)

| Code | Meaning / first move |
|---|---|
| `switchyard.down` | `/health` did not answer. Run the shown `docker compose up -d --no-build`. |
| `switchyard.config.drift` | `routes.toml` differs from the running server's routes; recreate the container (shown command). |
| `switchyard.tier.capable_dark` | A container listed in `ENGINE_PROBES` is not running (or absent) and its alias is the capable-tier model of a `stage_router` route (the message names the routes). Switchyard turns `degraded` with a summary starting "capable tier dark". The fix is the container's `ENGINE_START` command, else the compose directory to `up`. It shares one cached `docker inspect` probe with the topology view; a probe error or an unparseable `routes.toml` yields no problem. |
| `switchyard.capable_tier.upstream_errors` | Inferred from error counts in `/v1/stats` that the capable-tier engine is failing. **Heuristic:** no active probe, so it appears only after traffic hit the failing tier and never for an idle one. |
| `switchyard.routing.fallback_unavailable` | Requests were re-routed because a target was unavailable. |
| `relay.gateway.down` | Gateway URL does not answer; run `systemctl --user restart nemo-relay-gateway.service`. A 401 from `/v1/models` is **up**, not a failure. |
| `relay.plugin.attestation_weak` / `relay.exporters.off` / `relay.pii_redaction.off` / `relay.plugin.none_configured` | Policy findings from `plugins.toml`. Human decisions; the console will not change them. |
| `relay.config.newer_than_gateway` | `plugins.toml` changed after the gateway started; restart the gateway. |
| `relay.atof.large` | ATOF directory over 1 GiB; Relay has no retention. Prune rotated files by hand. |
| `dynamo.frontend.down` / `dynamo.worker.absent` / `dynamo.worker.stopped` | Containers not running; use the `up` action or the shown compose command. |
| `dynamo.card.contended` | Both engines run on the same GPU: one will fail to allocate. |
| `dynamo.holds_card` | Dynamo is up, the other engine is stopped, so its router aliases are dark. Informational. |
| `dynamo.structural_tag.off`, `dynamo.parser.dialect`, `dynamo.router.kv` | Worker/frontend flags likely wrong for this model; use the Params tab. |
| `console.adapter.error` / health `unknown` "status check failed" | An adapter raised. Check the console log (JSON, exception class only) and fix that system's paths (`REPO_ROOT`). |

### The console itself

| Symptom | Cause / fix |
|---|---|
| All three systems `unknown` or "absent", paths look wrong | Look for the status problem `console.repo_root.missing`: `REPO_ROOT` does not contain the expected compose files. Set `NEMO_CONSOLE_REPO_ROOT` (section 2). |
| Docker-based checks fail under systemd but work in a shell | The runner passes only `PATH HOME XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS DOCKER_HOST DOCKER_CONTEXT DOCKER_CONFIG LANG LC_ALL`. Add `DOCKER_HOST` / `DOCKER_CONTEXT` to the unit. Unverified under systemd. |
| `binary 'nemo-relay' not found on PATH` in a problem text | Binary outside `PATH`; add its directory to `EXTRA_BIN_DIRS`. |
| History charts empty after a restart | The 15 minute ring is in memory; 1h/24h use rollups written once a minute. Wait a minute. |
| Topology shows an engine `down` although you never run it | The topology probe checks one hard-coded container name (`SHARED_ENGINE` in `services/topology.py`). Edit it. |
| Live smoke tests skipped | Gating differs per adapter: Switchyard `NEMO_LIVE=1`, Relay `NEMO_CONSOLE_LIVE=1`, Dynamo automatic when `:8020/health` answers. |

## 8. Verification you can repeat

```bash
make all                                   # offline gates
cd backend && NEMO_LIVE=1 NEMO_CONSOLE_LIVE=1 uv run pytest -q -rs   # read-only live smoke, needs the real stack
```

The live tests never run a disruptive action. Actions that change state (recreate, restart, up/down) have been
run through the console only for the items marked verified-live in FEATURE-MATRIX.
