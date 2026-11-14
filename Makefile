# Nemo Console developer tasks. Nothing here touches Docker or systemd; the only network use is cargo fetching crates,
# the crate-age gate asking crates.io and `make setup` installing the pinned Prettier (npm ci, frontend/package-lock.json).
# Backend: Rust (backend/, a Cargo workspace; the toolchain is pinned in backend/rust-toolchain.toml and rustup
# installs it on first use). Frontend: plain ES modules, zero runtime npm dependencies (Prettier, the only
# devDependency, formats the code; nothing from npm is served or built into the binary). One build step since
# ADR-0022: `make wasm` compiles the backend's config validators (backend/crates/nemo-console-wasm) to WebAssembly
# for the config editor, into frontend/js/wasm/ (generated, never committed). It needs the wasm32-unknown-unknown
# target (rust-toolchain.toml installs it) and wasm-bindgen-cli at exactly the version backend/Cargo.toml pins.
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
BACKEND  := backend
FRONTEND := frontend
CARGO    := cargo
BIN      := $(BACKEND)/target/release/nemo-console
PRETTIER := $(FRONTEND)/node_modules/.bin/prettier
CARGO_HOME ?= $(HOME)/.cargo
WASM_BINDGEN ?= wasm-bindgen
WASM_SRC := $(BACKEND)/target/wasm32-unknown-unknown/release-wasm/nemo_console_wasm.wasm
# The size budget of the generated module (bytes, uncompressed); `make wasm` fails above it (ADR-0022).
WASM_BUDGET := 1500000

# The WebAssembly module carries no path of the build machine (it is served to the browser): panic locations name
# source files, so the rest of $(HOME) becomes ~, the cargo home /cargo, this checkout /build (rustc uses the LAST
# matching prefix, so the longer ones come later). Cargo's trim-paths is not stable on the pinned toolchain.
REMAP := --remap-path-prefix=$(HOME)=~ --remap-path-prefix=$(CARGO_HOME)=/cargo --remap-path-prefix=$(CURDIR)=/build

.PHONY: help setup hooks build release run test lint typecheck typecheck-wasm deny audit crate-age frontend-test \
        fmt fmt-check prettier-installed e2e parity wasm all \
        install-desktop uninstall-desktop

help:  ## list targets
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/'

setup:  ## build the backend (debug) from Cargo.lock; install the pinned Prettier (npm ci); enable the git hooks
	cd $(BACKEND) && $(CARGO) build --locked
	cd $(FRONTEND) && npm ci --ignore-scripts --no-audit --no-fund
	if git rev-parse --git-dir >/dev/null 2>&1; then $(MAKE) --no-print-directory hooks; fi

hooks:  ## use the versioned git hooks in .githooks (pre-push refuses code without a recorded e2e pass)
	git config core.hooksPath .githooks

build: wasm  ## debug build of the backend (what the e2e suite runs), plus the browser validators (`wasm`)
	cd $(BACKEND) && $(CARGO) build --locked

release: wasm  ## optimised build: backend/target/release/nemo-console (what `run`, install-desktop and the service use)
	cd $(BACKEND) && $(CARGO) build --locked --release

# The config editor's live checks: the backend's own validators (nemo-console-core) compiled to WebAssembly. The
# wasm-bindgen CLI must be the exact version of the crate (backend/Cargo.toml), or the generated glue does not fit the
# module. The output goes to a staging directory first and replaces frontend/js/wasm/ only after it passed the
# build-path check and the size budget.
wasm:  ## build the browser validators into frontend/js/wasm/ (generated; needs wasm-bindgen-cli, see README)
	want=$$(sed -n 's/^wasm-bindgen = "=\([0-9.]*\)".*/\1/p' $(BACKEND)/Cargo.toml); \
	have=$$($(WASM_BINDGEN) --version 2>/dev/null | awk '{print $$2}') || have=; \
	if [ -z "$$want" ] || [ "$$want" != "$$have" ]; then \
	  echo "wasm: wasm-bindgen CLI '$${have:-missing}' must be exactly $$want: cargo install --locked wasm-bindgen-cli@$$want" >&2; \
	  exit 1; \
	fi
	cd $(BACKEND) && RUSTFLAGS="$(strip $(RUSTFLAGS) $(REMAP))" $(CARGO) build --locked -p nemo-console-wasm --lib \
	  --target wasm32-unknown-unknown --profile release-wasm
	rm -rf frontend/js/wasm.staging
	$(WASM_BINDGEN) --target web --no-typescript --out-dir frontend/js/wasm.staging $(WASM_SRC)
	for f in frontend/js/wasm.staging/*; do \
	  for p in "$(CURDIR)/" "$(HOME)/" "$(CARGO_HOME)/"; do \
	    if grep -aqF "$$p" "$$f"; then echo "wasm: $$f contains the build path $$p" >&2; exit 1; fi; \
	  done; \
	done
	size=$$(wc -c < frontend/js/wasm.staging/nemo_console_wasm_bg.wasm); \
	gz=$$(gzip -9c frontend/js/wasm.staging/nemo_console_wasm_bg.wasm | wc -c); \
	if [ "$$size" -gt $(WASM_BUDGET) ]; then echo "wasm: $$size bytes is over the $(WASM_BUDGET)-byte budget" >&2; exit 1; fi; \
	rm -rf frontend/js/wasm; \
	mv frontend/js/wasm.staging frontend/js/wasm; \
	echo "wasm: frontend/js/wasm/nemo_console_wasm_bg.wasm $$size bytes ($$gz gzip -9), budget $(WASM_BUDGET), no build path inside"

run: release  ## serve API + UI on 127.0.0.1:8793 (foreground)
	$(BIN)

test:  ## backend tests (offline: fakes for every CLI and upstream; live smoke tests run only with NEMO_LIVE=1)
	cd $(BACKEND) && $(CARGO) test --locked --workspace

lint:  ## rustfmt check
	cd $(BACKEND) && $(CARGO) fmt --all --check

typecheck: typecheck-wasm  ## clippy, warnings are errors (the workspace lints in backend/Cargo.toml), native and wasm32
	cd $(BACKEND) && $(CARGO) clippy --locked --workspace --all-targets -- -D warnings

typecheck-wasm:  ## clippy on the two crates the browser build compiles, for wasm32-unknown-unknown
	cd $(BACKEND) && $(CARGO) clippy --locked -p nemo-console-core -p nemo-console-wasm --target wasm32-unknown-unknown -- -D warnings

deny:  ## licenses, bans, sources and advisories (cargo-deny, backend/deny.toml)
	cd $(BACKEND) && $(CARGO) deny check

audit:  ## RustSec advisories for Cargo.lock (cargo-audit)
	cd $(BACKEND) && $(CARGO) audit

crate-age:  ## dependency cooldown: every locked crate at least 14 days old, security fixes excepted
	python3 -I $(BACKEND)/tools/crate-age.py

frontend-test: wasm  ## node --test on the pure-logic modules (no npm install; the live-check tests load the wasm)
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
