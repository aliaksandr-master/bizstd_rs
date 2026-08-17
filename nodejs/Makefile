# The Node package. `make dev` here is what the repository's `make dev` calls
# for this language.

SHELL := /usr/bin/env bash

.DEFAULT_GOAL := dev
.PHONY: dev deps build typecheck test clean

dev: build typecheck test ## build the addon, check the types, run the tests
	@printf '\n\033[32mnodejs: addon built, typed, tested\033[0m\n'

deps: node_modules

node_modules: package.json
	npm install --silent
	@touch node_modules

build: deps ## compile the native addon for this machine
	npx napi build --platform --release

typecheck: deps ## tsc in strict mode over what gets published
	npx tsc --noEmit -p tsconfig.json

test: deps
	npm test

clean:
	rm -rf node_modules target npm *.node index.js index.d.ts
