# Releasing

How a release of Nemo Console is cut, and how the pinned toolchain and dependencies are bumped. Paths are relative to
the repository root. Every command here is offline except where it says otherwise.

## 1. Before the release

1. **Version.** Set `version` under `[workspace.package]` in `backend/Cargo.toml`, then refresh the lock file without
   touching any dependency: `cd backend && cargo update --workspace --offline`. `cargo build --locked` must still pass.
2. **Notes.** Add a dated section to `CHANGELOG.md` (Keep a Changelog; claims labelled implemented / verified-live /
   unverified as the README defines them) and replace `RELEASE-NOTES.md` with the notes for this release only.
3. **Supported versions.** Update the table in `SECURITY.md` when the minor version changes.
4. **Claims.** A "verified live" claim in `docs/FEATURE-MATRIX.md` or the README names what was run, against what, and
   when. Do not stamp something verified that was not run for this release.

## 2. Gates, from a clean clone

Run every gate from a fresh clone of the commit you will tag, outside the working checkout, so nothing untracked
(a build cache, a local venv, an ignored fixture) can make a gate pass:

```bash
git clone --no-local <checkout> /tmp/nc-release && cd /tmp/nc-release
make setup                 # cargo build --locked, enables .githooks
make all                   # rustfmt, clippy -D warnings, cargo test, cargo-deny, cargo-audit, crate-age, frontend tests, e2e
make parity                # the differential harness against the last Python backend (see section 4)
node parity/check-test-map.mjs --no-pending && node parity/check-routes.mjs
node parity/xstate.mjs && node parity/startup.mjs && node parity/launcher.mjs
```

`cargo deny`, `cargo audit` and `crate-age.py` read crates.io and the RustSec advisory database; nothing else uses the
network. CI (`.github/workflows/ci.yml`) runs the same gates on every push and pull request: the `backend`, `frontend`,
`e2e` and `parity` jobs. The pre-push hook (`.githooks/pre-push`) refuses a push whose tree has no recorded e2e pass
(`scripts/e2e-gate.sh`).

## 3. Tag and publish

Tag the release commit `vX.Y.Z` (annotated), push the tag, and create the GitHub release from the
`RELEASE-NOTES.md` text. Watch CI on the tag until every job is green.

## 4. The Python reference

The Python backend was removed in 0.4.0. Its last revision is the behavioural reference of the parity harness:
tag `python-final`, which must name commit `aa2f810` (`parity/ref.mjs` refuses a tag that names anything else, and
uses that commit when the tag has not been fetched). `node parity/ref.mjs` extracts that revision's `backend/` into
`parity/.ref/` and builds its venv once with `uv sync --frozen`; set `PARITY_UV_ONLINE=1` for that first install on a
new machine (PyPI). A deliberate behaviour change is recorded as a deviation in `backend/PORTING.md` section 13 and,
when the harness sees it, in `parity/allow-diffs.txt`.

## 5. Bumping the Rust toolchain

`backend/rust-toolchain.toml` pins one exact toolchain for developers and CI. To bump it:

1. Change `channel` to the new exact version (never `stable`), and let `rustup` install it on the next cargo call.
2. Run `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings`; fix new lints in
   the code rather than allowing them, unless the lint is wrong for this code base (then allow it in
   `backend/Cargo.toml` `[workspace.lints]` with a comment saying why).
3. Run the full gate of section 2 and note the bump in `CHANGELOG.md`.

## 6. Dependencies

- New or updated crates must have been public for at least 14 days (`backend/tools/crate-age.py`, run by
  `make crate-age` and CI). The only exceptions are security fixes: the smallest release that fixes a RustSec
  advisory, listed in `backend/tools/crate-age-exceptions.toml` with its advisory ids. Delete an exception once the
  gate reports it stale.
- Licences, banned crates and sources are checked by `cargo deny check` against `backend/deny.toml`; advisories by
  `cargo audit`. A new dependency's licence must be on the `deny.toml` allow list.
- Prefer the standard library or an existing dependency over a new crate. Update with
  `cargo update -p <crate> --precise <version>` so the lock file changes only where you meant it to.
