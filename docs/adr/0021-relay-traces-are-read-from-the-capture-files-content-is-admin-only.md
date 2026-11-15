# ADR-0021: Relay traces are read from Relay's own capture files; the index holds metadata only; content is an admin read

Status: Accepted (unreleased)

## Context

NeMo Relay records what an agent did at several levels: raw lifecycle events (ATOF, JSON Lines), trajectories (ATIF,
one JSON file per turn or session) and typed OpenTelemetry spans sent to a trace store. Until now the console showed
the first of these as a flat tail of the newest events, metadata only (ADR-0010), and nothing of the second.

The operator wants trace views in the console that stand next to what Grafana, Langfuse and Arize Phoenix show for
agent traces: a trace list, a waterfall, the conversation behind a model call, sessions, aggregate latency and token
charts, and a statement of how much of what Relay can record is switched on.

Three facts shape the design:

- The capture files are the complete record. Every OpenTelemetry projection is derived from the same events, and
  each projection drops something (the standard GenAI one drops marks and Relay's own identifiers).
- They are large. One model-call start carries the whole request, so single lines reach megabytes; a busy week is
  gigabytes, partly in gzip rotations.
- They hold conversation content. The console's existing rule for Relay events is an allowlist of identifier-shaped
  metadata; a trace viewer without the conversation is half a viewer.

## Decision

1. **Read the files, not a trace store.** The console builds its views from ATOF and ATIF on the console host. It
   needs no query access to Tempo, Phoenix or Langfuse, works when they are down, and shows what no projection
   keeps (marks, open scopes, streaming cadence, Relay's session and turn identity).
2. **An in-memory index, built in the background.** One task pairs scope start and end events into spans, groups
   spans into traces and sessions, summarises trajectories, and publishes a snapshot that requests read. Nothing is
   written to disk; a restart rebuilds it. Refresh is incremental and survives `copytruncate` rotation. Reading is
   bounded by settings (bytes per build, spans kept, line length, trajectory file size) and reports what it skipped.
3. **The index holds metadata only**, by the same rule as before: identifier-shaped strings, numbers, booleans and
   timestamps. Every viewer route is served from it, so a viewer key can see when, how long, which model, how many
   tokens and which tools, never what was said.
4. **Content is a separate, admin-only read.** A span's messages, tool arguments and results, and a trajectory
   step's text, are fetched on demand from the source file at the offset the index recorded. Those routes need the
   admin role, can be switched off (`relay_trace_content = off`), pass every text through the same secret scrubber the
   adapter uses elsewhere, cap each text and the whole answer, return an allowlist of request headers, and never
   return image data. The frontend asks for content only when the operator presses a button, and renders every
   captured string as text.
5. **No file name or path ever comes from a request.** Routes take trace, span and trajectory ids (UUID-shaped), a
   session key (identifier-shaped) and a step number; the index maps them to files it found itself, inside the
   capture directories, opened without following symlinks.
6. **Charts are hand-made SVG.** Geometry lives in pure functions with unit tests; rendering uses the existing DOM
   helper. No chart library, no canvas, no inline style: the CSP and the zero-dependency rule (ADR-0013) stand.
7. **Capture coverage is a first-class view.** The console reads `plugins.toml` (already an allowlisted, read-only
   file) and states which levels are on, where each trace projection goes, what was last written, and what a reader
   of the traces is missing.

## Consequences

- The Relay page gains five tabs: Traces, Sessions, Analytics, Trajectories, Capture. Addendum 9.12 is the contract.
- The first build after a start reads the newest file first and publishes as it goes; older rotations follow. Until
  then the index reports `building` with what it has.
- Memory grows with the number of spans kept (bounded), not with the size of the captures.
- Content of an event in a gzip rotation costs a stream decompression up to that event; beyond the configured bound
  the route answers 409 `content_unavailable` and the metadata stays visible.
- The console now reads conversation content when an admin asks for it. That is new: threat model I-9 and R-25.
- The Python reference has no such routes: a parity deviation in `backend/PORTING.md`.

## Alternatives considered

- **Query Tempo or Phoenix and draw their data.** Rejected: the console would need credentials or network paths it
  does not have today, would show only what one projection kept, and would be empty whenever the observability stack
  is down, which is when an operator most wants to look.
- **A persistent index in the console's SQLite file.** Rejected for now: it would put a second copy of trace
  metadata on disk, need migrations and a staleness story, and save only a rebuild that takes seconds.
- **Scan the files on each request.** Rejected: gigabytes per page view.
- **Put content in the index and gate it by role when serving.** Rejected: a bug in one route would leak it, and the
  process would hold every conversation in memory. Keeping content out of the index makes the viewer routes safe by
  construction.
- **Metadata only, no content at all.** Rejected: the operator already owns the files, and the comparison tools all
  show the conversation. An admin-only, switchable, scrubbed read is the smaller risk than sending operators to read
  raw JSON Lines.
- **A chart library.** Rejected under ADR-0013 and the CSP.
