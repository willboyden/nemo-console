# Architecture decision records

One record per row of the decision table in [`../ARCHITECTURE.md`](../ARCHITECTURE.md) section 1. Status of every
record is **Accepted** (v0.1.0). Where the code deviates from a decision, the record says so under
"Consequences".

| ADR | Decision |
|---|---|
| [0001](0001-hexagonal-core-and-systemadapter-port.md) | Hexagonal core with a `SystemAdapter` port |
| [0002](0002-host-run-fastapi-on-loopback.md) | Host-run FastAPI on loopback, not a container |
| [0003](0003-single-commandrunner-choke-point.md) | All process control through one `CommandRunner` |
| [0004](0004-core-owns-config-file-io.md) | The core owns config-file IO; adapters validate and restart |
| [0005](0005-apply-flow-with-auto-rollback.md) | Apply = validate, diff, confirm, backup, write, restart, health check, auto-rollback |
| [0006](0006-non-authoritative-validators-say-so.md) | Proxy validators declare `authoritative=false` |
| [0007](0007-trust-policy-files-read-only.md) | Relay trust/policy files are read-only |
| [0008](0008-dynamo-params-as-generated-compose-override.md) | Dynamo tunables as a generated compose override |
| [0009](0009-dynamo-lifecycle-via-docker-compose-directly.md) | Dynamo lifecycle via `docker compose`, never the model-switch helper |
| [0010](0010-events-carry-metadata-only.md) | Events carry metadata only |
| [0011](0011-playground-proxied-never-persisted.md) | Playground chat is proxied, streamed, never persisted |
| [0012](0012-sqlite-wal-and-sse.md) | SQLite (WAL) for state, SSE for live streams |
| [0013](0013-zero-build-zero-npm-frontend.md) | Zero-build, zero-npm frontend with a strict CSP |
| [0014](0014-localhost-security-baseline.md) | Localhost-service security baseline applied in full |
| [0015](0015-guardrails-rails-editable-weakening-deliberate.md) | NeMo Guardrails rails configs are editable; weakening them needs an explicit marker (exception to 0007) |
