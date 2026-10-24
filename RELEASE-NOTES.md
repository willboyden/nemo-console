# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.3.0 — 2026-10-24

Nemo Console 0.3.0 grows from four managed systems to ten. Every new system is one adapter plus a
generic, descriptor-driven page; the sidebar is grouped into Routing, Engines and load, Safety and
sandboxing, and Evaluation and data.

- **NVIDIA Data Designer**: dataset config scripts as editable config files, `preview` and `create`
  runs (one at a time, OpenShell-contained in the lab), record review, parquet/csv/jsonl/json copies.
- **NVIDIA OpenShell**: the sandbox gateway behind Data Designer. Health, sandboxes, logs, policy
  proofs; security checks raised as problems (the sandbox network must be internal, no non-loopback
  port). Its trust files are read-only (ADR 0007).
- **NVIDIA DCGM**: this host's dcgm-exporter plus every DCGM host the lab's Prometheus scrapes;
  per-GPU charts; per-GPU `dcgmi diag` in a hardened throwaway container; the exporter's field list
  editable with automatic rollback; flags an exporter older than 4.8.3 (CVE-2026-47483) and probes
  `/debug/pprof`.
- **NVIDIA AIPerf**: load-test presets (TTFT, ITL, latency, throughput) through a lab wrapper with
  its own router key and no network beyond the router.
- **NVIDIA NeMo Guardrails**: an isolated rails server; rails configs editable with validation and
  rollback, but a change that removes rails needs an explicit marker (ADR 0015); probe checks and a
  playground through the rails.
- **NVIDIA NeMo Evaluator**: offline benchmark suites (rule-scored only) against router models,
  scores per benchmark and model.
- **Fixed**: `nemo-console stop` now works with a browser open (open event streams end with
  `event: shutdown`); three status caches no longer skip their check right after a reboot; DCGM
  per-GPU charts render.
- An independent security review of the new code found no high-severity issue; every finding was
  fixed (THREAT-MODEL E-8..E-11, R-20..R-23).
- Known limitations: verified on one workstation only; several disruptive actions (OpenShell gateway
  start/stop and sandbox delete, DCGM level-2 diagnostics, a Guardrails config apply through the HTTP
  API) are covered by tests with fakes, not run live — `docs/FEATURE-MATRIX.md` says which.
