# ADR-0006: Validators that are only a proxy say so (`authoritative=false`)

Status: Accepted (v0.1.0)

## Context

Relay loads Switchyard routing through a native plugin whose schema differs from `switchyard-server`'s
(measured by the author: the server accepts fields such as `capable_system_prompt` that the plugin rejects).
Running the Switchyard dry-run against a Relay routes file would give false assurance.
`nemo-relay plugins validate` and `doctor --offline` can pass while a gateway start still fails (unsigned plugin
under host policy, a routes file the plugin rejects).

## Decision

`ValidationResult` and `ConfigFileSpec` carry `authoritative`. Relay's routes validator is a TOML parse plus a
semantic pass that mirrors the Switchyard schema-1 language, reports `authoritative=false`, and attaches a note
saying the real check is a gateway restart, which the apply flow performs with auto-rollback (ADR-0005).
Switchyard's validator (`switchyard-server --dry-run` in a throwaway, network-less, read-only container fed over
stdin, plus a semantic cross-check) is `authoritative=true`. Dynamo's override validator is authoritative only
if `docker compose config -q` also passed. The UI shows the flag.

## Consequences

- An operator is told when a green check does not mean "will start".
- `ok=false` from any validator, authoritative or not, still blocks apply (contract addendum 9.2).
- The semantic mirror can drift from the plugin's real schema; it is a hand-maintained copy
  (`relay.py: validate_routes_semantics`). Unverified against future Relay versions.
- Both routes validators (Switchyard and Relay) also raise an **error** for any `llm_client` `base_url` whose host is
  not loopback and not a bare single-label service name, and for userinfo in the URL, so a routes file cannot point
  traffic off the box. A bare single-label hostname (for example `metadata`) is accepted as a service name, so the
  check guards against off-box IPs and dotted hosts, not against a hostile local resolver or search domain. An
  `allowed_service_names` setting is a possible future option (not implemented).
- Switchyard's dry-run requires the image to be present locally (`--pull never`); otherwise the diagnostic says
  the image is missing.

## Alternatives considered

- **Use the Switchyard dry-run for both.** Rejected: measured to disagree.
- **Run the Relay plugin loader offline.** No such mode was found in Relay 0.9.2's CLI.
- **Hide the distinction.** Rejected: the failure it prevents is a false sense of safety.
