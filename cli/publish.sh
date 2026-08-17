#!/usr/bin/env bash
# publish.sh — put the command-line tool in the registry.
#
#   cli/publish.sh --dry-run   rehearse, send nothing
#   cli/publish.sh             do it
#
# Called by scripts/release.sh, which owns the guards that apply to the whole
# repository. What is here is only what is specific to this language.
#
# Re-running after a partial release is safe: a version already in the registry
# is skipped rather than treated as a failure. A one-command release that
# cannot be run twice is a one-command release nobody dares run once.
set -uo pipefail
cd "$(dirname "$0")"

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

step() { printf '\n\033[1m-- cli: %s\033[0m\n' "$1"; }
die() { printf '\033[31mcli: %s\033[0m\n' "$1" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || die "no version in Cargo.toml"

step "registry state"
# One request, no credentials. If the version is there, this run has nothing to
# do — which is the normal case when a later language failed and the release is
# being repeated.
if curl -fsS --max-time 15 "https://crates.io/api/v1/crates/bizstd-cli/$version" \
     -H "User-Agent: bizstd-release" >/dev/null 2>&1; then
  printf 'bizstd-cli %s is already published, skipping\n' "$version"
  exit 0
fi
printf 'bizstd-cli %s is not in the registry yet\n' "$version"

step "rehearsal"
cargo publish --dry-run || die "the registry would reject this"

if [ "$DRY_RUN" = 1 ]; then
  printf 'dry run: bizstd-cli %s was not published\n' "$version"
  exit 0
fi

step "publish"
[ -n "${CARGO_REGISTRY_TOKEN:-}" ] || [ -f "${CARGO_HOME:-$HOME/.cargo}/credentials.toml" ] \
  || die "no registry credentials: export CARGO_REGISTRY_TOKEN or run 'cargo login'"
cargo publish || die "publish failed"

# Nothing builds against this one, so there is nothing to wait for.
printf 'bizstd-cli %s published\n' "$version"
