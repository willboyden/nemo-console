# ADR-0010: Events carry metadata only

Status: Accepted (v0.1.0), **with a known deviation for Dynamo**

## Context

Switchyard's routing log and Relay's ATOF/ATIF traces can contain conversation content. Events are streamed to
the browser, kept in ring buffers, and could be scraped by anyone with viewer access.

## Decision

Events (`SystemEvent`) carry route, tier, model, token counts, latency, event type and identifiers, never prompt
or completion text.

- **Switchyard:** `parse_routing_line` copies a fixed set of fields; `route`, `algorithm`, `model`, `tier` and `session_id`
  are kept only when identifier-shaped, otherwise replaced by `<redacted>`.
- **Relay:** `parse_atof_line` copies from an explicit allowlist and only strings that look like identifiers
  (`[A-Za-z0-9_.:/@+-]{1,120}`), so free text cannot reach an event; lines over 1 MiB are skipped.
- **Core:** every event is passed through `redact_obj` (secret-shaped substrings and sensitive key names) again
  before it leaves the process, on both the list and the SSE paths.

## Consequences

- Tests: `test_routing_line_is_metadata_only` (Switchyard), `test_stream_delivers_events_redacted_and_releases_slot`
  (core), Relay ATOF tests in `backend/tests/relay/test_relay_events.py`.
- **Deviation:** Dynamo events are *container log lines* (`parse_log_line` in `adapters/dynamo.py`): ANSI stripped,
  secret patterns masked, absolute paths collapsed, truncated to 400 characters, level and component attached.
  That is redaction, not an allowlist. If the frontend or worker logs request content (for example at debug
  log levels), that content would be shown. Not observed at default log levels; not proven absent.
- The Switchyard `session_id` is passed through as an identifier; it is metadata but linkable.

## Alternatives considered

- **Structured Dynamo events from its metrics/OTLP instead of logs.** Not done in v0.1.0; logs were what was
  readable through the runner.
- **Drop Dynamo events.** The operator wants to see load errors and restarts; kept, with the caveat documented.
