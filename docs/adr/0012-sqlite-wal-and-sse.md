# ADR-0012: SQLite (WAL) for state; SSE for live streams

Status: Accepted (v0.1.0)

## Context

Single node, single user, a few writers (audit, revisions, rollups, keys, settings), small data, no wish to run
another service.

## Decision

`services/store.py` opens one SQLite connection (WAL, `busy_timeout=5000`, foreign keys on), created mode 0600
inside a 0700 state directory, guarded by a lock, with forward-only numbered SQL migrations
(`migrations/0001_init.sql`, tracked in `schema_version`). Tables: `keys` (SHA-256 only), `audit` (append-only:
UPDATE and DELETE triggers raise), `revisions` (config backups, pruned to the newest N per file),
`rollup` (1-minute averages, 8-day retention), `settings`. Live metrics are a 15-minute in-memory ring
(2 s cadence) plus rollups for the 1 h and 24 h windows (label cardinality capped at 256 series per system).
Live events use Server-Sent Events over `fetch` (so an `Authorization` header can be sent), capped at 8
concurrent streams (429), with a 15 s heartbeat and a 5 minute idle timeout (heartbeats do not reset it).

## Consequences

- Zero operations, one file to back up. Tests: `test_audit_is_append_only_and_records_mutations`,
  `test_ring_buffer_and_series_are_bounded`, `test_connection_cap_returns_429`,
  `test_idle_timeout_closes_stream_and_frees_slot`, `test_cancelled_consumer_releases_slot`.
- The audit triggers stop the application from editing rows but not someone with file access to the database
  (they can drop the triggers). Tamper-evidence is not claimed.
- The in-memory ring is lost on restart; history for the last hour or day is then rollup-resolution (1 minute).
- One connection and a lock serialise all DB access; fine at this scale.
- The audit table only receives authenticated, authorised mutating calls. Denied requests (401/403/421/429, viewer
  403s) are attacker-chosen volume, so they are counted (`nemo_console_denied_requests_total{status}`), kept in a
  100-entry in-memory ring and logged as at most one aggregated line per minute, never as rows.
- The SSE slot is taken inside the response generator (a stream that never starts holds nothing); the HTTP handler
  keeps a cheap pre-check that returns 429 when the cap is already reached.
- Frontend stream errors now parse the `{error:{code,message}}` envelope and treat every 4xx as final (an earlier
  revision looked for RFC-7807 fields and lost the message).

## Alternatives considered

- **Postgres or ClickHouse.** Rejected: an extra service for a single-user tool.
- **WebSockets.** Rejected: bidirectional not needed; SSE is simpler behind the same middleware.
- **Prometheus/Grafana only for history.** The console exposes its own `/metrics` (viewer role) for that, but
  the UI needs history without another stack.
