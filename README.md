# bizstd

**B**lazing-fast **I**nsert **ZSTD** — an append-only container for records that
arrive continuously and are read back in bulk.

A file is a fixed binary preamble, a text header zone that is edited in place,
and a data section made of independent zstd frames followed by an uncompressed
tail. Records are appended to the tail as they arrive; when a period ends the
tail is compressed into a frame and the header zone is updated. Both steps are
crash-safe, and every frame carries a checksum.

## Why it exists

Continuous data has two conflicting demands. Writing wants an append with no
seek and no rewrite; reading wants compression and the ability to skip. Rolling
one file per period gives compression but leaves thousands of tiny files; one
compressed stream gives a single file but has to be read from the beginning.

bizstd splits the difference. Each closed period is a self-contained zstd frame
that decompresses on its own, while the period being written is plain bytes at
the end of the same file. Nothing is rewritten, and the file stays readable at
every instant, including during a crash.

Whether that is the right trade for your data — and why zstd rather than gzip,
lz4 or xz — is measured rather than asserted: see [`benchmarks/`](benchmarks/),
including the cases where something else wins.

## Packages

| Language | Package | Install |
|---|---|---|
| Rust | [`bizstd`](https://crates.io/crates/bizstd) | `cargo add bizstd` |
| Python | `bizstd` (pure) + `bizstd-binary` (compiled) | `pip install bizstd` |
| Node.js | `bizstd` | `npm install bizstd` |

The Python and Node packages are being built; only the Rust crate is published
so far. This table is the plan, and it says so rather than pretending.

**Every package in this repository shares a major and minor version.** Only the
patch part may differ, so a binding can ship a fix without dragging the others
through a release while "which version of bizstd are you on" keeps one answer.
The series lives in [`VERSION`](VERSION) and `make versions` fails if a manifest
has drifted off it.

## Layout

```
rust/         the reference implementation; everything else binds to it
python/       bizstd and bizstd-binary
nodejs/       the npm package
benchmarks/   the measurements behind the claims above
scripts/      release and version checks shared by all of them
```

Each language directory carries its own tooling and its own `Makefile` with a
`dev` target. Nothing at the root knows what is inside them.

## Working on it

```bash
make dev          # every language's own verification loop, plus version checks
make dev FULL=1   # widened where a language supports it
make bench        # the format comparison
make versions     # are the manifests still on one series
```

Issues and pull requests are welcome. `make dev` is the whole bar — if it
passes, the change is reviewable.

## The file format

The byte layout, the header vocabulary and the compatibility rules are
documented in [`rust/README.md`](rust/README.md), which is also what the crate
registry shows. A reader in any language can be written from it without reading
the implementation.

## Licence

MIT. See [LICENSE](LICENSE).
