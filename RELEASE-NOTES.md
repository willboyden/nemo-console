# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.6.1 — 2026-11-04

A one-line fix to the navigation.

- **Fixed:** the sidebar showed the "Engines and load" section label twice, because the Model Optimizer entry was
  listed after the evaluation section (since 0.5.0). Model Optimizer now sits under Engines and load, next to AIPerf.
  A new frontend test requires each nav section to be one consecutive run of entries, so a section cannot be split
  again.
- **Unchanged:** the backend's behaviour (only its version number moved) and everything 0.6.0 added: the Getting
  started and FAQ tabs on every system page.

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
