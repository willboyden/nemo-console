# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.9.0 — 2026-11-05

Field help in the config editor, and the fixes from a security audit.

- **An info button (ⓘ) on every setting of the Form view.** Its pop-up says what the setting is, what changing it
  does, the values it accepts and when a change takes effect, or why the file is read-only. Sections the console knows
  have an "About this section" button. Every setting of the Switchyard, Relay, AIPerf, Evaluator, Model Optimizer,
  Guardrails and OpenShell files has a description; a Dynamo command-line flag also shows its entry from the Params
  tab. A setting without a description still shows its type, accepted values and the comment from the file.
- **Secret masks stop at the end of the line.** A line ending in "bearer" no longer swallows the next line into
  `[redacted]` (an OpenShell provider profile was shown corrupted), and the save-time secret scan no longer refuses
  such files.
- **Apply refuses a file the console shows masked** (409 `hidden_content`) instead of writing the mask over the real
  text, and a config read hashes exactly the bytes it showed, so a concurrent change is never overwritten.
- **The editor survives hostile files** (absurd nesting, very long lines of spaces, keys named `constructor`), the Form
  writer handles a few more YAML corner cases exactly, and a large paste no longer makes the diff use gigabytes.
- **How it was checked:** an independent security audit and a re-audit of the fixes; new Rust tests for the masks,
  the hidden-content refusal and the read retry; new frontend tests that every setting of every mock file and every
  schema key has a description, plus regression tests for each editor fix; the config-editor browser check opens and
  closes a pop-up.

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
