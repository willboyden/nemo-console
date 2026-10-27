# Publishing checklist

What must happen before this repository is released publicly. Paths are relative to the repository root.

**Status for v0.1.1 (2026-09-28).** Done:
- Section 1: `LICENSE` holder; `pyproject.toml` licence, author and URLs; the non-affiliation notice in the README (not
  in the UI); the licence check. Runtime dependencies are MIT, BSD, Apache-2.0 or PSF, plus certifi under MPL-2.0,
  used unmodified.
- Section 2: fixtures and test literals sanitised. The public history starts from one fresh commit, so earlier
  captures are not in it.
- Section 3, in part: the private checklist reference in `docs/ARCHITECTURE.md` and the repository name in problem
  fixes and test skips.
- Section 5, in part: `SECURITY.md` and the CI workflow.
- Section 7: gates run from a clean clone.

Deferred, still open after 0.2.0 (documented in the README under "Deployment assumptions" and "Known limitations"):
- Section 3: the directory layout assumptions.
- Section 4: configurable deployment names.

Still open: `CONTRIBUTING.md`, the code of conduct, the issue and PR templates, rendering the Mermaid diagrams, and
verifying the systemd unit.

## 1. Legal and metadata

- [ ] `LICENSE`: the copyright holder line was copied from a sibling project and names the wrong project. Replace it with
      the actual holder and year (the MIT text itself can stay if that is the intended licence).
- [ ] `backend/pyproject.toml`: add `license`, `authors`, `readme` and `[project.urls]` (repository, issues, changelog).
- [ ] Add a **non-affiliation notice** (README and the UI About/Settings text): this project is independent and is not
      affiliated with, endorsed by or sponsored by NVIDIA. "NVIDIA", "NeMo", "NeMo Switchyard", "NeMo Relay" and "Dynamo"
      are names of products of their respective owners and are used only to describe compatibility.
- [ ] Confirm third-party dependency licences (`backend/uv.lock`) are compatible with the chosen licence.

## 2. Scrub personal and machine-specific data

Test fixtures were captured from a real workstation and still contain usernames, home directories and other host
identifiers. Review each file, replace with neutral placeholders (`/home/user`, `example`, zero UUIDs), and re-run the
tests (parsers must not depend on the real values):

- `backend/tests/switchyard/fixtures/inspect.json`
- `backend/tests/switchyard/fixtures/routes.good.toml` (around lines 93-107 and 220)
- `backend/tests/dynamo/fixtures/worker_inspect.json`, `frontend_inspect.json`, `base_compose.yml`
- `backend/tests/relay/fixtures/atof_sample.jsonl`, `doctor_offline.json`, `plugins_validate.json`, `agents.json`, `plugins.toml`
- Path literals in `backend/tests/relay/test_relay_config.py` and `backend/tests/dynamo/test_adapter.py`
- Also check for: GPU UUIDs, MAC addresses, IP addresses, session ids, container ids, hostnames.
- Verify with a repository-wide search for the username, the home directory, `uuid`-shaped strings and RFC1918 addresses
  (`grep -rnE '/home/[a-z]|GPU-[0-9a-f]{8}|([0-9]{1,3}\.){3}[0-9]{1,3}'`), including `git log -p` (history may still
  contain them: publish from a fresh, squashed history if it does).

## 3. Remove parent-project references

The code and `docs/ARCHITECTURE.md` were written inside a larger private repository. Remove or generalise:

- References to that repository's agent-instruction file and to its agent skills/tooling directories (for example the
  `"... rule 4"` text in two Relay adapter strings, and the private security-checklist name cited in
  `docs/ARCHITECTURE.md`).
- Layout assumptions: `inference/<system>/...`, `clients/nemo-relay/...`, `ops/*.env`, the model-switch helper script named
  in `docs/ARCHITECTURE.md` and comments, and the `console.repo_root.missing` fix text that names the parent checkout.
- The Relay adapter module docstring and comments that cite parent-repo paths.
- Either make the directory layout configurable (settings for each path in `Settings.resolved_paths()`) or document it as
  the required layout.

## 4. Configurability of deployment names

Done in addendum 9.11 (2026-10-26) for the container, compose and unit names and the repo layout; still constants on
purpose: `STOPPABLE_*` (Dynamo), OpenShell's sandbox network and router address. The original list, kept for reference:

- `adapters/switchyard.py`: `CONTAINER`, `COMPOSE_PROJECT`, `COMPOSE_SERVICE`, `FALLBACK_IMAGE`, `LOG_PATH`.
- `adapters/dynamo.py`: `PROJECT`, `FRONTEND`, `WORKER`, `MOCKER`, service names, `STOPPABLE_ENGINE`, `STOPPABLE_PROJECT`,
  `STOPPABLE_SERVICE`, `DEFAULT_MODEL`. (The card model is already in `Settings` since 2026-10-06: `dynamo_*`.)
- `adapters/relay.py`: `UNIT`, `DEFAULT_ROUTE_IDS`.
- `services/topology.py` (fixed node labels; the engine nodes are the `topology_engines` setting since 2026-10-17), `services/engines.py` (`compose_dir_hint`
  assumes a name prefix and layout), `config.py` (`DEFAULT_ENGINE_PROBES`, `DEFAULT_ENGINE_START`, `DEFAULT_URLS`).
- Optional: an `allowed_service_names` setting for the routes validators (single-label hostnames are currently accepted;
  see THREAT-MODEL R-14).

## 5. Repository hygiene files (currently absent)

- [ ] `SECURITY.md`: supported versions, private reporting channel, response expectations, and a pointer to
      `docs/THREAT-MODEL.md`.
- [ ] `CONTRIBUTING.md`: setup (`make setup`), gates (`make all`), the fixture-capture rule (fixtures must be sanitised real
      output), the "no new outbound network" rule, and the status vocabulary for docs.
- [ ] `CODE_OF_CONDUCT.md` (optional but conventional).
- [ ] `.github/ISSUE_TEMPLATE/` (bug report asking for versions of the three systems, redacted logs; feature request) and
      `.github/pull_request_template.md` (gate checklist, verification labels).
- [ ] `.github/workflows/ci.yml`: `uv sync --frozen`, `uv run ruff check src tests`, `uv run mypy src`, `uv run pytest -q`
      (live smoke tests stay skipped), and `node --test tests/*.test.mjs` in `frontend/` on Node 22. Pin actions by commit
      SHA. No secrets required.
- [ ] `.gitignore` review: state files (`*.db`, `*.key`) are already excluded; confirm nothing else machine-specific is tracked.

## 6. Documentation

- [ ] Update `docs/ARCHITECTURE.md` to the final behaviour (loopback trust off by default, audit scope, `/health` fields,
      Dynamo events as redacted logs, closed override allowlist) or mark it as the original design contract with a
      link to the ADRs for later changes.
- [ ] Re-verify every claim in `README.md` and `docs/FEATURE-MATRIX.md` against the release commit; keep the labels
      implemented / verified-live / unverified honest. Do not write "verified" for anything not run.
- [ ] Render the Mermaid diagrams (README, THREAT-MODEL) and fix any that do not render; they are currently unvalidated.
- [ ] Verify the systemd unit template on a target distribution (see OPERATIONS section 3, including the `226/NAMESPACE` note).

## 7. Final gates (from a clean clone)

```bash
git clone <candidate> /tmp/nemo-console-check && cd /tmp/nemo-console-check
make setup && make all
```

Expected: no failures. Tests that depend on the parent repository layout or on live systems are skipped in a clean copy, so
the skip count differs from an in-repo run; record both counts. Then run the live smoke tests against a real stack if the
release notes claim any live verification, and record what was run and against which versions.
