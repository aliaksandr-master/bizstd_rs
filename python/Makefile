# The Python packages. `make dev` here is what the repository's `make dev`
# calls for this language.
#
# Everything runs inside .venv, created on demand: a language whose checks
# depend on what happens to be installed globally is a language whose checks
# mean nothing.

SHELL := /usr/bin/env bash
VENV := .venv
PY := $(VENV)/bin/python
UV ?= uv

.DEFAULT_GOAL := dev
.PHONY: dev venv build typecheck lint test clean

dev: build typecheck lint test ## build the extension, then check and test both packages
	@printf '\n\033[32mpython: extension built, typed, linted, tested\033[0m\n'

venv: $(VENV)/bin/activate

$(VENV)/bin/activate:
	$(UV) venv $(VENV)
	$(UV) pip install --python $(PY) -q "maturin>=1.7,<2" mypy pytest ruff hatchling

build: venv ## compile the extension and install both packages into the venv
	cd bizstd-binary && ../$(VENV)/bin/maturin develop --release
	$(UV) pip install --python $(PY) -q -e ./bizstd --no-deps

typecheck: venv ## mypy in strict mode over both packages and the tests
	$(VENV)/bin/mypy

lint: venv
	$(VENV)/bin/ruff check .
	$(VENV)/bin/ruff format --check .

test: venv
	$(VENV)/bin/pytest

clean:
	rm -rf $(VENV) bizstd-binary/target bizstd/dist .mypy_cache .pytest_cache .ruff_cache
