# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.4.0 — 2026-10-28

The backend is rewritten in Rust. Nothing changes for the browser, the API, your settings or your state.

- **One binary instead of a Python venv:** `make setup` (needs `rustup`; the toolchain is pinned) builds it, `make run`
  serves on 127.0.0.1:8793 as before, `make release` writes `backend/target/release/nemo-console`. The API, the
  `NEMO_CONSOLE_*` settings and `~/.config/nemo-console/env`, the state directory and its database, the `open` /
  `stop` / `mint-key` commands, the log lines and the exit codes are the same: a 0.3 state directory is used as is.
- **How it was checked:** every Python test was ported (936 of 936 mapped), and a differential harness runs 1,393
  cases against the last Python backend and this one side by side, with fakes for every CLI and upstream: all equal but
  one documented deviation. The Rust backend has **not** been run against the real Switchyard, Relay, Dynamo or the
  other systems yet; the "verified live" claims in the docs were observed with the Python backend (FEATURE-MATRIX).
- **Hardening before release:** roles are enforced by the request pipeline for every route, slow or idle clients and
  connection floods are cut off (5 s for a request head, 30 s for a body, 256 connections), background-task panics
  are contained, redaction fails closed, and run-file reads refuse symlinks (CHANGELOG; `backend/PORTING.md` A-9).
- **Deliberate differences** are few and listed in the CHANGELOG and `backend/PORTING.md` §13 (fixed parser limits
  instead of stack-dependent ones, offset-less timestamps read as UTC, a handful of edge cases); some Python bugs are
  kept on purpose for now (CHANGELOG, "Known Python bugs kept").

Upgrade: `git pull`, `make setup`, stop the old console (`nemo-console stop` with the old binary, or the new one: either
stops the other), start it again with `make run`, or re-run `make install-desktop` for the menu entry. If you use the
systemd template, point `ExecStart` at `backend/target/release/nemo-console` (`deploy/nemo-console.service`). The old
`backend/.venv` can be deleted.
