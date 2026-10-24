# ADR-0015: NeMo Guardrails rails configs are editable; weakening them must be deliberate

Status: Accepted (2026-10-24, addendum 9.8)

## Context

ADR-0007 makes trust and policy files read-only: a web console must not be the tool that loosens a security
control, because one stolen or misused admin key would then control it. OpenShell's sandbox policy follows that
rule (addendum 9.8).

NeMo Guardrails rails configs (`security/nemo-guardrails/config/<id>/config.yml`, `prompts.yml`, Colang files) are
also a safety control: removing `self check input` from `rails.input.flows` turns the jailbreak check off. But unlike
Relay's signature policy or OpenShell's sandbox policy, tuning rails is the everyday job of the system. Prompts are
reworded, flows are added and thresholds change, and every change has to be validated against the pinned
nemoguardrails and checked with real probe prompts. The independent audit of 2026-10-24 (M3) flagged the
conflict with ADR-0007.

## Decision

Rails configs stay editable through the console's normal apply flow (validate → diff → confirm → backup → atomic
write → restart → served-check → automatic rollback). Two rules make weakening deliberate:

1. **Shrinking the rails is refused by the validator.** `validate_config` for a rails `config.yml` returns an error,
   so apply is blocked, when the candidate removes a flow from `rails.input.flows` or `rails.output.flows`, or
   removes either section, compared with the live file. The exception is a candidate that carries the exact marker
   line `# nemo-console: allow-weaken-rails`. The marker is visible in the diff, stored in the revision history, and
   the apply is in the audit log.
2. **Executable code is never editable.** Python files under a config directory (`actions.py` and similar run inside
   the server) are read-only, as are files that are not plain text.

Every other edit (prompts, models, added flows) goes through without the marker. The `check` action sends fixed
benign and jailbreak-style probes after an edit to confirm the rails still block.

## Consequences

- An admin key can still weaken the rails, but not silently: the candidate has to say so in a line that lands in
  the diff, the revision history and the audit trail.
- Rewording a prompt so that it stops blocking is NOT caught by rule 1 (the flow list is unchanged). The `check`
  action is the control for that, and it has to be run by a person.
- The server holds no key and has no internet egress, so a weakened rail changes what Lightning answers, not what
  leaves the machine.

## Alternatives considered

- **Read-only, like ADR-0007.** Rejected: it would move every rails change out of the console, including the
  validate-and-rollback safety net that makes routine tuning safe.
- **A typed confirmation in the API.** Rejected for now: the core's apply body has no per-system fields, and a
  marker in the content carries the same intent while staying visible in history.
