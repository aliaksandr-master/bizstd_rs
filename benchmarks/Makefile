# The measurements behind the README's claims. Not part of any published
# package: this directory exists so a reader can check rather than believe.

SHELL := /usr/bin/env bash
RECORDS ?= 2000000
SEED    ?= 20260817
REPEATS ?= 3

.DEFAULT_GOAL := run
.PHONY: run dev clean

run: ## measure and write the report
	cargo run --release -- --records $(RECORDS) --seed $(SEED) --repeats $(REPEATS) | tee README.md
	@printf '\nwritten to benchmarks/README.md\n'

dev: ## a quick pass, so `make dev` at the root stays fast
	cargo build --release
	cargo run --release -- --records 50000 --repeats 1 > /dev/null
	@printf 'benchmarks build and run\n'

clean:
	cargo clean
