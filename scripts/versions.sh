#!/usr/bin/env bash
# versions.sh — every language package in this repository ships the same
# major.minor, and this is what makes that true rather than hoped.
#
#   scripts/versions.sh          report, and fail if a manifest disagrees
#   scripts/versions.sh --list   report only
#
# The series lives in VERSION at the root. Each manifest carries its own full
# version, and only the patch part may differ: a binding can ship a fix without
# dragging the others through a release, while "which version of bizstd are you
# on" stays a question with one answer.
#
# Written in shell and reading the manifests with grep on purpose: a version
# check that needs a toolchain installed cannot run in the job that is failing
# because a toolchain is missing.
set -uo pipefail
cd "$(dirname "$0")/.."

series=$(tr -d '[:space:]' < VERSION)
[ -n "$series" ] || { echo "VERSION is empty" >&2; exit 1; }

fail=0
report() { printf '  %-28s %-12s %s\n' "$1" "$2" "$3"; }

check() {
  local label="$1" version="$2"
  [ -n "$version" ] || return 0
  local got="${version%.*}"
  if [ "$got" = "$series" ]; then
    report "$label" "$version" "ok"
  else
    report "$label" "$version" "SERIES $got, EXPECTED $series"
    fail=1
  fi
}

printf 'series %s (from VERSION)\n' "$series"

[ -f rust/Cargo.toml ] &&
  check "rust/Cargo.toml" "$(grep -m1 '^version = ' rust/Cargo.toml | cut -d'"' -f2)"

for manifest in python/*/pyproject.toml; do
  [ -f "$manifest" ] || continue
  check "$manifest" "$(grep -m1 '^version = ' "$manifest" | cut -d'"' -f2)"
done

for manifest in nodejs/*/package.json nodejs/package.json; do
  [ -f "$manifest" ] || continue
  check "$manifest" "$(grep -m1 '"version"' "$manifest" | cut -d'"' -f4)"
done

if [ "$fail" != 0 ]; then
  printf '\n\033[31ma manifest is on a different series than VERSION\033[0m\n' >&2
  printf 'Only the patch part may differ. Bump VERSION and the manifests together.\n' >&2
  exit 1
fi
printf '\033[32mevery manifest is on series %s\033[0m\n' "$series"
