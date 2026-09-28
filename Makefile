# Nemo Console developer tasks. Nothing here touches Docker, the network or systemd.
# Backend uses uv + Python 3.13 (newer interpreters often lack wheels for some dependencies).
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
BACKEND  := backend
FRONTEND := frontend

.PHONY: help setup run test lint typecheck frontend-test all

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
