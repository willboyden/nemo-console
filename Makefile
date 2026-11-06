# Nemo Console developer tasks. Nothing here touches Docker or systemd; the only network use is cargo fetching crates,
# the crate-age gate asking crates.io and `make setup` installing the pinned Prettier (npm ci, frontend/package-lock.json).
# Backend: Rust (backend/, a Cargo workspace; the toolchain is pinned in backend/rust-toolchain.toml and rustup
# installs it on first use). Frontend: plain ES modules, no build, zero runtime npm dependencies (Prettier, the only
# devDependency, formats the code; nothing from npm is served or built into the binary).
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
BACKEND  := backend
FRONTEND := frontend
CARGO    := cargo
BIN      := $(BACKEND)/target/release/nemo-console
PRETTIER := $(FRONTEND)/node_modules/.bin/prettier

.PHONY: help setup hooks build release run test lint typecheck deny audit crate-age frontend-test fmt fmt-check \
        prettier-installed e2e parity all \
        install-desktop uninstall-desktop

help:  ## list targets
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/'

setup:  ## build the backend (debug) from Cargo.lock; install the pinned Prettier (npm ci); enable the git hooks
	cd $(BACKEND) && $(CARGO) build --locked
	cd $(FRONTEND) && npm ci --ignore-scripts --no-audit --no-fund
	if git rev-parse --git-dir >/dev/null 2>&1; then $(MAKE) --no-print-directory hooks; fi

hooks:  ## use the versioned git hooks in .githooks (pre-push refuses code without a recorded e2e pass)
	git config core.hooksPath .githooks

build:  ## debug build of the backend (what the e2e suite runs)
	cd $(BACKEND) && $(CARGO) build --locked

release:  ## optimised build: backend/target/release/nemo-console (what `run`, install-desktop and the service use)
	cd $(BACKEND) && $(CARGO) build --locked --release

run: release  ## serve API + UI on 127.0.0.1:8793 (foreground)
	$(BIN)

test:  ## backend tests (offline: fakes for every CLI and upstream; live smoke tests run only with NEMO_LIVE=1)
	cd $(BACKEND) && $(CARGO) test --locked --workspace

lint:  ## rustfmt check
	cd $(BACKEND) && $(CARGO) fmt --all --check

typecheck:  ## clippy, warnings are errors (the workspace lints in backend/Cargo.toml)
	cd $(BACKEND) && $(CARGO) clippy --locked --workspace --all-targets -- -D warnings

deny:  ## licenses, bans, sources and advisories (cargo-deny, backend/deny.toml)
	cd $(BACKEND) && $(CARGO) deny check

audit:  ## RustSec advisories for Cargo.lock (cargo-audit)
	cd $(BACKEND) && $(CARGO) audit

crate-age:  ## dependency cooldown: every locked crate at least 14 days old, security fixes excepted
	python3 -I $(BACKEND)/tools/crate-age.py

frontend-test:  ## node --test on the pure-logic modules (no npm install needed)
	cd $(FRONTEND) && node --test tests/*.test.mjs

# Prettier formats code and JSON only (js, mjs, cjs, ts, tsx, css, html, json); Markdown, YAML, fixtures, goldens and
# the parity harness's recorded data are left alone (.prettierignore). Options: .prettierrc.json.
prettier-installed:
	test -x $(PRETTIER) || { echo "Prettier is not installed: run 'make setup' (or: cd $(FRONTEND) && npm ci)" >&2; exit 1; }

fmt: prettier-installed  ## format the code and JSON in place with Prettier
	$(PRETTIER) --write --log-level warn .

fmt-check: prettier-installed  ## Prettier check: fails and lists every file that is not formatted (`make fmt` fixes them)
	$(PRETTIER) --check --log-level warn .

e2e: build  ## browser end-to-end: every page and tab, then the config editor (browser checks vs backend validators); records a pass
	scripts/e2e-gate.sh run

parity: build  ## differential check against the last Python backend (parity/ref.mjs: git archive + uv, offline), as CI runs it
	node parity/run.mjs --a python --b rust --allow-diff-routes parity/allow-diffs.txt
	node parity/xstate.mjs
	node parity/startup.mjs
	node parity/launcher.mjs

all: lint fmt-check typecheck test deny audit crate-age frontend-test e2e  ## every gate

# Paths go into a .desktop Exec line and a sed replacement, so the checkout path is limited to a safe character set.
install-desktop: release  ## add "Nemo Console" to your app menu (starts the console if needed, then opens the UI)
	test -x $(BIN) || { echo "$(BIN) missing: the release build failed" >&2; exit 1; }
	dir=$$(pwd -P); apps="$${XDG_DATA_HOME:-$$HOME/.local/share}/applications"; \
	case "$$dir" in *[!A-Za-z0-9._/+-]*) echo "install-desktop: move the checkout to a path of [A-Za-z0-9._/+-] only" >&2; exit 1;; esac; \
	mkdir -p "$$apps"; \
	sed -e "s|@BIN@|$$dir/$(BIN)|g" -e "s|@ICON@|$$dir/$(FRONTEND)/favicon.svg|g" \
	    deploy/nemo-console.desktop > "$$apps/nemo-console.desktop"; \
	chmod 644 "$$apps/nemo-console.desktop"; \
	echo "installed $$apps/nemo-console.desktop"

uninstall-desktop:  ## remove that launcher
	rm -f "$${XDG_DATA_HOME:-$$HOME/.local/share}/applications/nemo-console.desktop"
