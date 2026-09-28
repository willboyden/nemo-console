# ADR-0008: Dynamo tunables as a structured catalog rendering a generated compose override

Status: Accepted (v0.1.0)

## Context

Dynamo's behaviour (context length, KV cache size, parser dialects, router mode, speculative decoding) is set by
worker and frontend command-line flags in a reviewed compose file. Hand-editing that file from a web UI would
put a reviewed artefact under automated writes, and free-text flag entry would be an injection surface.

## Decision

The Dynamo adapter exposes a fixed `ParamSpec` catalog (14 parameters at v0.1.0; types, bounds, choices, help
text; flags taken from the pinned image's `--help`). `preview_params` renders the exact override YAML that would
be used; `apply_params` writes it to `<state dir>/dynamo/override.yml` (0600), runs `docker compose config -q`
with it, and recreates the worker, rolling back to the previous override if the worker does not become healthy.
The repo compose file is exposed read-only and never edited. `docker compose` is then invoked with the base file
plus the override whenever the override is non-empty. The override file is also editable through the config API
but is validated against a **closed** allowlist: only the `dynamo-worker` and `dynamo-frontend` services, only
`command` and `environment`; `command` must start with the expected module; every `-`-prefixed token must be an
allowlisted flag (no single-dash or unknown flags); `$` is refused unless the token is byte-identical to a base-compose
token (compose would otherwise interpolate host env into argv); `--model` and `--served-model-name` must equal the
base; environment keys must already exist in the base compose and may not contain `$` or secret-looking values; if
the base compose cannot be read, these checks fail closed. Duplicate flags are also refused (normalised across
`--x=v`, `_`/`-` and `--no-x`, so a later duplicate cannot override a pinned value), `--speculative-config` must be exactly
`{"method":"mtp","num_speculative_tokens":1..8}`, and path- or JSON-looking values must equal the base. Remaining gap:
enum-valued flags are not checked against the catalog's choices and numbers are not type-checked (admin-only; argv
list, no shell).

## Consequences

- Reversible by deleting one file; the reviewed compose stays authoritative in version control.
- The catalog is pinned to one image version's flags; a new Dynamo version needs a catalog update.
- Dynamo's own parsers are configured with `--dyn-*` flags; the status check warns if the worker was started
  with vLLM's ignored `--tool-call-parser`.
- `POST .../params/preview` returns the adapter's `param_warnings` in `warnings` (an earlier revision returned an
  empty list; corrected).
- Not verified live: applying parameters to a real worker (preview rendering was checked against the real
  compose; the recreate and rollback are exercised by fakes only).

## Alternatives considered

- **Edit the repo compose in place.** Rejected: unreviewed writes to a tracked file.
- **Free-text extra flags field.** Rejected: unbounded surface, and flag semantics differ between frontend
  and worker.
- **Environment-only configuration.** Not sufficient; most of these are CLI flags.
