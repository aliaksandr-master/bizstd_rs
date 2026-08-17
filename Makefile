# Makefile — the top of a repository that holds one format and several
# implementations of it. Each language lives in its own directory with its own
# tooling; this file knows which directories exist and nothing about what is
# inside them.
#
#   make dev              every language's own verification loop, plus versions
#   make dev FULL=1       the above, widened where a language supports it
#   make versions         are all the manifests on the same major.minor
#   make bench            the format comparison behind the README's claims
#   make tag              annotated tag for the current version
#   make publish DRY_RUN=1   the whole release except the uploads
#   make publish          release every language, one command
#
# A language is added by creating its directory and giving it a Makefile with a
# `dev` target. Nothing here needs to change for that.

SHELL := /usr/bin/env bash

# Directories that carry a Makefile of their own, in the order a release needs:
# the Rust crate is what the others bind to, so it goes first.
LANGUAGES := rust cli python nodejs
PRESENT := $(foreach dir,$(LANGUAGES),$(if $(wildcard $(dir)/Makefile),$(dir),))

SERIES := $(shell tr -d '[:space:]' < VERSION)

.DEFAULT_GOAL := help
.PHONY: help dev versions bench tag publish clean $(PRESENT)

help: ## show this help
	@printf 'bizstd, series %s\n\n' '$(SERIES)'
	@printf 'languages present: %s\n\n' '$(if $(PRESENT),$(PRESENT),none yet)'
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2}'

dev: versions ## run every language's verification loop
	@for dir in $(PRESENT); do \
	  printf '\n\033[1m######## %s\033[0m\n' "$$dir"; \
	  $(MAKE) --no-print-directory -C "$$dir" dev FULL=$(FULL) || exit 1; \
	done
	@printf '\n\033[32mevery language passed\033[0m\n'

versions: ## fail if a manifest is on a different major.minor
	@scripts/versions.sh

bench: ## measure this format against the alternatives
	@$(MAKE) --no-print-directory -C benchmarks run

tag: ## annotated tag for the version the manifests name
	@version=$$(sed -n 's/^version = "\(.*\)"/\1/p' rust/Cargo.toml | head -1); \
	git diff --quiet && git diff --cached --quiet || { echo "working tree is dirty"; exit 1; }; \
	git rev-parse -q --verify "refs/tags/v$$version" >/dev/null && { echo "tag v$$version already exists"; exit 1; }; \
	git tag -a "v$$version" -m "bizstd $$version"; \
	printf 'created v%s - push it with: git push origin v%s\n' "$$version" "$$version"

publish: ## release every language at the current version, in dependency order
	@scripts/release.sh $(if $(DRY_RUN),--dry-run,)

clean: ## remove build output everywhere
	@for dir in $(PRESENT) benchmarks; do \
	  [ -f "$$dir/Makefile" ] && $(MAKE) --no-print-directory -C "$$dir" clean || true; \
	done
