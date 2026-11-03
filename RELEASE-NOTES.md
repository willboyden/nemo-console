# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.5.0 — 2026-11-03

Two more NVIDIA systems, the eleventh and twelfth. Both are driven through a lab wrapper on PATH, and both run their
work in the background so a closed tab never stops it.

- **NVIDIA Model Optimizer** (ADR-0017): offline compression of a pinned checkpoint through the `modelopt-local`
  wrapper. The page shows the wrapper, the recipe file and the last job, the `mo.*` metrics, an editor for
  `recipes.yaml` (validated with the wrapper's own schema before the write; a file with any error runs nothing) and a
  `run` action that starts a job in the background (disruptive: it takes a GPU). `report` reads a job's verdict. The
  console reads each job's `run.json`; it never runs the compression code, never writes a job's files and never stops
  a running job.
- **NVIDIA NeMo Gym** (ADR-0018): reinforcement-learning environments and rollouts through the `gym-local` wrapper,
  whose client runs on an internal network with a key-holding router broker as its only way out. The page shows the
  wrapper, the broker, the client network's containment, the image and whether the key file exists, the `gym.*`
  metrics, and a `smoke` action (disruptive, one at a time, 1-20 tasks) that runs one bounded rollout in the
  background. `verify` and `report` are safe. A client network that is not internal makes the system down and blocks
  a run.
- **How it was checked:** offline tests with fakes for both wrappers, the browser suite over every page and tab, and
  the parity harness against the last Python backend (the two new systems are the documented deviation D-31). Live,
  the status pages of both systems were observed against the installed wrappers on the maintainer's workstation
  (2026-11-03). No compression job and no smoke run has been started from the console yet.
- **Also:** `make parity` now runs the same checks as CI, and the dependency cooldown has no exceptions left.

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
Restart after pulling: the frontend is served from disk, and a running 0.4 backend lacks the new systems' routes.
Without `modelopt-local` / `gym-local` on PATH the two pages show the system as down, with the install step.
