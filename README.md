# Nemo Console

A local, single-user web console to **monitor, configure and interact with** eleven NVIDIA-ecosystem systems
running on one workstation:

- **NeMo Switchyard**: tier-based request routing (an HTTP server in a container).
- **NeMo Relay**: an agent gateway with plugin policy and ATOF/ATIF trace export (a host binary, run as a
  systemd user unit).
- **NVIDIA Dynamo**: an LLM serving frontend plus a vLLM worker (docker compose).
- **NVIDIA SkillEvaluator** (new in 0.2.0): scores agent skills (`SKILL.md` folders) for quality, security and
  duplication. A CLI, not a service: the console runs it on demand and keeps the reports.
- **NVIDIA Data Designer** (new in 0.3.0): generates synthetic datasets from config scripts through the lab router
  (validate → preview → create, record review, parquet/csv/jsonl/json copies under `~/Datasets`). Also a CLI, not a
  service, driven the same way (install: the lab's `clients/data-designer/install.sh`).
- **NVIDIA OpenShell** (new in 0.3.0): the sandbox gateway that contains Data Designer runs. Status, sandbox list,
  logs and policy proofs; its trust files are shown read-only.
- **NVIDIA DCGM** (new in 0.3.0): GPU telemetry from this host's dcgm-exporter plus every DCGM host the lab's
  Prometheus scrapes; per-GPU diagnostics in a throwaway container; the exporter's field list is editable with
  rollback. Flags an exporter older than 4.8.3 (CVE-2026-47483).
- **NVIDIA AIPerf** (new in 0.3.0): load tests against router models from named presets, one run at a time, with
  TTFT/ITL/throughput from each run.
- **NVIDIA Model Optimizer** (new in 0.5.0): offline compression of a pinned checkpoint (quantization,
  REAP expert pruning or Minitron). A CLI, not a service: the console starts a job in the
  background and then reads its verdict, its output size and its compression ratio.
- **NVIDIA NeMo Guardrails** (new in 0.3.0): a rails server (input and output self-checks) in an internal-network
  container; rails configs editable with validation and rollback; probe checks and a playground through the rails.
- **NVIDIA NeMo Evaluator** (new in 0.3.0): offline benchmark suites (GSM8K, MMLU, ...) against router models, scores
  per benchmark and model.

It is a single host-run binary (Rust, since 0.4.0; the Python backend before it is the behavioural reference) bound
to loopback, with a zero-build, zero-npm browser UI. One adapter per
system plugs into a system-agnostic core; the UI renders from descriptors (status, metrics, config files,
parameters, actions, playground), so adding a system is one adapter file (SkillEvaluator was added that way).

> **Status: v0.4.0** (the backend rewritten in Rust; [`CHANGELOG.md`](CHANGELOG.md)). Read the [status vocabulary](#status-vocabulary) before trusting any claim in these docs.
> Nothing here has been used by anyone other than its author, on one workstation, against one set of pinned
> versions (Switchyard 0.3.0, Relay 0.9.2, Dynamo 1.5.0 and a 1.6.0 dev build, SkillEvaluator 0.5.0, Data Designer
> 0.9.3, OpenShell CLI 0.0.71 / gateway 0.0.77, dcgm-exporter 4.8.4 with DCGM 4.6.1, AIPerf 0.13.0, NeMo Guardrails
> 0.24.1, NeMo Evaluator 0.3.0).

## Status vocabulary

Every feature claim in this repo is one of:

| Label | Meaning |
|---|---|
| **implemented** | Read in the code; covered by offline tests using fakes and captured fixtures. Not exercised against the real system. |
| **verified-live** | Exercised against a real Switchyard 0.3.0, Relay 0.9.2 and Dynamo 1.5.0 on one workstation (evidence is stated in [`docs/FEATURE-MATRIX.md`](docs/FEATURE-MATRIX.md)). |
| **unverified** | Only fakes have run it. It may work; nobody has watched it work. |

Gate results (2026-10-28, the 0.4.0 tree): `cargo test --locked` 842 passed / 0 failed (the live smoke tests return
early without `NEMO_LIVE` / `NEMO_CONSOLE_LIVE`), rustfmt and clippy `-D warnings` clean, cargo-deny and cargo-audit
clean, crate-age 265 locked crates with 0 too new and 2 security exceptions, frontend `node --test` 122 passed, browser
suite (headless Chrome 151) 13 routes and 46 pages and tabs with no error state, parity against the last Python backend
1,393 cases with 1,392 equal and the one difference documented (PORTING D-6), all 936 pytest ids mapped to Rust tests,
and the cross-implementation checks (state directory, launcher, startup errors) passing; the clean-clone runs are in
`backend/PORTING.md` A-8 and A-9. Mermaid diagrams in these docs were **not rendered**; treat them
as unvalidated.

## What it does

| Area | What you get |
|---|---|
| Observe | Health and status per system with actionable problems (each with one copy-pasteable fix), live metrics with 15 min / 1 h / 24 h history, a topology view, live event streams (SSE), and a routing summary (by route, model, tier, tokens, errors) derived from Switchyard's routing log. |
| Configure | Switchyard `routes.toml` editor: validate, diff, confirm, backup, atomic write, recreate the container, health check, **automatic rollback** if unhealthy, revision history and manual rollback. Relay `routes.toml` the same way (with a weaker validator, stated in the UI). Dynamo tunables as a structured parameter form that renders a compose override file. |
| Act | Per-system actions with danger levels (safe / disruptive / destructive) and mandatory confirmation. Dynamo up/down/restart with a **preflight** that names what shares the GPU. |
| Interact | A playground that sends one chat request through a system, streamed, never stored. |
| Audit | Append-only record of every mutating call (params redacted, chat text and file bodies never stored). |

## Architecture at a glance

```mermaid
flowchart LR
  B[Browser SPA<br/>no build, no CDN] -->|loopback HTTP + SSE| C[nemo-console (Rust, axum)<br/>127.0.0.1:8793]
  C --> M[Security middleware<br/>Host, Origin, CSRF, auth, caps, headers, audit]
  M --> S[Core services<br/>config store, metrics, SSE, settings]
  S --> A1[Switchyard adapter]
  S --> A2[Relay adapter]
  S --> A3[Dynamo adapter]
  A1 & A2 & A3 -->|argv only| R[CommandRunner<br/>docker, systemctl, nemo-relay]
  A1 & A2 & A3 -->|HTTP, loopback only| H[System HTTP endpoints]
  S --> DB[(SQLite WAL<br/>keys, audit, revisions, rollups, settings)]
```

*(Unrendered Mermaid.)* Details: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (the locked contract),
[`docs/adr/`](docs/adr/) (17 decisions with rejected alternatives), [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md),
[`docs/OPERATIONS.md`](docs/OPERATIONS.md), [`docs/FEATURE-MATRIX.md`](docs/FEATURE-MATRIX.md), [`docs/PUBLISHING-CHECKLIST.md`](docs/PUBLISHING-CHECKLIST.md),
[`CHANGELOG.md`](CHANGELOG.md).

## Quick start

Prerequisites: Linux, a Rust toolchain manager (`rustup`; the exact toolchain is pinned in
`backend/rust-toolchain.toml` and installed on first build), Docker (rootless
works and is what the author used), the three systems deployed the way the adapters expect (see
[Deployment assumptions](#deployment-assumptions)), Node 22 and a Chrome or Chromium only if you run the frontend tests
and the browser end-to-end suite.

```bash
make setup          # cargo build --locked (backend/), enable the git hooks
make run            # release build, then serves API + UI on http://127.0.0.1:8793
make all            # rustfmt + clippy + cargo test + cargo-deny + cargo-audit + crate-age + node --test + browser e2e
make install-desktop  # optional (Linux): "Nemo Console" in your app menu; see docs/OPERATIONS.md section 3
```

**A key is required for everything except `GET /api/v1/health`.** First start writes an admin API key to
`<state dir>/bootstrap-admin.key` (mode 0600) and logs its path. Open the UI: it prompts for a bearer; paste the key
(kept in the tab's `sessionStorage`). Mint more with `nemo-console mint-key [admin|viewer]`, which prints only the path
of a 0600 key file. Full instructions: [`docs/OPERATIONS.md`](docs/OPERATIONS.md).

UI without a backend (canned data, nothing verified about real systems): `cd frontend && node dev/mock-server.mjs`
(its default port is 18796; the real backend is 8793; pass `--port` to change the mock's).

## Security posture (summary)

To report a vulnerability, see [`SECURITY.md`](SECURITY.md).

Loopback bind; `Host` allowlist (421); same-origin-or-absent `Origin` on unsafe methods (403); `X-Console: 1`
on every non-GET (403); bearer keys stored as SHA-256 with `admin` / `viewer` roles (an unauthenticated caller is
role `public` and gets 401 on everything but `/health`); strict CSP and other
security headers on every response including errors; body and SSE caps; all process control through one argv-only
runner with a bare-name binary allowlist (`docker`, `systemctl`, `nemo-relay`, `openshell`, the two SkillEvaluator
launchers and the lab wrappers `data-designer-local`, `data-designer-export`, `aiperf-local`, `guardrails-local`,
`evaluator-local`); config writes only to paths fixed at startup, refusing symlinks and
secret-looking content; secrets referenced by environment-variable **name**, never stored or returned; no
outbound network beyond configured loopback URLs.

**Known gaps, stated plainly** (full list in [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md)):

- Loopback trust is **off by default**. If you opt in (`NEMO_CONSOLE_TRUST_LOOPBACK=true`) any local process becomes
  `admin` without a key; the console forces it back off when the host is not loopback or `allowed_hosts` is set
  (`/health.trust_notice` says why; it is `null` otherwise). The listen host is still a plain setting and nothing forbids a non-loopback bind.
- The console can start and stop containers and restart a systemd user unit. Compromising it is
  roughly equivalent to compromising the Docker user it runs as.
- Dynamo events are redacted **free-text container log lines**, not allowlisted metadata (see limitations).
- Playground chat text transits the console process in memory and is passed to whichever system you target.

## Known limitations

- **Switchyard "capable tier dark" detection is indirect unless you turn on the active probe.** Two passive signals
  exist, and an opt-in third:
  (a) `switchyard.tier.capable_dark` (setting `engine_probes`, alias to engine container) fires when a mapped alias
  is the `capable_target` model of a `stage_router` route in the parsed `routes.toml` (a passthrough such as
  `direct-*` does not count) and the mapped container is *not running* per `docker inspect`; it makes Switchyard
  `degraded` with a summary prefixed "capable tier dark". Its fix is the container's `engine_start` command when one
  is set, else a guessed `docker compose up -d` in the compose directory; a mapped container that does not exist at
  all is only the info problem `switchyard.tier.capable_not_deployed`. (b) `switchyard.capable_tier.upstream_errors`
  is a traffic heuristic on `/v1/stats` error counts. Neither notices a running-but-broken engine, and (a) only knows
  the containers you list in `engine_probes` (the defaults list two aliases and containers from the author's
  deployment). (c) With `NEMO_CONSOLE_CAPABLE_PROBE=true` (off by default) the console sends one 1-token completion per
  running capable-tier alias through the router, at most every 5 minutes, and raises
  `switchyard.tier.capable_broken` when it fails twice; each probe is a real router request, billed to the key named
  by `capable_probe_key_env` (see [`docs/OPERATIONS.md`](docs/OPERATIONS.md)).
- **Playground "served by" headers are guessed.** The console looks for `X-Served-By-Route` style headers
  (several candidate names); they were absent from real Switchyard responses, so the route/tier badges are
  usually empty.
- **Topology edges are approximations** built from adapter health plus probes (router liveness, the engine
  containers of the `topology_engines` setting; opt-in engines that are stopped are drawn `off`, not red, and a missing
  container is `off` "(not deployed)"). They show "plausible path", not observed traffic. The edge label "agent gateway"
  is partly clipped behind a node in the rendered view (cosmetic, known).
- **`plugins.toml` and `config.toml` (Relay) are read-only by design.** They hold signature-enforcement policy;
  the console shows them and never writes them.
- **The Dynamo tab has no config-file tab.** The backend exposes the compose file (read-only) and the generated
  override through the config API, but the UI only offers the parameter form and lifecycle.
- **`/health` reports the caller's role** (`admin`, `viewer` or `public`) and a `trust_notice`; with the default
  settings an unauthenticated caller sees `public` and a null notice.
- **Dynamo events are redacted log lines**, not an allowlist of metadata fields (ADR-0010 deviation).
- The container, compose-project and unit names, and where each system's files live under the repo root, are settings
  since the unreleased work after 0.3.1, defaulting to the author's deployment (see
  [Deployment assumptions](#deployment-assumptions)). Still fixed in code: the one container Dynamo's `up` may stop
  (`stop_sharing_engine`, deliberately not a setting: a setting must not widen what the console stops), OpenShell's
  sandbox network and router address (they must match the lab wrapper), and the router liveness path.
- **Route validators guard against off-box IPs and dotted hostnames, not against a hostile local resolver.** A bare
  single-label hostname (for example `metadata`) is accepted as an in-network "service name"; a resolver search domain
  could send it off the box. An `allowed_service_names` setting is a possible future option, not implemented.
- **Redaction is bounded and chunked** (4000-character chunks, linear time). A secret straddling a chunk boundary could
  slip through.
- **The systemd unit template is untested.** Hardening options that need user namespaces can fail with status
  `226/NAMESPACE` on distributions that restrict unprivileged user namespaces; treat any such option as optional.
- **Dynamo override gaps:** enum-valued flags are not checked against the catalog's choices and numeric values are not
  type-checked in the hand-edited override (admin-only; the command is an argv list, never a shell).

## Deployment assumptions

The adapters were written against one deployment. Its names and paths are now settings whose defaults are that
deployment (`NEMO_CONSOLE_*`, listed in [`docs/OPERATIONS.md`](docs/OPERATIONS.md) section 2): set the ones that differ
for yours. What they describe:

- Switchyard runs as a docker compose service in a project directory containing `routes.toml` and
  `docker-compose.yml`, with a container name the adapter verifies by compose labels (`SWITCHYARD_CONTAINER`,
  `SWITCHYARD_COMPOSE_PROJECT`, `SWITCHYARD_COMPOSE_SERVICE`; directory `LAYOUT__SWITCHYARD`).
- Relay's gateway runs as a systemd user unit (`RELAY_UNIT`, default `nemo-relay-gateway.service`) on `127.0.0.1:4040`.
- Dynamo runs as a compose project (`DYNAMO_COMPOSE_PROJECT`) with frontend / worker services (`DYNAMO_SERVICES`,
  `DYNAMO_CONTAINERS`), pinned to a GPU by UUID.
- A LiteLLM-compatible router answers `/health/liveliness` on `127.0.0.1:4000` (topology probe only).
- OpenShell: the lab's Data Designer runner (`clients/data-designer/runner`: gateway `ai-lab-dd-gateway`, forwarder
  `ai-lab-dd-cli-fwd`, sandbox network `openshell-docker`). DCGM: dcgm-exporter on `127.0.0.1:9400` (container
  `ai-lab-observability-dcgm-exporter-1`, fields in `observability/dcgm/counters.csv`) and Prometheus on
  `127.0.0.1:9090`. AIPerf / NeMo Evaluator / NeMo Guardrails: the lab wrappers `aiperf-local`, `evaluator-local`,
  `guardrails-local` on PATH (each reads its own 0600 router key file) and, for Guardrails, the server on
  `127.0.0.1:8806` (`security/nemo-guardrails`). Container names are settings (`NEMO_CONSOLE_*`).
- The root that holds those directories is `NEMO_CONSOLE_REPO_ROOT`; each system's directory under it is a `LAYOUT__*`
  setting (defaults `inference/switchyard`, `clients/nemo-relay`, `inference/dynamo`, ...). The default root walks up from
  the executable to a directory containing `inference/dynamo/docker-compose.yml`, else uses the current directory; a
  root that lacks the Dynamo compose file of the layout is reported as a problem. For a standalone checkout set it
  explicitly.

## Repository layout

```
backend/                    Cargo workspace (Rust; Cargo.lock, rust-toolchain.toml, deny.toml)
  crates/nemo-console/src/
    adapters/   base.rs (locked port), one module per system (switchyard, relay, dynamo, skillevaluator, ...)
    api/        mod.rs (router + security middleware), observe / configure / act / admin handlers, bodies
    services/   runner, config_store, metrics, events (SSE), playground, auth, audit, settings, topology,
                pyyaml / pytoml / pyfmt (Python-exact YAML, TOML and formatting), ...
    domain/     models.rs (locked contract)   migrations/0001_init.sql
  crates/nemo-console/tests/  every ported test (fakes + captured fixtures), goldens, *_live_smoke opt-in
  tools/        crate-age.py (dependency cooldown gate), gen-test-map.mjs
  PORTING.md    the port's contract, deviations from the Python backend, addenda; TEST-MAP.md
frontend/       index.html, js/ (ES modules, web components), css/, i18n/en.json, dev/mock-server.mjs, tests/, e2e/
parity/         differential harness: the last Python backend (parity/ref.mjs) vs this backend, case by case
deploy/         nemo-console.service (opt-in systemd user unit template), nemo-console.desktop
docs/           ARCHITECTURE.md, adr/, THREAT-MODEL.md, OPERATIONS.md, FEATURE-MATRIX.md, PUBLISHING-CHECKLIST.md
```

## Development

| Target | Does |
|---|---|
| `make setup` | `cargo build --locked`, enable the git hooks (`make hooks`) |
| `make run` | release build, then serve on 127.0.0.1:8793 (`make release` builds `backend/target/release/nemo-console`) |
| `make test` / `lint` / `typecheck` | `cargo test`, `cargo fmt --check`, `cargo clippy -D warnings` |
| `make deny` / `audit` / `crate-age` | cargo-deny, cargo-audit, the 14-day dependency cooldown (security fixes excepted) |
| `make frontend-test` | `node --test` on the pure-logic modules |
| `make e2e` | builds, then every page and tab in headless Chrome against the real backend; records a pass for the pre-push hook |
| `make parity` | the differential harness against the last Python backend (needs `uv` and its cache; see `parity/README.md`) |
| `make all` | every gate except parity |

Pushing is gated: the `pre-push` hook (`.githooks/pre-push`) refuses a commit or tag that `make e2e` has not passed
on (exact `backend/` and `frontend/` trees; docs-only commits after a pass need no rerun). If your Chrome is not on
PATH, name it once: `git config nemo-console.chrome /path/to/chrome`. CI runs the same suite (job `e2e`).

Live smoke tests are read-only against real systems and need the deployment described above. They never run by
default: Switchyard and Dynamo need `NEMO_LIVE=1`, Relay `NEMO_CONSOLE_LIVE=1`.

## License and trademarks

MIT; see [`LICENSE`](LICENSE).

This is an independent project. It is not affiliated with, endorsed by or sponsored by NVIDIA. "NVIDIA", "NeMo",
"NeMo Switchyard", "NeMo Relay" and "Dynamo" are names of products of their respective owners and are used here
only to describe compatibility.
