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
| `PLAYGROUND_KEY_ENV` | `{"switchyard":"LITELLM_API_KEY","relay":"LITELLM_API_KEY"}` | JSON: logical system -> **name** of the env var holding its playground credential. Relay's gateway answers 401 without a bearer, so it has a default too (2026-10-17); `NEMO_CONSOLE_PLAYGROUND_KEY_ENV__RELAY=OTHER_API_KEY` points it elsewhere. |
| `TRUST_LOOPBACK` | `false` | Opt in to treating key-less loopback callers as admin. Forced off when `HOST` is not loopback or `ALLOWED_HOSTS` is set (see `trust_notice` in `/health`). |
| `BACKGROUND_TASKS` | `true` | Run the metrics poller. |
| `POLL_INTERVAL_S` | `2.0` | Poll cadence (Settings accepts 0.5 to 60). |
| `RING_SECONDS` | `900` | In-memory metric window. |
| `STATUS_TTL_S`, `ADAPTER_TIMEOUT_S`, `ACTION_TIMEOUT_S` | `2.0`, `8.0`, `900.0` | Status cache, per-probe timeout, default per-action timeout. |
| `ACTION_TIMEOUTS` | `{}` | JSON object of per-action overrides keyed `system.action`, e.g. `{"dynamo.up": 1200}`. |
| `ENGINE_PROBES` | five aliases to containers (Super VL = the Dynamo worker, Bolt, GLM, flashnext, the 27B) | JSON object alias -> engine container name. If the alias is the `capable_target` model of a `stage_router` route in Switchyard's `routes.toml` (parsed; `direct-*` passthroughs do not count) and its container exists but is not running, Switchyard is `degraded` with the problem `switchyard.tier.capable_dark`; a container that does not exist is the info problem `switchyard.tier.capable_not_deployed` and changes nothing. The default entries name containers from the author's deployment: replace them for yours. |
| `ENGINE_START` | one command per default probe container | JSON object engine container name -> the command (run from the repo root) shown as the `capable_dark` fix. Display only: the console never runs it. A container without an entry gets `cd <compose dir> && docker compose up -d`. Each default says in a `#` comment what it stops or needs (starting one of these engines tears down whatever holds its cards). |
| `TOPOLOGY_ENGINES` | lightning (always on), Bolt, GLM, flashnext, the 27B (opt-in) | JSON list of `{"id","label","container","opt_in"}`; REPLACES the default. Engine nodes of the topology: running = up, stopped = `off` (opt-in) or `down`, no such container = `off` labelled "(not deployed)". An id that repeats or shadows a fixed node is dropped. |
| `DYNAMO_OPT_IN` | `true` | Dynamo is opt-in: with no frontend, worker or mocker container running or restarting it is `health=off` with one info problem `dynamo.off` (not `down`), and `/health` ignores it. `false` restores the error-level report. |
| `DYNAMO_START` | `scripts/use-model.sh qwen3.8-27b-dynamo` | Command (run from the repo root) shown as the fix wherever the BASE Dynamo stack should be started: `dynamo.off`, and on the base stack `dynamo.frontend.down`, `dynamo.worker.absent` and `up` refused by an exclusive engine. Display only. While the stack runs from a repo override file (e.g. Super VL) those fixes are the console's own `up` instead, which uses the files in use. |
| `DEFAULT_MODEL_START` | `scripts/use-model.sh   # lists the engines; ...` | Command shown as the `dynamo.holds_card` fix and in preflight. Neutral on purpose: without an alias the script only lists the engines; switching away from Dynamo (which stops it) is your choice of alias. Display only. |
| `DYNAMO_EXCLUSIVE_ENGINES` | Bolt, GLM (`lil-glm53-tp2`), flashnext and vllm-qwen38 containers | JSON list. Running on any of the worker's cards they cannot coexist with it: `up` is refused and, beside a running or restarting worker, `dynamo.card.contended`. Only `ai-lab-vllm-qwen38-vllm-1` can ever be stopped by the console (`stop_sharing_engine`). |
| `DYNAMO_COEXISTING_ENGINES` | lightning, ollama and chatterbox-tts containers | JSON list. Allowed on the worker's cards; never block `up`. |
| `DYNAMO_IGNORED_CONTAINERS` | the dcgm-exporter container | JSON list. GPU-visible monitoring; never blocks `up`. Any running container on the card that is in none of the three lists blocks `up` and is named (never stopped). |
| `DYNAMO_DARK_WHILE_HELD` | the exclusive engines' aliases and routes (Bolt, GLM, flashnext, the 27B) | JSON list of what cannot answer while Dynamo holds the card(s); shown in `dynamo.holds_card` (info; `[]` turns it off) and the preflight `router_aliases_dark:` line. |
| `BODY_CAP_BYTES`, `CONFIG_CAP_BYTES` | `1048576`, `262144` | Request and config size limits. |
| `SSE_MAX_CONNECTIONS`, `SSE_IDLE_TIMEOUT_S`, `SSE_HEARTBEAT_S` | `8`, `300`, `15` | Event stream limits. |
| `SHUTDOWN_GRACE_S` | `5` | Seconds uvicorn waits for open connections on SIGTERM before closing them. Open event streams already end at once (`event: shutdown`); keep it below `nemo-console stop`'s 10 s. |
| `PLAYGROUND_MAX_TOKENS` | `4096` | Clamp for chat `max_tokens`. |
| `REVISIONS_KEEP` | `50` | Config revisions kept per file. |
| `OTLP_ENDPOINT` | unset | e.g. `http://localhost:4317`; needs the optional `opentelemetry` packages (not installed by default; otherwise one warning is logged). Unverified. |
| `EXTRA_BIN_DIRS` | `["~/.local/bin"]` | Extra directories searched for the five allowed binaries. |
| `SWITCHYARD_CONTAINER`, `SWITCHYARD_COMPOSE_PROJECT`, `SWITCHYARD_COMPOSE_SERVICE` | `ai-lab-switchyard-switchyard-1`, `ai-lab-switchyard`, `switchyard` | The Switchyard container and the compose labels it must carry (a container with other labels is treated as absent). |
| `SWITCHYARD_IMAGE` | `ai-lab-switchyard:0.3.0` | Image the routes validator's `--dry-run` uses when the container cannot be inspected. |
| `SWITCHYARD_LOG_PATH` | `/var/log/switchyard/routing.jsonl` | The routing log inside the container (absolute; read with `docker exec ... tail`). |
| `RELAY_UNIT` | `nemo-relay-gateway.service` | systemd `--user` unit of the Relay gateway (must end in `.service`). |
| `DYNAMO_COMPOSE_PROJECT` | `ai-lab-dynamo` | Compose project label the Dynamo containers must carry. |
| `DYNAMO_SERVICES` | `frontend dynamo-frontend`, `worker dynamo-worker`, `mocker dynamo-mocker`, `sv_frontend dynamo-sv-frontend` | JSON object role -> compose service; single roles merge over the defaults (`NEMO_CONSOLE_DYNAMO_SERVICES__WORKER=...`). The generated override and the GPU parse use these names. |
| `DYNAMO_CONTAINERS` | `ai-lab-dynamo-dynamo-<service>-1` per role | JSON object role -> container name; merges like `DYNAMO_SERVICES`; each role needs its own name. |
| `GUARDRAILS_COMPOSE_PROJECT`, `DCGM_COMPOSE_PROJECT` | `ai-lab-nemo-guardrails`, `ai-lab-observability` | Compose project labels checked before the console restarts those containers. |
| `LAYOUT` | `switchyard inference/switchyard`, `relay clients/nemo-relay`, `dynamo inference/dynamo`, `skills .claude/skills`, `data_designer clients/data-designer`, `openshell_runner clients/data-designer/runner`, `observability observability`, `aiperf clients/aiperf`, `guardrails security/nemo-guardrails`, `evaluator clients/nemo-evaluator`, `skillevaluator clients/skillevaluator`, `ollama inference/ollama` | JSON object key -> directory under `REPO_ROOT` (relative, plain segments; no `..`, absolute path or backslash). Set single keys: `NEMO_CONSOLE_LAYOUT__SWITCHYARD=deploy/switchyard`; unknown keys are refused. The file names inside each directory (`routes.toml`, `docker-compose.yml`, ...) are fixed. |
| `CAPABLE_PROBE` | `false` | Active capable-tier probe (see below). |
| `CAPABLE_PROBE_INTERVAL_S`, `CAPABLE_PROBE_TIMEOUT_S`, `CAPABLE_PROBE_RETRY_S` | `300`, `60`, `30` | At most one probe per alias per interval (minimum 300); per-request timeout; delay before the one retry of a failed probe. |
| `CAPABLE_PROBE_KEY_ENV` | `LITELLM_API_KEY` | **Name** of the env var holding the router key the probe sends (same naming rule as the playground's: `LITELLM_*`, `NEMO_CONSOLE_PLAYGROUND_*`, `*_API_KEY`). |

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
checkout). The same merge applies to `ENGINE_PROBES`, `ENGINE_START` and `PLAYGROUND_KEY_ENV`. Lists (`DYNAMO_*`,
`TOPOLOGY_ENGINES`) replace their default.

URLs changed at runtime on the Settings page are stored in the database, validated (plain `http`, host in
`ALLOWED_URL_HOSTS`, no credentials or query) and take effect for adapters only **after a restart**
(the page shows "restart required").

Every value above that names a container, compose project, service, unit or path is checked with a full match when the
console starts (a leading `-`, a space, `..` or an unknown key is refused, and the console does not start). None of
them can be changed through the API. Still constants in `adapters/*.py`: the one container Dynamo's `up` may stop
(`stop_sharing_engine`; a setting must not widen what the console stops), OpenShell's sandbox network and router address
(they must match the lab wrapper), and the router liveness path (`/health/liveliness`).

**Active capable-tier probe (`CAPABLE_PROBE=true`).** The passive checks only see a capable-tier container that is
stopped. A container can run while its engine cannot answer (a tensor-parallel rank died, `/health` still says 200). With
the probe on, for every capable-tier alias whose container is running, the console sends one chat completion through
the LiteLLM router (`max_tokens: 1`, `temperature: 0`, the word `ping`, LiteLLM's response cache bypassed), in the
background, one request at a time, at most once per `CAPABLE_PROBE_INTERVAL_S` per alias. A failure is retried once
after `CAPABLE_PROBE_RETRY_S`; two failures raise `switchyard.tier.capable_broken` and Switchyard turns `degraded`
("capable tier not answering"). Only the verdict and an HTTP status or error class are kept and logged, never the answer.
Each probe is a real router request: it appears in the router's logs, traces and spend under the key you name, so a key
of its own (with access to the capable-tier aliases only) keeps it separate. A busy engine that queues the probe behind
long prefills for longer than `CAPABLE_PROBE_TIMEOUT_S` twice in a row is reported broken; raise the timeout if that
happens. The Dynamo compose FILES are not a constant: up/down/restart use the files the running containers
were started from (their compose labels, checked to lie inside the configured compose directory), and the configured
`inference/dynamo/docker-compose.yml` only when no Dynamo container exists (ARCHITECTURE 9.6).

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
# {"status":"degraded","version":"0.2.0","trust":"bearer","role":"public","trust_notice":null,"systems":{...}}   (run, unauthenticated)
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
reviewed compose file. While the stack runs from a repo override file (e.g. Super VL) the console override is not used
at all and Params are refused.

SkillEvaluator runs: `<STATE_DIR>/skillevaluator/runs/<UTC stamp>-<skill>/` (0700), newest 200 kept. Each holds
`run.json` (0600, metadata), the Tier 1/2 reports, and for a Tier 3 run SkillEvaluator's tree
`<skill>/<run>/result.json`, `report.html` and `trials/` (the agents' transcripts: treat as sensitive). Delete a run
directory to forget it; the console never serves these files.

**Roll back the console itself:** `git checkout <tag>` then `make setup`. Database migrations are forward-only and
only `0001` exists at v0.1.0, so no downgrade path has been exercised.

## 6. What the console needs from each system

| System | Needs | Console reads |
|---|---|---|
| Switchyard | container running as the compose service the adapter expects, HTTP on the configured URL; `docker` on PATH | `/health`, `/v1/models`, `/v1/stats`, `/metrics`, `routing.jsonl` via `docker exec ... tail`, `routes.toml` |
| Relay | user unit `nemo-relay-gateway.service`, `systemctl` and `nemo-relay` on PATH, gateway on the configured URL | `systemctl --user show`, the gateway's own `GET /health` (any HTTP answer under 500 means up; 404 is expected), LiteLLM's `GET /health/liveliness` for the upstream, `plugins list --json`, ATOF directory listing and tail, `plugins.toml`, `config.toml`, `routes.toml` |
| Dynamo | compose project with `dynamo-frontend`, `dynamo-worker` (and optional `--profile mock` mocker); `docker` on PATH | frontend `/health`, `/v1/models`, `/metrics`; worker metrics read through `docker exec` (the worker port is not reachable from the host under rootless Docker); `docker logs` |
| SkillEvaluator | `skillevaluator-local` (a wrapper that sets the provider and reads its own key file) or `skillevaluator` on PATH (`~/.local/bin` is searched); with plain `skillevaluator`, `SKILL_EVAL_LLM_PROVIDER`, `SKILL_EVAL_LLM_BASE_URL`, `SKILL_EVAL_LLM_API_KEY`, `SKILL_EVAL_LLM_MODEL` and, for Tier 2, `SKILL_EVAL_EMBEDDING_MODEL` in the console's environment (e.g. the systemd EnvironmentFile, 0600); skills under `<repo>/.claude/skills/<name>/SKILL.md`. **Tier 3** needs `skillevaluator-local` and the lab's runner (image, `ai-lab-skilleval` network, broker and egress containers: `clients/skillevaluator/runner/launch.py check`) | `--version` (cached), the LiteLLM URL's `/health/liveliness`, the skills directory listing, its own run directories under `<state>/skillevaluator/runs/` (for Tier 3: `<skill>/<run>/result.json`, read no-follow and size-capped) |
| OpenShell | the `openshell` CLI on PATH (`~/.local/bin`) with the lab gateway registered as `openshell`; the runner's gateway and forwarder containers (started on demand by `data-designer-local`, or the console's `gateway_up`); `docker` on PATH | `openshell --version`, `openshell -g openshell status`, `sandbox list/get -o json`, `logs`, `doctor check`, `provider list`, `policy prove`; `docker inspect` of the two containers; `docker network inspect openshell-docker`; the runner's `gateway.toml`, `policy.yaml`, provider profile and compose file (read-only) |
| DCGM | dcgm-exporter on the configured URL (lab: `cd <repo>/observability && docker compose --profile gpu up -d dcgm-exporter`), Prometheus on the configured URL for the other hosts, the full DCGM image pulled once for diagnostics (`docker pull` the `dcgm_diag_image` setting — the console never pulls); `docker` on PATH | `/metrics`, `/debug/pprof/` (must not answer 200), Prometheus `/api/v1/query` (`up` and `DCGM_*` for `dcgm_prometheus_jobs`), `docker inspect` / `logs` of the exporter, `observability/dcgm/counters.csv` |
| AIPerf | `clients/aiperf/install.sh` then `clients/aiperf/mint-key.sh` (puts `aiperf-local` on PATH, key `~/.config/aiperf/litellm.key`) | `aiperf-local --version`, the LiteLLM URL's `/health/liveliness`, `clients/aiperf/profiles.yaml`, its own run directories under `<state>/aiperf/runs/` |
| NeMo Guardrails | `security/nemo-guardrails/mint-key.sh`, `install.sh` (builds the image, puts `guardrails-local` on PATH), then `docker compose up -d` in that directory; server reachable on the configured URL (`127.0.0.1:8806`) | `/v1/health`, `/v1/rails/configs`, `docker inspect` of the server, gateway and broker containers, every file under `security/nemo-guardrails/config/<id>/` |
| NeMo Evaluator | `clients/nemo-evaluator/install.sh`, `mint-key.sh`, then `evaluator-local prefetch <bench...>` once (the only step with internet; datasets cached under `~/.local/share/nemo-evaluator`) | `evaluator-local --version`, `suites`, `cached --json`; `clients/nemo-evaluator/suites.yaml`; its own run directories under `<state>/evaluator/runs/` (`summary.json` only) |

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
| Dynamo `refused: exclusive engine(s) on the worker's card: ...` (`dynamo.up.exclusive_engine`) | The default model (or another exclusive engine) holds the card. Dynamo is opt-in. | Run the fix, `DYNAMO_START` (default `scripts/use-model.sh qwen3.8-27b-dynamo`), from the repo root: it stops the default model in the right order. The console never stops it, and `stop_sharing_engine` does not apply. |
| Dynamo `refused: the worker's card is in use by ...` (`dynamo.up.card_in_use`) | The opt-in 27B runs on the card, or a container the console does not know (not in any `DYNAMO_*` list). | For the 27B: re-run `up` with "stop" ticked (stops that one label-verified container). For an unknown container: identify it first (`docker inspect`); stop it with its own tooling, or add it to `DYNAMO_COEXISTING_ENGINES` if it fits beside the worker. |
| Dynamo `refused: the CPU mocker is running` / `refused: the real worker is running (same model name as the mocker)` | Mocker and worker cannot coexist. | Run `down` first. |
| Dynamo `cannot determine the worker's GPU UUID(s) from the compose files in use or the container` (`dynamo.preflight.failed`) | Compose path wrong (`REPO_ROOT`), `docker compose ... config` failed, or the worker has no device pin. A variant stack never falls back to the base file's card. | Fix `REPO_ROOT`; run the files' `docker compose ... config` by hand; check the compose pins UUIDs. |
| Dynamo `refused: ...` with `dynamo.compose.untrusted` | The running containers' compose label names a file outside the configured compose directory (also through a symlink), a missing file, another project directory, or the containers disagree. Nothing ran; there is no fallback to the base file. | `docker inspect --format '{{index .Config.Labels "com.docker.compose.project.config_files"}}' ai-lab-dynamo-dynamo-worker-1`; take that stack down with the files it was started from, or set `REPO_ROOT` to the checkout it came from. |
| Params apply / override apply `refused: the live Dynamo stack runs from docker-compose.yml, docker-compose.super-vl.yml ...` | A repo override file is in use (`dynamo.compose.override`, info); the console override restates the base worker command and would replace that stack's worker. | Change the variant's compose file in git, or `down` it first. |
| `up_mock` refused (`dynamo.mock.override_in_use`) | The mocker belongs to the base compose. | `down` first. |
| `dynamo.up.unhealthy` | Worker recreated but `/v1/models` stayed empty for 600 s. | `docker logs --tail 80 <worker>`; check VRAM. |

### Playground

| You see | Cause | Fix |
|---|---|---|
| 503 `credential_missing` "env var <NAME> is not set in the console process" | The named variable is not in the console's environment. | Export it before starting (or `Environment=` in the unit); Settings shows `[unset]`. |
| 502 `upstream_unreachable` "could not reach the system: <ExceptionName>" | System down, wrong URL, or stopped engine. | Check that system's tab. |
| 502 `upstream_error` "system answered 401: ..." | The credential is wrong (Switchyard and Relay both default to `LITELLM_API_KEY`). | Point `PLAYGROUND_KEY_ENV` (per system) / Settings at a variable that holds a valid key. |
| 502 `upstream_error` "system answered 4xx/5xx" | The system rejected the request (e.g. a model alias whose engine is stopped, a "capable" tier that is dark). | Choose a `direct-*` route or start the engine. |
| Playground/event stream fails with an HTTP error | The frontend now parses the `{error:{code,message}}` envelope (`parseErrorBody` in `frontend/js/sse.js`); 4xx is final, no auto-retry (the UI offers Retry). Not exercised against every error in a browser. | If the message is unclear, reproduce with `curl` to read the body. |
| 429 `too_many_streams` "at most 8 concurrent event streams" | Too many open event panels/tabs. | Close tabs; idle streams end after 5 minutes. |
| "Served by" badges empty | The system did not send a recognised served-by header (real Switchyard did not). Not an error. | None. |

### Status problems (shown on the system tabs; each has an exact fix)

| Code | Meaning / first move |
|---|---|
| `switchyard.down` | `/health` did not answer. Run the shown `docker compose up -d --no-build`. |
| `switchyard.config.drift` | `routes.toml` differs from the running server's routes; recreate the container (shown command). |
| `switchyard.tier.capable_not_deployed` (info) | The capable-tier model of a `stage_router` route maps to a container that does not exist on this host. Switchyard stays `up`. The fix names both options: point the route at a deployed engine, or deploy that one (its start command says what it stops). |
| `switchyard.tier.capable_dark` | A container listed in `ENGINE_PROBES` exists but is not running and its alias is the capable-tier model of a `stage_router` route (the message names the routes). Switchyard turns `degraded` with a summary starting "capable tier dark". The fix is the container's `ENGINE_START` command, else the compose directory to `up`. It shares one cached `docker inspect` probe with the topology view; a probe error or an unparseable `routes.toml` yields no problem. |
| `switchyard.tier.capable_broken` | Only with `CAPABLE_PROBE=true`: the alias's container runs, but a 1-token completion through the router failed twice (timeout, router error, or no answer). The message gives the reason (status only); the fix is the container's logs, then its start command. |
| `switchyard.tier.probe_unconfigured` / `switchyard.tier.probe_denied` (info) | The probe is on but the env var named by `CAPABLE_PROBE_KEY_ENV` is not set in the console process, or the router refused the key (401/403). Nothing is reported broken until the probe can run. |
| `switchyard.capable_tier.upstream_errors` | Inferred from `/v1/stats` that the capable-tier engine is failing. `calls` there counts successes (calls + errors = attempts), so it fires when errors >= successes (errors > 0) or errors rose since the last poll with no new success; the message reads "N errors, M successes". **Heuristic:** no active probe, so it appears only after traffic hit the failing tier and never for an idle one. |
| `switchyard.routing.fallback_unavailable` | Requests were re-routed because a target was unavailable. |
| `relay.gateway.down` | Gateway URL does not answer; run `systemctl --user restart nemo-relay-gateway.service`. A 404 from `/health` is **up**, not a failure. |
| `relay.gateway.error` | The gateway answered its own `/health` with a 5xx; restart it. |
| `relay.gateway.upstream_error` | The gateway is up but its upstream LiteLLM does not answer `/health/liveliness`, so requests through it will fail. The console never polls `/v1/models` through the gateway: LiteLLM logs every keyless call to Langfuse as an error. |
| `relay.plugin.attestation_weak` / `relay.exporters.off` / `relay.pii_redaction.off` / `relay.plugin.none_configured` | Policy findings from `plugins.toml`. Human decisions; the console will not change them. |
| `relay.config.newer_than_gateway` | `plugins.toml` changed after the gateway started; restart the gateway. |
| `relay.atof.large` | ATOF directory over 1 GiB; Relay has no retention. Prune rotated files by hand. |
| `dynamo.off` (info, health `off`) | Dynamo is opt-in and no container of it is running or restarting. Normal. To start it, run the fix (`DYNAMO_START`). |
| `dynamo.frontend.down` / `dynamo.worker.absent` / `dynamo.worker.stopped` | Some Dynamo container runs or restarts but not all (or `DYNAMO_OPT_IN=false`); `dynamo.frontend.down` also covers a variant's real frontend (`ai-lab-dynamo-dynamo-sv-frontend-1`) behind its gateway. The start fix is `DYNAMO_START` on the base stack and the console's `up` for a variant; `dynamo.worker.stopped` says "restarting (crash loop)" for a looping worker. |
| `dynamo.compose.override` (info) | The stack runs from repo override files (named); up/down/restart use exactly those files, Params and the console override are refused. Normal for the lab's Super VL. |
| `dynamo.compose.untrusted` | See the action row above: the compose label failed the checks; lifecycle actions and Params refuse. |
| `dynamo.card.contended` | The worker runs or restarts while an exclusive engine runs on one of its cards: they cannot share VRAM (on 2026-09-28 the worker crash-looped beside the default model). Fix: the console `down`; to run Dynamo instead, stop that engine first. |
| `dynamo.holds_card` (info) | Dynamo is up and no exclusive engine runs, so the `DYNAMO_DARK_WHILE_HELD` aliases and routes cannot answer. Information, not a fault; the fix only names how to switch away (`DEFAULT_MODEL_START`). |
| `dynamo.card.shared` / `dynamo.card.unknown` (preflight) | An exclusive engine, or a container the console does not know, runs on the worker's card; `up` is refused until it stops. |
| `dynamo.structural_tag.off`, `dynamo.parser.dialect`, `dynamo.router.kv` | Worker/frontend flags likely wrong for the base compose's model; use the Params tab. Not raised for a variant stack (its flags live in its own compose file). |
| `skillevaluator.live.busy` | A Tier 3 run is in progress; one runs at a time. Watch it in the runs list; `report` with its id shows its progress. |
| `skillevaluator.live.needs_wrapper` | Tier 3 was asked for with plain `skillevaluator`. Install the lab wrapper (`clients/skillevaluator/install.sh`). |
| Tier 3 run `error` "launch: NOT READY: ..." in its report | The runner is not ready (image, network, broker or egress missing). `clients/skillevaluator/runner/launch.py check` in the lab repo says which. |
| Tier 3 run `interrupted` | The console stopped (the launcher gets SIGINT; the lab launcher then stops its job container: read from its code, unverified live) or restarted while it ran. Start it again; the report shows a result.json that landed later anyway. After a hard kill of the console, check `docker ps --filter label=ai-lab.skilleval.job`. |
| `skillevaluator.tier2.no_embeddings` | Tier 2 could not get embeddings (the lab serves `nomic-embed` through the router from Ollama). Fix: `cd <repo>/inference/ollama && docker compose up -d`. |
| `console.adapter.error` / health `unknown` "status check failed" | An adapter raised. Check the console log (JSON, exception class only) and fix that system's paths (`REPO_ROOT`). |

### The console itself

| Symptom | Cause / fix |
|---|---|
| All three systems `unknown` or "absent", paths look wrong | Look for the status problem `console.repo_root.missing`: `REPO_ROOT` does not contain the expected compose files. Set `NEMO_CONSOLE_REPO_ROOT` (section 2). |
| Docker-based checks fail under systemd but work in a shell | The runner passes only `PATH HOME XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS DOCKER_HOST DOCKER_CONTEXT DOCKER_CONFIG LANG LC_ALL`. Add `DOCKER_HOST` / `DOCKER_CONTEXT` to the unit. Unverified under systemd. |
| `binary 'nemo-relay' not found on PATH` in a problem text | Binary outside `PATH`; add its directory to `EXTRA_BIN_DIRS`. |
| History charts empty after a restart | The 15 minute ring is in memory; 1h/24h use rollups written once a minute. Wait a minute. |
| Topology shows an engine you never run | The engine nodes come from `TOPOLOGY_ENGINES` (default: the author's engines). A missing container is drawn `off` "(not deployed)"; only an always-on (`opt_in: false`) engine that exists and is stopped is `down`. Set the list for your deployment. |
| A page shows "Something went wrong: The requested module ... does not provide an export named ..." right after an upgrade | The browser kept console files from before 0.3.1, which were served without `Cache-Control`, and mixed them with new ones. Reload once with Ctrl+Shift+R. Since 0.3.1 every frontend file is revalidated on each load (`no-cache`, ETag), so this cannot recur. |
| Live smoke tests skipped | Gating differs per adapter: Switchyard `NEMO_LIVE=1`, Relay `NEMO_CONSOLE_LIVE=1`, Dynamo automatic when `:8020/health` answers. |

## 8. Verification you can repeat

```bash
make all                                   # offline gates, including the browser e2e suite (needs Chrome)
make e2e                                   # only the browser suite: every page and tab, real backend, headless Chrome
cd backend && NEMO_LIVE=1 NEMO_CONSOLE_LIVE=1 uv run pytest -q -rs   # read-only live smoke, needs the real stack
```

The live tests never run a disruptive action. Actions that change state (recreate, restart, up/down) have been
run through the console only for the items marked verified-live in FEATURE-MATRIX.
