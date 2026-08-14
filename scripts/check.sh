#!/usr/bin/env bash
# check.sh — the whole verification loop in one file, so that CI and a person
# run the same thing rather than two things that drift apart.
#
#   scripts/check.sh          format, lints, build, tests, documentation
#   scripts/check.sh --full   the above plus benchmarks, audit, deny, coverage
#
# Anything that finds a defect exits non-zero and stops the run: a check that
# reports a failure and carries on is a check people learn to scroll past.
# Benchmarks are the one exception — they record a baseline and never gate,
# because a regression threshold nobody calibrated fails on a warm afternoon.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }

step "toolchain"
rustc --version
# The pin is the source of truth; the channel manifest says whether it has
# fallen behind. Never fatal: a release day is a bad day to be blocked by curl.
pinned=$(grep -m1 '^channel' rust-toolchain.toml | cut -d'"' -f2)
latest=$(curl -fsS --max-time 10 https://static.rust-lang.org/dist/channel-rust-stable.toml 2>/dev/null \
         | grep -m1 -A2 '^\[pkg.rust\]' | grep '^version' | cut -d'"' -f2 | cut -d' ' -f1 || echo "")
if [ -n "$latest" ] && [ "$pinned" != "$latest" ]; then
  printf '\033[33mpinned %s, stable is %s\033[0m\n' "$pinned" "$latest"
fi

step "minimum supported rust version"
# The manifest promises a floor; this is what makes the promise true. Skipped
# rather than failed when the toolchain is absent - the loop still has to run
# on a machine that has only one compiler installed.
msrv=$(grep -m1 '^rust-version' Cargo.toml | cut -d\" -f2)
if rustup toolchain list 2>/dev/null | grep -q "^${msrv}"; then
  cargo "+${msrv}" build --all-targets
  cargo "+${msrv}" test --all-features
else
  printf '\033[33mtoolchain %s not installed, skipped - install it with: rustup toolchain install %s\033[0m\n' "$msrv" "$msrv"
fi

step "format"
cargo fmt --all --check

step "lints"
cargo clippy --all-targets --all-features -- -D warnings

step "build"
cargo build --all-targets

step "tests"
cargo test --all-features

step "documentation"
# The examples in the documentation are compiled by `cargo test`; this catches
# the other half — broken intra-doc links and undocumented public items.
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

step "package"
# Everything the registry would receive, assembled but not sent. Catches a
# missing file or an excluded one long before release day. `--allow-dirty`
# because this runs mid-work, when the tree is dirty by definition; the clean
# tree is demanded by scripts/release.sh, where it means something.
cargo package --quiet --allow-dirty

if [ "${1:-}" = "--full" ]; then
  # No benchmarks step: there is no benches/ directory, and `cargo bench`
  # against an empty one writes an empty baseline that looks exactly like a
  # recorded measurement. When there is something to measure the step comes
  # back, along with the numbers it produces.

  step "advisories"
  if have cargo-audit; then cargo audit; else echo "cargo-audit not installed, skipped"; fi

  step "licences and bans"
  if have cargo-deny; then cargo deny check; else echo "cargo-deny not installed, skipped"; fi

  step "coverage"
  if have cargo-llvm-cov; then cargo llvm-cov --summary-only; else echo "cargo-llvm-cov not installed, skipped"; fi
fi

printf '\n\033[32mall checks passed\033[0m\n'
