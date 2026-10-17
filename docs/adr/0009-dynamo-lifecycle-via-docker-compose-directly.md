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

## Addendum (2026-10-06): the premise changed; the card is shared with the default model

**What changed.** The context above assumed one other engine (`vllm-qwen38`) shared the worker's GPU, so stopping it was
enough. Since 2026-09-26 the author's default model is `qwen3.8-flash-next`: SGLang at tensor-parallel 2, holding about
60 GB of **both** cards, plus the always-on `nemotron-lightning` (about 29 GB of the worker's card). The 27B and Dynamo are
opt-in and off by default. The worker wants 0.5 of the card, which does not fit beside the default model; on 2026-09-28
Dynamo was started beside it and the worker crash-looped ("Free memory on device ... less than desired GPU memory
utilization"). The default model must also start before lightning, an order that only its own script knows.

**Decision (amends the one above; the console still executes only `docker compose`).**

- The console still never *runs* the model-switch script. It now *shows* it: the start fix is the setting `dynamo_start`
  (default `scripts/use-model.sh qwen3.8-27b-dynamo`, which stops the default model in the right order) and the restore fix
  is `default_model_start` (default `scripts/use-model.sh qwen3.8-flash-next`). No problem suggests a bare
  `docker compose up -d` any more. The original measurement (the helper stops engines on the other card) is now the
  intended effect, because the default model spans both cards.
- Sharers are classified by settings: exclusive (default: the flashnext and 27B containers) refuse `up`; coexisting
  (lightning, ollama) and ignored (the dcgm exporter) do not; any other container on the card refuses `up` and is named,
  with an identify-first fix, never `docker stop`.
- `stop_sharing_engine=true` may only stop the 27B (`STOPPABLE_ENGINE`, fixed in code so a setting cannot widen what the
  console stops). With the default model on the card, `up` stays refused and the fix is `dynamo_start`.
- With `dynamo_opt_in` (default true), a Dynamo with no container running or restarting is `health=off` with one info
  problem, not `down` with errors; `/health` ignores it.

**Consequences.** Covered by fakes only (`backend/tests/dynamo/test_adapter.py`, `backend/tests/core/test_dynamo_policy.py`);
the container classification uses device-request shapes read from the live containers on 2026-10-06 (`docker inspect`, read
only), but no lifecycle action was run against the live stack for this change. The container names in the settings
defaults are the author's; another deployment sets the `dynamo_*` settings instead of editing code (the stoppable 27B is
still a code constant).

## Addendum (2026-10-17): a stack can run from more than one compose file

The author's resident Dynamo stack is now Nemotron 3.5 Super VL, started from `docker-compose.yml` +
`docker-compose.super-vl.yml` (an nginx gateway at the `dynamo-frontend` service name, the real frontend as
`dynamo-sv-frontend`, the worker on both GPUs). The decision stands (the console executes only `docker compose`), with one
change: the compose files are the ones the running containers were started from (their compose label, treated as
untrusted and confined to the configured compose directory), not always the base file; with no Dynamo container the base
applies as before. Params and the console override are refused while a repo override file is in use, and the restore fix
(`default_model_start`) is the switch script without an alias, so no fix tears the running layout down. ARCHITECTURE 9.6
has the rules.
