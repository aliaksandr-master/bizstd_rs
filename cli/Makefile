# The command-line tool. `make dev` here is what the repository's `make dev`
# calls for this language.

SHELL := /usr/bin/env bash
CARGO ?= cargo

.DEFAULT_GOAL := dev
.PHONY: dev fmt lint test install clean

dev: ## format, lints, build, tests
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(CARGO) build --release
	$(CARGO) test --release
	@printf '\n\033[32mcli: built, linted, tested\033[0m\n'

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) clippy --all-targets --all-features -- -D warnings

test:
	$(CARGO) test --release

install: ## put it on this machine's PATH
	$(CARGO) install --path .

clean:
	$(CARGO) clean
