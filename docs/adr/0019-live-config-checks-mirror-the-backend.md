# ADR-0019: Live config checks in the browser mirror the backend validators; the backend still decides

Status: Accepted (0.8.0)

## Context

Editing a config was a textarea plus a Validate button. The backend validators are the authority (ADR-0005,
ADR-0006), but each check is a POST that writes an audit row, and some validators start a process or a container
(the Switchyard dry-run, `guardrails-local validate`, Data Designer's script loader). So they cannot run on every
keystroke, and a typo stayed invisible until someone pressed the button.

The console knows four schemas well: AIPerf load profiles, Model Optimizer recipes and NeMo Evaluator suites (closed
schemas with ranges and names) and the Switchyard routes file (targets, clients and routes that name each other).
Everything else is TOML or YAML whose meaning only the real tool knows.

## Decision

1. **A browser-side parser for TOML 1.0 and a strict subset of block YAML** (`frontend/js/cfg-parse.js`). It keeps
   the exact source span of every value so the new Form view can rewrite one value in place and leave comments,
   blank lines and key order alone (`cfg-edit.js`). YAML is typed the way PyYAML 6 types it (YAML 1.1: `yes`,
   `0755`, `1e3`), because the backend loads it with a PyYAML-compatible loader. Valid YAML the subset does not model
   (anchors, aliases, tags, several documents, multi-line flow or quoted values) is reported as **unsupported**,
   never as an error: the Form view is then unavailable for that file and the Text view and the server check still
   work.
2. **Live checks on every change** (`cfg-checks.js`): syntax, the four schemas above, Switchyard cross-references and
   `base_url` rules, and content the backend refuses to save (secret-looking literals, NUL). Results mark the lines
   in the Text view's gutter and the fields in the Form view, and are listed in an Edit checks panel.
3. **The backend still decides.** The live checks never block anything. The server validator runs on Check
   (Ctrl/Cmd+S) and always before Apply, and only its verdict blocks an apply (unchanged). The panel shows when the
   last server result is older than the draft.
4. **The mirror is tested against the backend, not trusted.** `tests/cfg-checks.test.mjs` reads the schema constants
   out of the Rust adapters (`aiperf.rs`, `modelopt.rs`, `evaluator.rs`, `switchyard.rs`) and fails when they
   differ. `frontend/e2e/config-editor.mjs` (part of `make e2e` and CI) runs the real backend on seeded random
   mutations of each schema file and fails when the browser and the backend disagree on whether a draft has errors.

## Consequences

- Most mistakes show up as you type, at the line or field they concern, without an audit row per keystroke.
- Two copies of each schema exist. The drift tests catch a changed constant or a changed verdict on the mutation
  corpus; they cannot prove the copies agree on every input. Where they disagree, the server wins, so the cost of a
  drift is a misleading hint, not a bad write.
- The YAML subset is maintained by hand. It was compared with PyYAML 6 and tomllib on 178 real files of the
  author's lab and 4,300 mutated copies (0 false errors, 0 missed errors on files it models) and on 1,619 random
  Form edits (every value read back as typed, nothing else changed) when it was written; those corpora are
  private, so the committed tests use synthetic cases.
- The Switchyard dry-run itself needs docker; the e2e mirror compares only the semantic diagnostics.
- Unsaved drafts now survive switching files and leaving the page within a session (memory only, never stored;
  reloading the tab asks first).

## Alternatives considered

- **Auto-run the server validator while typing (debounced).** Rejected: an audit row per pause, process starts for
  some validators, and the audit log would drown real actions.
- **A schema endpoint served by the backend.** Better long term (one source of truth), but it changes the API
  contract the parity harness checks against the Python reference. Possible later; the drift tests make the mirror
  safe enough until then.
- **A third-party editor or YAML library (CodeMirror, js-yaml).** Rejected by ADR-0013 (zero npm, strict CSP).
