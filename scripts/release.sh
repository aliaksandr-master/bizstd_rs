#!/usr/bin/env bash
# release.sh — publish the version that Cargo.toml already names.
#
#   scripts/release.sh --dry-run   print the plan, rehearse the upload, send nothing
#   scripts/release.sh             do it
#
# The version is never passed in on the command line. It lives in Cargo.toml,
# it is reviewed like any other change, and the tag has to agree with it — a
# release invoked with a version typed at the prompt is a release nobody
# reviewed.
#
# Publishing is irreversible. A version can be yanked, which stops new
# dependants from picking it up, but the files stay in the registry forever.
# That is why every guard below refuses rather than warns.
set -euo pipefail
cd "$(dirname "$0")/.."

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  "") ;;
  *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
die() { printf '\033[31m%s\033[0m\n' "$1" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || die "no version in Cargo.toml"
tag="v$version"
branch=$(git rev-parse --abbrev-ref HEAD)

step "plan"
printf 'version   %s\n' "$version"
printf 'tag       %s\n' "$tag"
printf 'branch    %s\n' "$branch"
printf 'registry  %s\n' "${CARGO_REGISTRY:-crates.io}"
printf 'mode      %s\n' "$([ "$DRY_RUN" = 1 ] && echo 'dry run — nothing is sent' || echo 'live')"

step "guards"

git diff --quiet && git diff --cached --quiet \
  || die "the working tree is dirty; commit or stash first"

[ -z "$(git status --porcelain --untracked-files=normal)" ] \
  || die "untracked files present; they would not be in the archive but they are in your head"

# In CI the checkout is detached at the tag, which is the same thing as
# "released from the tag" and is checked below; elsewhere the branch must be
# the production one.
[ "$branch" = "main" ] || [ "$branch" = "HEAD" ] || [ "$DRY_RUN" = 1 ] \
  || die "releases are cut from main, this is '$branch'"

git rev-parse -q --verify "refs/tags/$tag" >/dev/null \
  || die "tag $tag does not exist; create it with 'make tag'"

tagged=$(git rev-list -n 1 "$tag")
head=$(git rev-parse HEAD)
[ "$tagged" = "$head" ] || [ "$DRY_RUN" = 1 ] \
  || die "tag $tag points at $tagged, HEAD is $head"

# A tag that exists only on this machine publishes something nobody can check
# out afterwards. In CI the tag is by definition on the remote already, and
# `ls-remote` there would need credentials the job has no reason to hold.
if [ -z "${CI:-}" ]; then
  if remote_tag=$(git ls-remote --tags origin "refs/tags/$tag^{}" 2>/dev/null | cut -f1) && [ -n "$remote_tag" ]; then
    [ "$remote_tag" = "$tagged" ] || die "tag $tag differs between here and origin"
  else
    [ "$DRY_RUN" = 1 ] || die "tag $tag is not pushed; run 'git push origin $tag'"
  fi
fi

printf 'every guard passed\n'

step "verification"
scripts/check.sh

step "rehearsal"
# --dry-run assembles and uploads nothing, but it does everything else the real
# publish does, including refusing on a manifest the registry would reject.
cargo publish --dry-run

if [ "$DRY_RUN" = 1 ]; then
  printf '\n\033[32mdry run finished — %s was not published\033[0m\n' "$version"
  exit 0
fi

step "publish"
[ -n "${CARGO_REGISTRY_TOKEN:-}" ] || [ -f "${CARGO_HOME:-$HOME/.cargo}/credentials.toml" ] \
  || die "no registry credentials: export CARGO_REGISTRY_TOKEN or run 'cargo login'"

cargo publish

printf '\n\033[32mbizstd %s published\033[0m\n' "$version"
printf 'next: bump the version in Cargo.toml and open the CHANGELOG section for the one after it\n'
