# ADR-0009: Dynamo lifecycle via `docker compose` directly, never the model-switch helper

Status: Accepted (v0.1.0)

## Context

Dynamo's worker shares one GPU with another serving engine (they are pinned to the same GPU UUID; only one can
hold the VRAM). The operator's existing helper script for switching the loaded model also stops engines on the
*other* GPU (measured by the author on 2026-09-23). A console button that invoked it would make "start Dynamo"
silently take down unrelated serving.

## Decision

`up`, `down`, `restart` and `up_mock` run `docker compose` on the Dynamo project only (with the override file
when present). A `preflight` action lists the running containers pinned to the same GPU UUID (read from the
compose file, falling back to the running worker's device request) and states which router aliases go dark while
Dynamo holds the card. `up` is **refused** while another engine holds the card unless the request carries
`stop_sharing_engine=true`, which stops exactly one named container after verifying its name, compose project,
service name and config-file path labels; the stop is a plain compose `down` in that engine's own project,
without the volume-removal flag. If any other container shares the card, or the labels do not match, `up` is
refused and nothing is stopped. The UI runs preflight before every lifecycle action and shows the shared card and
darkened aliases in the confirmation.

## Consequences

- Tests with fakes cover refusal and the stop path (see `backend/tests/dynamo/test_adapter.py`).
- Verified live (maintainer): preflight, `down` (400 without confirm), `up`, then serving again, all through the
  console. **Not verified live:** `restart`, `stop_sharing_engine` (the live run started from the state where the sharing engine was already stopped).
- The name, project and service of the sharing engine are constants in `adapters/dynamo.py`; a different
  deployment needs a code change.
- `up_mock` starts a CPU mocker and is classed `safe`; it is refused while the real worker runs.
- Cold start is roughly four minutes; the action blocks for up to 600 s and the router holds the per-system
  busy lock meanwhile (a second mutation returns 409 `busy`).

## Alternatives considered

- **Call the helper script.** Rejected on the measurement above.
- **Stop *any* container found on the card.** Rejected: only the named, label-verified engine is ever stopped.
- **No sharing awareness.** Rejected: `up` would fail late with an out-of-memory error inside the worker.
