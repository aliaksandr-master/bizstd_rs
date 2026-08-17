# The Rust crate. `make dev` here is what the repository's `make dev` calls for
# this language, and what CI runs.

SHELL := /usr/bin/env bash
CARGO ?= cargo

CHECK_ARGS := $(if $(FULL),--full,)

.DEFAULT_GOAL := dev
.PHONY: dev fmt lint test doc package clean

dev: ## format, lints, build, tests, documentation, packaging
	@./check.sh $(CHECK_ARGS)

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) clippy --all-targets --all-features -- -D warnings

test:
	$(CARGO) test --all-features

doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps --all-features

package:
	$(CARGO) package --allow-dirty

clean:
	$(CARGO) clean
