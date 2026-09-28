# ADR-0003: All process control through one `CommandRunner`

Status: Accepted (v0.1.0)

## Context

The console executes commands whose arguments partly derive from user input (file ids, action parameters,
container names). Shell injection or an unexpected binary would be the most direct route from the web API to
arbitrary code execution as the operator.

## Decision

Adapters receive a `CommandRunner` (`adapters/base.py` protocol, `services/runner.py` implementation) and may not
import `subprocess`. The runner:

- takes an argv list (no shell), rejects empty argv, more than 64 arguments, NUL bytes, and arguments over 8192
  characters;
- allows only the bare names `docker`, `systemctl`, `nemo-relay` (so `/bin/sh` and `../x` are refused) and
  resolves them via `PATH` plus configured extra dirs (`NEMO_CONSOLE_EXTRA_BIN_DIRS`, default `~/.local/bin`);
- passes a minimal child environment (`PATH HOME XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS DOCKER_HOST
  DOCKER_CONTEXT DOCKER_CONFIG LANG LC_ALL`);
- starts the child in its own session, enforces a timeout (capped at 600 s) and an output cap, and kills the
  whole process group on either;
- passes stdin as bytes, never in argv;
- redacts stdout and stderr before returning them.

Adapters additionally validate identifiers with `fullmatch` and verify container identity by compose labels
via `docker inspect` before acting.

## Consequences

- One place to audit for command injection. Tests: `test_runs_allowlisted_binary_with_argv_no_shell`,
  `test_binary_allowlist`, `test_timeout_kills_process`, `test_output_cap_stops_runaway_producer`,
  `test_stdin_bytes_and_env_is_minimal`.
- The allowlist is by binary, not by subcommand. `docker` can do anything the Docker user can do; the
  constraint on *which* docker commands run is in the adapters' fixed literals, not in the runner.
- Two adapters (Switchyard dry-run, Dynamo override validation) write candidate content to a stdin pipe or a
  0600 temp file under the state dir rather than into the live path.
- Secrets in argv are refused by validators (Dynamo override allowlist), but the runner itself cannot know
  what is secret.

## Alternatives considered

- **Docker SDK / systemd D-Bus bindings.** Adds dependencies and a larger attack surface than three CLIs, and
  the CLIs are what the operator already uses (rootless contexts, `--user`).
- **Per-adapter subprocess use with review discipline.** Rejected: discipline does not scale to three
  implementers.
- **Subcommand-level allowlist in the runner.** More secure but brittle across CLI versions; deferred.
