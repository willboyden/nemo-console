# ADR-0002: Host-run FastAPI on `127.0.0.1:8793`, not a container

Status: Accepted (v0.1.0); amended by ADR-0016 (v0.4.0: the process is a Rust binary, `backend/target/release/nemo-console`)

## Context

The console must run `docker compose`, `docker inspect/exec/logs` and `systemctl --user` for the operator's own
user, against a rootless Docker daemon and the operator's systemd user manager. The three managed systems
publish on loopback ports.

## Decision

Run the console as a normal process owned by the operator (`nemo-console` entry point, optionally the
`deploy/nemo-console.service` user-unit template), listening on `NEMO_CONSOLE_HOST:NEMO_CONSOLE_PORT`
(default `127.0.0.1:8793`; 8792 was avoided because another local tool used it). Do not containerise it.

## Consequences

- No container ever holds a Docker socket. Mounting the socket into a network-facing service is
  root-equivalent; here the equivalent power lives in a process that listens only on loopback.
- It can reach loopback ports and the user systemd bus directly.
- The console is exactly as privileged as the operator's user. A compromise of the console is a compromise of
  that user's Docker and user-unit control (see THREAT-MODEL, E-1).
- `host` is a plain setting: nothing prevents `NEMO_CONSOLE_HOST=0.0.0.0`. The architecture document says
  "bind 127.0.0.1 only"; the code does not enforce it (it only builds the `Host` allowlist from the configured
  host and port). Operators must not change it without adding their own network controls. What the code does do:
  loopback trust is forced off for a non-loopback host or when `allowed_hosts` is set.
- The default repo root walks up from the source file (0.4: the executable) to a directory containing the Dynamo compose file, else falls
  back to the current directory; a root without it is reported as the status problem `console.repo_root.missing`.
  A standalone checkout should set `NEMO_CONSOLE_REPO_ROOT`.
- Not verified: running under the provided systemd unit (the template sets `NoNewPrivileges` and similar hardening, and namespace-based options could fail with 226/NAMESPACE on restricted distributions;
  it was not started under systemd during this documentation pass).

## Alternatives considered

- **Container with the Docker socket mounted.** Rejected: root-equivalent inside a web service.
- **Container with a socket proxy exposing a verb allowlist.** Adds a second component and still needs
  `systemctl --user` for Relay, which a container cannot reach without more host access.
- **CLI/TUI only.** No live charts or streaming, and no shared safety UI for confirmation.
