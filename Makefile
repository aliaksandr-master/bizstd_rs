# Makefile — two commands matter, `dev` and `publish`; the rest are the steps
# they are made of, exposed so a single one can be re-run while working.
#
#   make dev              format, lints, build, tests, docs — the whole loop
#   make dev FULL=1       the above plus benchmarks, audit, deny, coverage
#   make publish DRY_RUN=1   print the release plan, send nothing
#   make publish          release the version in Cargo.toml to the registry
#
# The loop lives in scripts/check.sh so that CI and a person run the same file
# rather than two things that drift apart.

SHELL := /usr/bin/env bash
CARGO ?= cargo

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TAG := v$(VERSION)

# `make dev FULL=1` widens the loop; `make publish DRY_RUN=1` disarms it.
CHECK_ARGS := $(if $(FULL),--full,)
RELEASE_ARGS := $(if $(DRY_RUN),--dry-run,)

.DEFAULT_GOAL := help
.PHONY: help dev full fmt fmt-check lint build test doc package audit deny tag publish version clean

help: ## show this help
	@printf 'bizstd %s\n\n' '$(VERSION)'
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2}'

dev: ## the whole local verification loop (FULL=1 to widen it)
	@scripts/check.sh $(CHECK_ARGS)

full: ## shorthand for `make dev FULL=1`
	@scripts/check.sh --full

fmt: ## rewrite the source in the project's format
	$(CARGO) fmt --all

fmt-check: ## fail if the source is not formatted
	$(CARGO) fmt --all --check

lint: ## clippy, warnings are errors
	$(CARGO) clippy --all-targets --all-features -- -D warnings

build: ## build the library and every target
	$(CARGO) build --all-targets

test: ## run the test suite, including the doc tests
	$(CARGO) test --all-features

doc: ## build the documentation, warnings are errors
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps --all-features

package: ## assemble the registry archive without publishing it
	$(CARGO) package --list >/dev/null
	$(CARGO) package

audit: ## known advisories in the dependency tree
	$(CARGO) audit

deny: ## licences, duplicate versions, source allow-list
	$(CARGO) deny check

tag: ## create the annotated tag for the version in Cargo.toml
	@git diff --quiet && git diff --cached --quiet || { echo "working tree is dirty"; exit 1; }
	@git rev-parse -q --verify 'refs/tags/$(TAG)' >/dev/null && { echo "tag $(TAG) already exists"; exit 1; } || true
	git tag -a '$(TAG)' -m 'bizstd $(VERSION)'
	@printf 'created %s — push it with: git push origin %s\n' '$(TAG)' '$(TAG)'

publish: ## release the version in Cargo.toml (DRY_RUN=1 to rehearse)
	@scripts/release.sh $(RELEASE_ARGS)

version: ## print the version this checkout would publish
	@printf '%s\n' '$(VERSION)'

clean: ## remove build output
	$(CARGO) clean
	rm -rf benchmarks
