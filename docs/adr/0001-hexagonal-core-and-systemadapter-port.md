# ADR-0001: Hexagonal core with a `SystemAdapter` port

Status: Accepted (v0.1.0)

## Context

The console manages three unrelated systems (an HTTP router in a container, a host binary under systemd, a
compose-managed serving stack). They differ in how health, metrics, events, configuration and lifecycle are
obtained, but the operator wants one UI and one set of safety rules (confirmation, audit, redaction, rollback).
Fan-out of the build across several implementers also required a contract that could be frozen early.

## Decision

The core (API, storage, SSE, audit, config store, metrics poller) depends only on `domain/models.py` and
`adapters/base.py`. Each system is one `SystemAdapter` subclass (`adapters/switchyard.py`, `relay.py`,
`dynamo.py`) registered lazily in `adapters/__init__.py`. The port covers: `status`, `metrics`, `recent_events`,
`follow_events`, `config_files` / `config_path` / `read_config` / `validate_config` / `post_apply`,
`param_catalog` / `preview_params` / `apply_params`, `actions` / `run_action`, `playground` / `playground_url`.
The UI renders from the returned descriptors (`ConfigFileSpec`, `ParamSpec`, `ActionSpec`, `PlaygroundSpec`)
rather than from per-system code paths in the core. The two contract files were declared locked; changes are
additive only.

Rules every adapter must follow (base.py docstring): never shell out (use the injected runner), never read
secret files, read only paths handed in through `ctx.paths`, treat failure as data (a down system yields
`health="down"` plus a problem with a fix, not an exception), never fabricate a metric.

## Consequences

- A fourth system is one adapter file plus one registry line. (Implemented; only three adapters exist, so the
  claim that a fourth is cheap is untested.)
- `container.build_container` wraps a failing adapter import/construction in `MissingAdapter`, so one broken
  system degrades to `health=unknown` rather than taking the console down (test:
  `test_missing_adapter_module_is_unknown_with_problem`).
- The port is a lowest common denominator plus optional methods (params). The frontend still has per-system
  view modules (`views/switchyard.js`, `relay.js`, `dynamo.js`); "renders from descriptors" holds for the
  config editor, parameter form, actions panel and playground, not for the system-specific summary tabs.
- Deployment specifics (container names, compose project names, unit name) live as constants inside adapters,
  not in settings. That keeps the port clean but makes the adapters deployment-specific (see README).

## Alternatives considered

- **One module per system with its own routes** (no port). Fastest to write, but every safety rule (confirm,
  audit, redact, rollback) would be re-implemented three times and drift.
- **Plugin discovery via entry points.** Overkill for three in-tree adapters; lazy registry is enough.
- **Config-file-driven generic adapter** (URLs and jq-style extractors in YAML). Cannot express Dynamo's
  preflight, Relay's systemd health settling, or Switchyard's recreate-not-restart requirement.
