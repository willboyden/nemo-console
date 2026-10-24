# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.3.1 — 2026-10-24

A fix for the five pages added in 0.3.0, and a browser test that now gates every push.

- **Fixed: GPU telemetry, AIPerf, Guardrails, OpenShell and Evaluator showed "Something went wrong"** in a browser
  that had used an older version. The console served its files without `Cache-Control`, so the browser kept old
  shared modules and paired them with the new pages. Every frontend file is now revalidated on each load
  (`Cache-Control: no-cache`, ETag 304). **If you used an earlier version, reload the console once with
  Ctrl+Shift+R.**
- **Fixed, found by that suite on a fresh clone:** the Dynamo Params preview answered 500 when the Dynamo compose was
  missing (now a 422 with the reason), and the Config tab of a system that is not deployed said "Something went
  wrong" (now "This file is not on this machine", with the reason and a Retry button).
- **Added: a browser end-to-end suite** (`make e2e`): the real backend and a headless Chrome open every page and
  every tab and fail on any error state, console error or uncached-module risk. It runs in `make all` and in CI.
- **Added: a pre-push hook** (`make hooks`, part of `make setup`) that refuses to push code the suite has not passed
  on. If your Chrome is not on PATH: `git config nemo-console.chrome /path/to/chrome`.

Upgrade: pull, `make setup`, restart the console (`nemo-console stop`, then start it as before), reload the page with
Ctrl+Shift+R.
