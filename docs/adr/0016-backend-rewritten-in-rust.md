# ADR-0016: The backend is rewritten in Rust; the last Python backend stays the reference

Status: Accepted (v0.4.0). Amends ADR-0002 (the host-run process is now a Rust binary, not FastAPI/uvicorn).

## Context

The 0.1-0.3 backend was a FastAPI app on uvicorn with pydantic models, run from a uv-managed Python 3.13 venv. Running
it needed the right interpreter, a venv and its wheels on every machine; its own dependency tree (FastAPI, Starlette,
pydantic, httpx, PyYAML) was most of the supply-chain surface; and the companion consoles of the same lab were being
ported to Rust at the same time, so one toolchain and one set of gates could serve all of them.

## Decision

Port the backend to Rust (one binary; axum + hyper on tokio, rusqlite, reqwest with rustls) with a fixed contract:
the same API (paths, methods, auth roles, status codes, bodies and error texts), the same settings and env file, the
same state directory and database schema, the same CLI and exit codes, the same log lines. Where Python's behaviour
is observable (pydantic's messages, `repr()`, `json.dumps`, PyYAML, tomllib, `shlex`, `difflib`, `gzip`,
`datetime.fromisoformat`), it is ported exactly and pinned by goldens generated with the Python implementation.
Deliberate differences are listed, each with its reason, in `backend/PORTING.md` section 13.

The Python backend is removed from the checkout. Its last revision (main at `aa2f810`) stays the behavioural
reference: the parity harness (`parity/`) extracts it with `git archive`, builds its venv with `uv sync --frozen`
(offline from the uv cache) and runs every corpus case against both backends with identical hermetic setups.

## Consequences

- One self-contained binary per checkout (`make release`); no interpreter or venv on the machine. The Rust toolchain
  is pinned (`backend/rust-toolchain.toml`) and the crates are pinned (`backend/Cargo.lock`), checked by cargo-deny,
  cargo-audit and a 14-day crate cooldown (security fixes excepted).
- The behaviour is defined by the Python reference plus the documented deviations, not by Rust's defaults: a change of
  behaviour now needs a parity case and, if deliberate, a deviation entry.
- Python bugs the parity checks surfaced were KEPT for parity (listed in PORTING.md and the CHANGELOG), to be fixed
  in both the reference expectations and the Rust code together.
- Every "verified live" claim in the docs was observed with the Python backend; until the Rust binary is run against
  the real systems those claims read "verified live in 0.3.x; implemented and parity-checked in 0.4.0".
- Contributors need rustup and Node 22 (frontend tests and e2e); `uv` only to run the parity harness.

## Alternatives considered

- **Keep Python, harden the packaging** (pinned interpreter via uv, pip-audit). Keeps the largest dependency surface
  and the per-machine venv; does not converge with the other consoles.
- **Rewrite with a new API.** Rejected: the frontend's JavaScript is the API contract; a compatible port could be
  checked mechanically, a redesign could not.
- **Keep both backends.** Rejected: two implementations to keep equal forever; the reference is kept as a fixed
  revision instead.
