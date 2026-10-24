# Nemo Console developer tasks. Nothing here touches Docker, the network or systemd.
# Backend uses uv + Python 3.13 (newer interpreters often lack wheels for some dependencies).
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
BACKEND  := backend
FRONTEND := frontend

.PHONY: help setup hooks run test lint typecheck frontend-test e2e all install-desktop uninstall-desktop

help:  ## list targets
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/'

setup:  ## create the backend venv and install runtime + dev deps from uv.lock; enable the git hooks
	cd $(BACKEND) && uv venv --python 3.13 && uv sync --frozen
	if git rev-parse --git-dir >/dev/null 2>&1; then $(MAKE) --no-print-directory hooks; fi

hooks:  ## use the versioned git hooks in .githooks (pre-push refuses code without a recorded e2e pass)
	git config core.hooksPath .githooks

run:  ## serve API + UI on 127.0.0.1:8793 (foreground)
	cd $(BACKEND) && uv run nemo-console

test:  ## backend pytest (offline: fakes for HF and docker)
	cd $(BACKEND) && uv run pytest -q

lint:  ## ruff
	cd $(BACKEND) && uv run ruff check src tests

typecheck:  ## mypy --strict
	cd $(BACKEND) && uv run mypy src

frontend-test:  ## node --test on the pure-logic modules (no npm install needed)
	cd $(FRONTEND) && node --test tests/*.test.mjs

e2e:  ## browser end-to-end: real backend + headless Chrome, every page and tab; records a pass for the pre-push hook
	scripts/e2e-gate.sh run

all: lint typecheck test frontend-test e2e  ## every gate

# Paths go into a .desktop Exec line and a sed replacement, so the checkout path is limited to a safe character set.
install-desktop:  ## add "Nemo Console" to your app menu (starts the console if needed, then opens the UI)
	test -x $(BACKEND)/.venv/bin/nemo-console || { echo "run 'make setup' first" >&2; exit 1; }
	dir=$$(pwd -P); apps="$${XDG_DATA_HOME:-$$HOME/.local/share}/applications"; \
	case "$$dir" in *[!A-Za-z0-9._/+-]*) echo "install-desktop: move the checkout to a path of [A-Za-z0-9._/+-] only" >&2; exit 1;; esac; \
	mkdir -p "$$apps"; \
	sed -e "s|@BIN@|$$dir/$(BACKEND)/.venv/bin/nemo-console|g" -e "s|@ICON@|$$dir/$(FRONTEND)/favicon.svg|g" \
	    deploy/nemo-console.desktop > "$$apps/nemo-console.desktop"; \
	chmod 644 "$$apps/nemo-console.desktop"; \
	echo "installed $$apps/nemo-console.desktop"

uninstall-desktop:  ## remove that launcher
	rm -f "$${XDG_DATA_HOME:-$$HOME/.local/share}/applications/nemo-console.desktop"
