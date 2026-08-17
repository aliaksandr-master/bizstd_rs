#!/usr/bin/env bash
# release.sh — one command that releases everything in this repository.
#
#   scripts/release.sh --dry-run   the whole thing except the uploads
#   scripts/release.sh             do it
#
# Or, from the root: `make publish` and `make publish DRY_RUN=1`.
#
# The version is never passed in on the command line. It lives in the
# manifests, it is reviewed like any other change, and the tag has to agree
# with it — a release invoked with a version typed at the prompt is a release
# nobody reviewed.
#
# Every package here shares a major and minor version, so they go out together.
# The order is the dependency order and is not adjustable: the crate is what
# the bindings build against, and inside Python the compiled package precedes
# the one that depends on it. Publishing in any other order opens a window
# where the registry holds a package whose dependency is not there yet.
#
# Publishing is irreversible. A version can be yanked, which stops new
# dependants from resolving to it, but the files stay in the registry. That is
# why every guard below refuses rather than warns, and why each language's
# publish step skips what is already there instead of failing — a release that
# cannot be re-run after a partial failure is a release that leaves the
# registry half-updated with no way forward.
set -uo pipefail
cd "$(dirname "$0")/.."

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  "") ;;
  *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
die() { printf '\033[31m%s\033[0m\n' "$1" >&2; exit 1; }

series=$(tr -d '[:space:]' < VERSION)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' rust/Cargo.toml | head -1)
[ -n "$version" ] || die "no version in rust/Cargo.toml"
tag="v$version"
branch=$(git rev-parse --abbrev-ref HEAD)

# Languages in dependency order. A language joins by having a publish.sh; this
# list is the only thing that knows the order they must go out in.
LANGUAGES="rust python nodejs"
present=""
for language in $LANGUAGES; do
  [ -x "$language/publish.sh" ] && present="$present $language"
done

step "plan"
printf 'series    %s\n' "$series"
printf 'version   %s\n' "$version"
printf 'tag       %s\n' "$tag"
printf 'branch    %s\n' "$branch"
printf 'languages %s\n' "${present:-none with a publish.sh}"
printf 'mode      %s\n' "$([ "$DRY_RUN" = 1 ] && echo 'dry run — nothing is sent' || echo 'live')"

step "guards"

git diff --quiet && git diff --cached --quiet \
  || die "the working tree is dirty; commit or stash first"

[ -z "$(git status --porcelain --untracked-files=normal)" ] \
  || die "untracked files present; they would not be in any archive but they are in your head"

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

# A tag that exists only on this machine releases something nobody can check
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

step "versions"
scripts/versions.sh || die "the manifests disagree about the version"

step "verification"
# The whole repository's checks, not this language's: a release that ships one
# package while another is broken is a release that has to be followed by an
# apology.
make dev || die "make dev failed"

for language in $present; do
  step "$language"
  if [ "$DRY_RUN" = 1 ]; then
    "$language/publish.sh" --dry-run || die "$language: the rehearsal failed"
  else
    "$language/publish.sh" || die "$language: publishing failed. Fix it and re-run;
  everything already published will be skipped."
  fi
done

if [ "$DRY_RUN" = 1 ]; then
  printf '\n\033[32mdry run finished — %s was not published anywhere\033[0m\n' "$version"
  exit 0
fi

printf '\n\033[32mbizstd %s published:%s\033[0m\n' "$version" "$present"
printf 'next: bump VERSION and every manifest together, and open the CHANGELOG section for what comes after\n'
