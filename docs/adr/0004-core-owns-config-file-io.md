# ADR-0004: The core owns config-file IO; adapters only validate and restart

Status: Accepted (v0.1.0)

## Context

Writing configuration files is the riskiest filesystem operation the console performs: it can replace a router's
routing table. Three adapters implementing it independently would produce three sets of race and permission bugs.

## Decision

`services/config_store.py` performs all config reads and writes. Adapters expose `config_files()`,
`config_path()`, `read_config()`, `validate_config()` and `post_apply()` only. The store:

- builds an allowlist of absolute, resolved paths **at startup**, from each adapter's `config_files()` and
  `config_path()`; a path that later resolves differently is refused (`path_changed`, 403), as is a symlink
  (`symlink_refused`, 403);
- computes the SHA-256 from the raw file bytes and uses it for compare-and-swap: apply with a stale
  `base_sha256` returns 409 `stale_base` and leaves the file untouched;
- caps content at 256 KiB (413), rejects NUL bytes (422 `invalid_content`), and refuses content that looks like a
  literal secret (422 `secret_in_content`; env-var **names** are allowed);
- backs up the previous bytes into SQLite (`revisions`, last 50 per file by default) before writing;
- writes atomically: temp file in the same directory, `fchmod` to the original mode, `fsync`, `os.replace`,
  directory `fsync`;
- serialises per (system, file) with an asyncio lock, and the routers serialise per system (409 `busy`);
- records the apply in the audit log without file content (audit rows exist only for authenticated, authorised calls, see ADR-0012).

## Consequences

- `os.replace` swaps the inode. A container that bind-mounts the file keeps the old inode, so an adapter's
  `post_apply` must **recreate**, not restart (Switchyard does; contract addendum 9.2).
- File revisions live in the SQLite database, not on disk beside the file. Losing the state directory loses
  rollback history.
- Rollback re-validates the target revision with the adapter's validator and re-runs the secret scan before writing
  (422 `validation_failed` / `secret_in_content` / `invalid_revision`, nothing written), so a stored revision that no
  longer passes today's rules cannot be restored blindly (`test_rollback_validates_and_scans_the_target_revision`).
- An absent editable file reads as empty content and the first apply creates it with private permissions; a failed
  first apply removes it again (`test_absent_file_reads_as_empty_and_apply_creates_it_private`,
  `test_failed_first_apply_removes_the_new_file`). Viewers receive content with literal secrets redacted (env-var names
  kept); the SHA-256 is always of the raw bytes (`test_viewer_gets_redacted_config_admin_gets_raw`).
- One semaphore(2) bounds concurrent validate, apply and rollback calls (each may start a container).

## Alternatives considered

- **Adapters write files.** Rejected for duplication of dangerous code.
- **Backups as sibling files (`routes.toml.bak.N`).** Puts copies in directories that may be tracked by git
  or mounted into containers; SQLite keeps them private (mode 0600 state dir).
- **File locking instead of CAS.** Editors and humans do not take locks; a content hash detects edits made
  outside the console.
