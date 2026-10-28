# Parity harness: the last Python backend vs the Rust backend

Runs the last Python backend (the reference) and another backend side by side with identical hermetic setups, sends
both the same requests in the same order, and compares what comes back. Node 22, no npm dependencies.

Since 0.4.0 the checkout holds only the Rust backend. The reference is a git revision of this repository:
`python@<tag or commit>` (plain `python` = `DEFAULT_REF` in `ref.mjs`, main at `aa2f810`, the code the port was
checked against). `ref.mjs` extracts that revision's `backend/` with `git archive` into `parity/.ref/<rev>/` (gitignored)
and builds its venv once with `uv sync --frozen`, OFFLINE from the uv cache (`PARITY_UV_ONLINE=1` allows PyPI on a new
machine). Every check below takes `--python python@<rev>` (run.mjs: `--a`/`--b`). Each backend reports its own
release in `version` fields; the normaliser maps both to `<version>`.

```sh
node parity/run.mjs --allow-diff-routes parity/allow-diffs.txt     # = --a python --b rust (make parity)
node parity/run.mjs --a python@v0.3.1 --b rust               # against an older Python release (expect its diffs)
node parity/run.mjs --a python --b python                     # the harness checks itself: must be 0 diffs
node parity/run.mjs --a rust --b rust [--rust-bin PATH]       # default binary: backend/target/debug/nemo-console
node parity/ref.mjs [rev]                                     # prepare a reference; prints its backend dir (for parity/gen)
node parity/check-routes.mjs                              # every Python route has corpus cases (+ Rust route table)
node parity/check-test-map.mjs [--allow-unmapped]         # backend/TEST-MAP.md vs pytest-ids.txt
node --test parity/tests/lib.test.mjs                     # self-tests of the parser, comparer, normaliser, fakes
node parity/xstate.mjs                                    # each backend reads the console.db the other wrote
node parity/launcher.mjs                                  # `open` / `stop` across implementations (cross stops too)
node parity/startup.mjs                                   # bad settings / taken port: exit codes and messages
```

`run.mjs` options: `--scenario NAME` (repeatable; default all), `--only REGEX` (repeatable; case id or route),
`--filter SUBSTRING` (case id or route; cases that
need a value captured by a filtered-out case are skipped), `--allow-diff-routes FILE` (one `METHOD /template` or one
case id per line, or `stub:<system_id>`, `#` comments: diffs there are reported but do not fail the run; since 0.4.0
`allow-diffs.txt` holds only the deviations recorded in backend/PORTING.md §13), `--dump` (writes `out/dump-<scenario>.json`: A's normalised answer, argv and upstream calls per case),
`--keep` (keep the temp dirs), `--tmp DIR`, `--list`. Output: a summary grouped by route, and `out/report.json`
(gitignored). Exit 0 only when no case differs (outside the allowed routes). `PARITY_PROGRESS=1` prints a dot per case,
`PARITY_SERVER_LOGS=1` saves both servers' logs in `out/`. Caveat: `--filter` / `--only` on the `lifecycle` and
`probe-broken` scenarios still compares the WHOLE-RUN upstream set (background tasks call between cases), so a filtered
run can show a `global:<scenario>` diff a full run does not; judge those scenarios on full runs.

## Scenarios

| name | repo + HOME | fake CLIs on PATH | upstreams | cases |
|---|---|---|---|---|
| `full` | seeded from `backend/crates/nemo-console/tests/fixtures/` (the Python tests' captured outputs) | yes | fake servers | ~1,000: every route, every action of every adapter, config round trips, stateful flows, the role sweep |
| `empty` | empty (like the e2e suite) | none (PATH is an empty dir) | ports nothing listens on | ~345: the not-installed / down paths |
| `loopback` | empty | none | refused | 15: `NEMO_CONSOLE_TRUST_LOOPBACK=true` (no-key admin, proxy headers turn it off) |
| `lifecycle` | seeded | yes | fake servers | 10: background tasks ON (poller every 0.5 s: ring + 1h/24h rollups), the active capable probe (its completion request is compared), SSE heartbeat + idle timeout (1.5 s), SIGTERM with a stream open (`event: shutdown`, then the exit status: killed by SIGTERM) |
| `probe-broken` | seeded | yes | fake servers, router unreachable | 10: the same with the probe failing twice (`switchyard.tier.capable_broken`, Switchyard `degraded`) |

Per-case shaping flags (each a documented normalisation): `collapseKeepAlive` (consecutive `: keep-alive` comments
count as one: their number depends on scheduling jitter), `historyShape` (a metrics-history answer is compared as
`{nonEmpty, values}`: tick counts and timestamps are wall-clock), `waitExit` (the backend's exit status after the case
is compared). Scenarios with background tasks set `wholeRunCallsOnly`: calls happen between cases, so only the
whole-run distinct upstream-request set is compared (the CLI set too depends on adapter-internal caches there).
Whole-run sets are built from the complete logs, so calls made between cases count.

Each backend gets its own temp dir: `home/` (HOME and every XDG dir), `state/`, `repo/` (NEMO_CONSOLE_REPO_ROOT),
`bin/` (the only PATH entry), its own fake upstream servers and its own fake machine state. Both use the worktree's
`frontend/` (same ETags), `LANG=C.UTF-8`, `NEMO_CONSOLE_NO_USER_ENV=1`, background tasks off, status cache off
(`NEMO_CONSOLE_STATUS_TTL_S=0`), and a dummy `LITELLM_API_KEY`. Admin key: `<state>/bootstrap-admin.key`; viewer key:
`<backend> mint-key viewer` with the same env (its output is compared too). Readiness is a TCP connect, so no probe
request lands in the console's own `/metrics`. Ports are never handed out twice; the backends are started one after
the other and stopped (SIGTERM, then SIGKILL) when the scenario ends.

## What is compared, per case

- status; the headers in `CHOSEN_HEADERS` of `run.mjs` (content-type, cache-control, etag, last-modified, CSP and the
  other security headers, x-served-by-route/-tier, x-accel-buffering, allow, ...);
- the body: JSON as values with a parser that keeps int vs float (`lib/json.mjs`: key order ignored, list order kept,
  null vs absent distinct, `1` never equals `1.0`); SSE as a list of events (comments included, JSON data parsed);
  Prometheus text as a sorted multiset of lines; static files byte for byte; other text as text;
- the fake-CLI calls made during the case: argv, cwd, stdin and the environment the runner passed (as a set; the
  read-only probes the adapters cache with a TTL are compared over the whole run instead, see `CACHED_ARGV`);
- the requests sent to the fake upstreams: method, path, decoded query, the headers the console sets, JSON body.

Each case is sent to both backends at the same moment and the next case starts when both answered, so wall-clock
caches age alike. Every difference is reported with a JSON pointer (or a named field) and both values.
**Normalisation**: every rule (temp paths, ports, generated ids, timestamps, durations, ...) is listed with its reason
in `RULES` of [`corpus/normalise.mjs`](corpus/normalise.mjs); nothing else is normalised.

## Layout

- `run.mjs` runner; `lib/` servers, seeding, raw HTTP client + SSE parser, JSON parser, comparer
- `corpus/index.mjs` scenarios and the ordered case list; `corpus/core.mjs` security / static / global routes;
  `corpus/systems.mjs` per-system cases; `corpus/normalise.mjs` the normalisation rules
- `fakes/upstreams.mjs` fake HTTP services (litellm, switchyard, relay, dynamo, dcgm, prometheus, guardrails)
- `fakes/cli.mjs` + `fakes/cli-table.mjs` fake CLIs for every allowlisted binary; `bin/<name>` shims are generated
  per backend (`#!/bin/sh` + `exec <node> fakes/cli.mjs <sideDir> <name> "$@"`); every call is appended to
  `<sideDir>/cli.jsonl`; an argv the table does not know exits 1 and is listed under `unknownCli` in the report
- the fixtures are the Rust tests' own (`backend/crates/nemo-console/tests/fixtures/`, copies of the Python tests' fixtures); the harness keeps no second copy
- `routes.python.json` + `tools/dump_routes.py`, `pytest-ids.txt`, `check-routes.mjs`, `check-test-map.mjs`

## Adding a case

Append an object to the right list in `corpus/` (order matters: state carries over from case to case). Fields
(documented at the top of `corpus/index.mjs`): `id`, `route` (`"METHOD /template"` from `routes.python.json`),
`method`, `path` (sent raw), `auth` (`admin` default, `viewer`, `none`, `bad`, `raw:<header>`), `host`, `csrf: false`,
`headers`, `json` or `body`+`contentType`, `chunked`, `readMs` (SSE), `capture` (`{var: '/json/pointer'}` or
`{var: {header}}`, used later as `{{var}}`, per backend; a var one backend failed to capture falls back to the other
backend's value and the case reports it), `during`, `after` (e.g. `waitRuns('aiperf')` for background jobs),
`concurrent` (several requests in flight), `pauseMs`, `kind: 'cli'` + `args`. Then run `--a python --b python`: it
must stay at 0 diffs. A new fake command goes into `fakes/cli-table.mjs` (deterministic output from the argv, the
seeded files and the fake machine state `fake-world.json`); a new upstream path into `fakes/upstreams.mjs`.
