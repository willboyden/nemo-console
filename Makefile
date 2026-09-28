# Nemo Console developer tasks. Nothing here touches Docker, the network or systemd.
# Backend uses uv + Python 3.13 (newer interpreters often lack wheels for some dependencies).
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
BACKEND  := backend
FRONTEND := frontend
APPS_DIR := $(HOME)/.local/share/applications

.PHONY: help setup run test lint typecheck frontend-test all install-desktop uninstall-desktop

help:  ## list targets
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/'

setup:  ## create the backend venv and install runtime + dev deps from uv.lock
	cd $(BACKEND) && uv venv --python 3.13 && uv sync --frozen

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

all: lint typecheck test frontend-test  ## every gate

install-desktop:  ## add "Nemo Console" to your app menu (starts the console if needed, then opens the UI)
	test -x $(BACKEND)/.venv/bin/nemo-console || { echo "run 'make setup' first" >&2; exit 1; }
	mkdir -p $(APPS_DIR)
	sed -e 's|@BIN@|$(CURDIR)/$(BACKEND)/.venv/bin/nemo-console|' -e 's|@ICON@|$(CURDIR)/$(FRONTEND)/favicon.svg|' \
	    deploy/nemo-console.desktop > $(APPS_DIR)/nemo-console.desktop
	chmod 644 $(APPS_DIR)/nemo-console.desktop
	@echo "installed $(APPS_DIR)/nemo-console.desktop"

uninstall-desktop:  ## remove that launcher
	rm -f $(APPS_DIR)/nemo-console.desktop
