#!/usr/bin/env bash
# publish.sh — put both Python packages in the registry, in the order that
# leaves it consistent at every instant.
#
#   python/publish.sh --dry-run   build, check, send nothing
#   python/publish.sh             do it
#
# Order is not a preference. `bizstd` declares a dependency on
# `bizstd-binary`, so publishing it first opens a window where
# `pip install bizstd` resolves the dependency and finds nothing. The binary
# goes first, always.
#
# Re-running after a partial release is safe: a version already in the registry
# is skipped rather than treated as a failure.
set -uo pipefail
cd "$(dirname "$0")"

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

VENV=.venv
# Where the platform wheels are. Locally this holds whatever this machine can
# build; in a release it is the directory the CI matrix artefacts were
# downloaded into.
WHEELS=${WHEELS:-bizstd-binary/target/wheels}

step() { printf '\n\033[1m-- python: %s\033[0m\n' "$1"; }
die() { printf '\033[31mpython: %s\033[0m\n' "$1" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' bizstd/pyproject.toml | head -1)
[ -n "$version" ] || die "no version in bizstd/pyproject.toml"

# Already there? One request each, no credentials needed.
published() {
  curl -fsS --max-time 15 "https://pypi.org/pypi/$1/$2/json" -H "User-Agent: bizstd-release" >/dev/null 2>&1
}

step "build"
[ -d "$VENV" ] || die "no $VENV; run 'make dev' in this directory first"
(cd bizstd-binary && "../$VENV/bin/maturin" build --release --out target/wheels) \
  || die "the extension did not build"
# The source distribution is the fallback for a platform the matrix does not
# cover: it needs a Rust toolchain on the user's machine, which is worse than a
# wheel and much better than "no matching distribution".
(cd bizstd-binary && "../$VENV/bin/maturin" sdist --out target/wheels) \
  || die "the sdist did not build"
"$VENV/bin/python" -m build --wheel --sdist --outdir bizstd/dist bizstd >/dev/null \
  || die "the pure package did not build"

step "what is about to be published"
wheels=$(find "$WHEELS" -name '*.whl' 2>/dev/null | sort)
sdists=$(find "$WHEELS" -name '*.tar.gz' 2>/dev/null | sort)
printf '%s\n' "$wheels" "$sdists" | sed 's|.*/|  |' | awk 'NF'
platforms=$(printf '%s\n' "$wheels" | sed 's/.*-\([^-]*\)\.whl$/\1/' | sort -u | awk 'NF' | wc -l | tr -d ' ')
printf '  platforms covered: %s\n' "$platforms"

# The whole point of shipping wheels is that a user does not need a compiler.
# Publishing one platform's wheel silently turns every other platform into a
# source build, or into a failure when there is no sdist. That is a decision,
# not a detail, so it has to be made out loud.
if [ "$platforms" -lt 2 ] && [ "${ALLOW_PARTIAL_WHEELS:-0}" != 1 ]; then
  die "only $platforms platform(s) built here.
  A release from one machine covers one platform; the rest of the world gets a
  source build at best. Either publish from the CI matrix, or say so on purpose:
      make publish ALLOW_PARTIAL_WHEELS=1
  or point this at the artefacts you already have:
      make publish WHEELS=/path/to/downloaded/wheels"
fi

if [ "$DRY_RUN" = 1 ]; then
  step "rehearsal"
  "$VENV/bin/twine" check "$WHEELS"/*.whl "$WHEELS"/*.tar.gz bizstd/dist/* 2>/dev/null \
    || printf 'twine not installed, metadata not checked\n'
  printf 'dry run: nothing was uploaded\n'
  exit 0
fi

step "credentials"
# twine rather than `uv publish`: twine reads ~/.pypirc itself, so the token
# never has to be passed on a command line, put in an environment variable, or
# handled by anything between the file and the upload. `uv publish` wants it as
# an argument, which is one more place for it to end up in a shell history.
[ -n "${TWINE_PASSWORD:-}" ] || [ -f "$HOME/.pypirc" ] \
  || die "no credentials: write ~/.pypirc, or export TWINE_USERNAME=__token__ and TWINE_PASSWORD"
[ -x "$VENV/bin/twine" ] || die "twine is missing from $VENV; run 'make dev' in this directory"

# --- the compiled half first ------------------------------------------------
#
# `--skip-existing` rather than a check on the version: a version can be present
# and still be missing most of its wheels, which is exactly what happens when a
# release is cut from one machine and the matrix build catches up afterwards.
# Skipping per file lets that second pass add what is missing and leave the rest
# alone, without anyone having to work out which is which.
step "bizstd-binary $version"
"$VENV/bin/twine" upload --skip-existing "$WHEELS"/*.whl "$WHEELS"/*.tar.gz \
  || die "bizstd-binary upload failed"
for _attempt in $(seq 1 30); do
  published bizstd-binary "$version" && break
  sleep 2
done

# --- then the package that depends on it ------------------------------------
step "bizstd $version"
"$VENV/bin/twine" upload --skip-existing bizstd/dist/* || die "bizstd upload failed"

printf '\n\033[32mpython: bizstd and bizstd-binary %s published\033[0m\n' "$version"
