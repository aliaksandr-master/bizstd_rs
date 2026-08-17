#!/usr/bin/env bash
# publish.sh — put the npm package in the registry.
#
#   nodejs/publish.sh --dry-run   build, check, send nothing
#   nodejs/publish.sh             do it
#
# Called by scripts/release.sh, which owns the guards that apply to the whole
# repository.
#
# Re-running after a partial release is safe: a version already in the registry
# is skipped rather than treated as a failure.
set -uo pipefail
cd "$(dirname "$0")"

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

# Where the per-platform addons are. Locally this holds whatever this machine
# can build; in a release it is where the CI matrix artefacts were downloaded.
ARTEFACTS=${ARTEFACTS:-artifacts}

step() { printf '\n\033[1m-- nodejs: %s\033[0m\n' "$1"; }
die() { printf '\033[31mnodejs: %s\033[0m\n' "$1" >&2; exit 1; }

version=$(node -p "require('./package.json').version" 2>/dev/null)
[ -n "$version" ] || die "no version in package.json"

step "registry state"
if npm view "bizstd@$version" version >/dev/null 2>&1; then
  printf 'bizstd %s is already published, skipping\n' "$version"
  exit 0
fi
printf 'bizstd %s is not in the registry yet\n' "$version"

step "platform packages"
# napi turns the downloaded addons into one npm package per platform, wired to
# the main package as optionalDependencies. Without this step the published
# package has no binary for anything.
if [ -d "$ARTEFACTS" ]; then
  npx napi create-npm-dirs >/dev/null 2>&1
  npx napi artifacts --npm-dir npm --output-dir "$ARTEFACTS" || die "could not place the artefacts"
else
  printf '\033[33mno %s directory: only this machine'\''s binary will be published\033[0m\n' "$ARTEFACTS"
  [ "${ALLOW_PARTIAL_BINARIES:-0}" = 1 ] || die "a package with one platform'\''s binary fails to install everywhere else.
  Point this at the matrix build:
      make publish ARTEFACTS=/path/to/downloaded
  or say so on purpose:
      make publish ALLOW_PARTIAL_BINARIES=1"
fi
ls npm/*/ 2>/dev/null | head -20

if [ "$DRY_RUN" = 1 ]; then
  step "rehearsal"
  npm pack --dry-run >/dev/null || die "npm would reject this package"
  printf 'dry run: nothing was uploaded\n'
  exit 0
fi

step "credentials"
npm whoami >/dev/null 2>&1 || die "not logged in: run 'npm login'"

step "publish"
# The platform packages first: the main package lists them as optional
# dependencies, and publishing it first leaves installs resolving names that do
# not exist yet.
for dir in npm/*/; do
  [ -f "$dir/package.json" ] || continue
  name=$(node -p "require('./$dir/package.json').name")
  if npm view "$name@$version" version >/dev/null 2>&1; then
    printf '  %s already published\n' "$name"
  else
    (cd "$dir" && npm publish --access public) || die "$name upload failed"
  fi
done

npm publish --access public || die "bizstd upload failed"
printf '\n\033[32mnodejs: bizstd %s published\033[0m\n' "$version"
