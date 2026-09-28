# ADR-0005: Apply = validate, diff, confirm, backup, write, restart, health-check, auto-rollback

Status: Accepted (v0.1.0)

## Context

A syntactically valid but semantically bad routing file can leave the router down. The operator is using the
console precisely because they do not want to be at a shell when that happens.

## Decision

The apply pipeline is fixed: the UI validates and shows a diff, the operator confirms
(`confirm:true` is mandatory, 400 `confirm_required` otherwise), the core re-validates (a validator returning
`ok=false` blocks the apply with 422 `validation_failed` even when `authoritative=false`), backs up, writes
atomically, and calls the adapter's `post_apply`. If `post_apply` returns `post_check="unhealthy"` (or raises),
the core writes the previous bytes back and calls `post_apply` again. The result carries `rolled_back=true`; if
the rollback is also unhealthy the detail says so ("investigate manually"). Manual rollback to a stored
revision uses the same write-and-check path.

Health checks are per adapter: Switchyard polls `/health` for up to 30 s after `docker compose up -d
--force-recreate --no-build`; Relay restarts the user unit and requires several consecutive healthy checks with
a stable PID and restart counter (to catch crash loops); Dynamo waits up to 600 s for the worker to be recreated
and `/v1/models` to list a model.

## Consequences

- Tests prove the mechanism with fakes: `test_auto_rollback_when_unhealthy`,
  `test_auto_rollback_when_post_apply_raises`, `test_manual_rollback_to_a_revision`,
  `test_validation_failure_blocks_write`.
- **Auto-rollback has not been observed live.** The live check exercised Switchyard config validate, a
  stale-sha 409, and a successful apply with container recreate and health check; a deliberately failing apply
  was not run against a real system.
- Rolling back means the router is briefly down twice (bad config, then restored config).
- Dynamo parameter apply has its own, separate rollback (restore previous override, recreate worker); it is
  implemented and unverified live.
- `--no-build` on recreate means a missing image fails the apply (and rolls back) rather than triggering a build.

## Alternatives considered

- **Validate-only, no restart.** Leaves the operator to restart; defeats the purpose and hides drift (the
  Switchyard adapter reports `switchyard.config.drift` when file and running server disagree).
- **Blue/green (start a second instance, switch).** Impossible with a single fixed host port and a single GPU.
- **Rollback only on explicit operator click.** Rejected: the failure mode is an unattended router outage.
