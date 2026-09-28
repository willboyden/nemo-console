# ADR-0007: Relay trust and policy files are read-only in v1

Status: Accepted (v0.1.0)

## Context

Relay's `plugins.toml` carries the plugin attestation mode and trusted public keys (signature enforcement) and the
PII-redaction and exporter configuration; `config.toml` is the user config layer. Editing these is a security
decision (it decides which native code the gateway will load), not a tuning knob.

## Decision

The Relay adapter lists both files with `editable=false` and a `why_readonly` explanation. The core refuses
apply and rollback for them with 403 `read_only`. The console still reads them (contents run through a secret
scrubber; the SHA-256 is of the raw bytes) and derives problems from them: weak attestation
(`relay.plugin.attestation_weak`), no plugin configured, no exporter enabled, PII redaction absent while ATOF/ATIF
are on, and "file changed after the gateway started". Each problem's `fix` tells the human what to change by hand.

## Consequences

- The console cannot be used to weaken Relay's signature policy, even by an attacker holding an admin key.
- Operators edit these files in an editor and restart via the console (`gateway_restart`).
- The UI offers review only; there is no in-console propose-and-apply flow for policy.
- A future writable mode would need a distinct, stronger confirmation and probably a second factor.

## Alternatives considered

- **Editable with confirmation.** Rejected for v1: one stolen or misused admin key would then control code
  loading.
- **Hide the files.** Rejected: operators need to see policy state, and problems reference it.
