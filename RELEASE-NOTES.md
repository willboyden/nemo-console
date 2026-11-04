# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.8.0 — 2026-11-04

A config editor with live edit checks. Every Config tab now has a **Form** view and a **Text** view of the same
draft, and an **Edit checks** panel that updates as you type.

- **Form view (TOML and YAML):** one section per table or mapping and a typed control per value: switches for
  true/false, drop-downs for fixed choices and for the targets and clients a Switchyard routes file defines, number
  fields that refuse anything that is not a number. Each field shows what it accepts and the comment from its line.
  Search, Changed / With problems filters and a per-value revert. An edit rewrites only that value's characters, so
  comments and layout survive and the diff shows exactly what changed.
- **Text view:** line numbers that mark problem lines, Tab / Shift+Tab indent, Enter keeps the indentation,
  Ctrl/Cmd+S runs the server check.
- **Edit checks:** syntax errors with their line; the rules of AIPerf profiles, Model Optimizer recipes and NeMo
  Evaluator suites; Switchyard cross-references and `base_url` rules; secret-looking literals the console would refuse
  to save. Click a problem to jump to its line or field.
- **The server still decides:** the browser checks are early warnings. The backend's validator runs on Check and
  before every Apply, and the panel says when its last result is older than the draft (ADR 0019).
- **Drafts are kept** when you switch files or leave the page and come back (memory only, for the session; reloading
  the tab asks first).
- **How it was checked:** new unit tests for the parser, the checks and the in-place writer, including a test that
  reads the schema constants from the Rust adapters; a new browser check (`frontend/e2e/config-editor.mjs`, in
  `make e2e` and CI) in which the browser checks must agree with the real backend validators on seeded random
  mutations of each schema file, then the editor is driven in Chrome. Files that use YAML features the browser parser
  does not model (anchors, tags, several documents, multi-line lists or strings) open in the Text view.
- **Unchanged:** the backend's behaviour and API (only its version number moved).

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
