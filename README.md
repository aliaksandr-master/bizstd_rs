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

## What a file looks like

```text
┌──────────────────────────────────────────────────────────────────────┐
│ PREAMBLE                                                   16 bytes  │
│ binary, fixed size, never moves                                      │
├──────────────────────────────────────────────────────────────────────┤
│ HEADER ZONE                             header_area bytes, usually   │
│ ASCII text, rewritten in place                              4096     │
│                                                                      │
│   _schema:samples@1                                                  │
│   _record:fixed:16                                                   │
│   _frames:0,0,443,69ab…;1,443,446,14a0…                              │
│   stream:alpha                     ← yours, no underscore            │
│   ⏎                                ← an empty line ends the headers  │
│   \0\0\0 … zero padding to the end of the zone                       │
├──────────────────────────────────────────────────────────────────────┤
│ DATA                                        from 16 + header_area    │
│                                                                      │
│   ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌──────────────────┐  │
│   │  frame 0   │ │  frame 1   │ │  frame 2   │ │  raw tail        │  │
│   │  zstd      │ │  zstd      │ │  zstd      │ │  uncompressed,   │  │
│   │  closed    │ │  closed    │ │  closed    │ │  being written   │  │
│   └────────────┘ └────────────┘ └────────────┘ └──────────────────┘  │
│    ↑ offsets and lengths live in _frames          ↑ everything after │
│                                                     the last frame   │
└──────────────────────────────────────────────────────────────────────┘
```

Three properties follow from that shape, and they are the whole design:

- **the header zone has a fixed size**, so counters, the frame list and the
  preview are rewritten without moving a byte of data;
- **each frame decompresses on its own**, so a reader reaches the part it wants
  without reading what comes before;
- **the tail is plain bytes**, so appending is one write with no seek — and the
  file is a valid file at every instant, including halfway through a crash.

### The preamble, byte by byte

```text
offset  size  field            value
   0     6    magic            "BIZSTD"
   6     1    version          1
   7     1    flags            bit 0: the data section holds compressed frames
   8     4    header_area_len  u32 little-endian, usually 4096
  12     4    reserved         zero
```

Everything in the preamble is fixed-width and never moves, which is what lets a
reader decide whether a file is worth opening from its first sixteen bytes.

A reader refuses a `version` it does not know rather than guessing: reading an
unknown layout as if it were familiar corrupts silently, which is worse than
failing.

### Every header the container owns

Keys beginning with `_` are the container's, and the list is closed. **Every one
of them is a cache**: the data section is the truth, and `rebuild_headers`
derives them again from it.

| Header | Holds |
|---|---|
| `_schema` | the schema's name, `name@version` by convention |
| `_schema_fields` | `name:type:offset` for each field, `;`-separated |
| `_schema_hash` | FNV-1a 64 over the field list — a fingerprint, not a signature |
| `_record` | `fixed:<size>` or `prefixed` — how records are delimited |
| `_source` | where the records came from, free text |
| `_writer` | what produced the file, free text |
| `_created_at` | RFC 3339 UTC, when the file was created |
| `_compression` | `zstd` or `none` |
| `_compression_level` | the level the last write used; `0` means stored plainly |
| `_records` | records in closed frames — the tail is not counted |
| `_bytes_raw` | uncompressed bytes in closed frames |
| `_frames` | the index: `id,offset,len,xxh64;…`, the checksum optional |
| `_preview` | the first few records rendered, for looking at without decoding |
| `_sealed` | `true` when the writer finished with the file |

**Anything without a leading underscore is yours.** The container stores it,
never reads it, and hands it back. Keys are `[a-z0-9][a-z0-9_]*`; values escape
exactly two characters, `\n` and `\\`.

A value's round trip is byte-identical, so a header written by one version and
read by another comes back the same.

## Packages

| Language | Package | Install |
|---|---|---|
| Rust | [`bizstd`](https://crates.io/crates/bizstd) | `cargo add bizstd` |
| Python | [`bizstd`](https://pypi.org/project/bizstd/) (pure) + [`bizstd-binary`](https://pypi.org/project/bizstd-binary/) (compiled) | `pip install bizstd` |
| Node.js | [`bizstd`](https://www.npmjs.com/package/bizstd) | `npm install bizstd` |
| Command line | [`bizstd-cli`](https://crates.io/crates/bizstd-cli) | `cargo install bizstd-cli` |

Rust and Python are published. The Node package is built and tested and has not
been published yet; the table says so rather than pretending.

**Every package in this repository shares a major and minor version.** Only the
patch part may differ, so a binding can ship a fix without dragging the others
through a release while "which version of bizstd are you on" keeps one answer.
The series lives in [`VERSION`](VERSION) and `make versions` fails if a manifest
has drifted off it.

## The command line

```bash
cargo install bizstd-cli
```

Seven commands, and what separates them is what they do to a file rather than
how they are built.

### Looking

```bash
bizstd inspect day.bizstd
```

```text
day.bizstd
  size          5.3 KiB
  format        version 1, header zone 4096 B, flags 0b01
  compression   zstd (level 3)
  sealed        true

schema  samples@1  (fixed, 17 B per record)
     0  u64        time_nanos
     8  f64        value
    16  u8         flags

contents
  frames        3
  records       300 closed
  raw bytes     5.0 KiB in frames, 0 B in the tail
  stored        1.3 KiB (3.82x of the raw)

frames
     #     id       offset         len   checksum
     0      0            0         443   69abc94d8f4f4983
     1      1          443         446   14a07cdcbf90c868
     2      2          889         446   5da0ea0b0bdd4e68

system headers
  _schema              samples@1
  …

application headers
  stream               alpha

first records
  time_nanos=1700000000000000000  value=100.5  flags=0
```

`inspect` is written to be read. Everything else is written to be piped.

### Converting

```bash
bizstd try-json day.bizstd | jq 'select(.value > 100)'
bizstd try-csv  day.bizstd > day.csv
bizstd meta-json day.bizstd | jq '.headers._schema'
```

**Nothing but the format reaches standard output.** No progress, no summary, no
banner — every note goes to standard error. A tool that has to be told not to
corrupt its own output is a tool nobody pipes twice.

`try-json` writes one object per line and `try-csv` one row per record, both
using the field names and types the file's own schema declares. `--limit N`
stops early; `--no-header` drops the CSV header row.

Integers past 2^53 come out as strings. Every JSON reader that parses into a
double rounds them silently, and a nanosecond timestamp missing its last digits
still looks perfectly reasonable.

### Checking and repairing

```bash
bizstd verify day.bizstd    # 0 sound · 1 problems found · 2 unreadable
bizstd fix    day.bizstd
```

The exit code is the answer, which is what makes `verify` usable from a script
without parsing its prose:

| Code | Means |
|---|---|
| `0` | sound |
| `1` | the file was read and something is wrong |
| `2` | the file could not be read at all |

That last distinction matters: a script that treats "corrupted" and "not there"
the same way will one day delete the wrong thing.

`fix` derives the system headers from the data and writes them back, then
verifies. When the headers were repaired and the data was not, it says so
instead of reporting success.

### Rewriting

```bash
bizstd rebuild day.bizstd --level 19               # for a file that stopped growing
bizstd rebuild day.bizstd --level 0                # store the bytes plainly
bizstd rebuild day.bizstd --header-area 65536      # room for more frames
```

Level 0 is worth one warning, and the tool prints it: the frame index can no
longer be rebuilt from the data, because raw frames do not announce their own
length the way zstd frames do.

## Layout

```
rust/         the reference implementation; everything else binds to it
cli/          the command-line tool
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

## Thanks, AI

This library was written by a person. It was **released** with the help of an
AI assistant, and that is a distinction worth being precise about, because the
second part is the one that usually does not happen.

The interesting work — the format, the crash-safety argument, the decision to
put the frame index in an editable text zone — is the part anyone enjoys. What
stands between that and a package other people can install is a long tail of
work nobody enjoys: splitting one file into a crate, writing the README twice
over, choosing a licence, wiring seven build targets, chasing a wheel that
builds but will not import, writing the changelog, filling in the release notes,
measuring the thing honestly enough to publish numbers that do not flatter it.

None of it is hard. All of it is tedious, and it arrives exactly when the fun
has run out. That is why so much good code sits in a directory called `old/`
and never becomes a package — not because the author lost interest in the
problem, but because they lost interest in the paperwork.

An assistant is unreasonably good at precisely that. It does not get bored of
build matrices. It will write the same explanation for the fourth audience
without complaining. It will run the benchmark again because the first numbers
were measured on a busy machine.

So: the format is a person's. The release is a collaboration. If that is what it
takes to get more good code out of more `old/` directories and onto a registry,
it seems a fair trade.

## Licence

MIT. See [LICENSE](LICENSE).
