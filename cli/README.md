# bizstd — the command line

```bash
cargo install bizstd-cli
```

Installs a `bizstd` binary for working with container files.

## Commands

```
bizstd rebuild <file> [--level N] [--header-area N]
bizstd verify <file>
bizstd fix <file>
bizstd try-json <file> [--limit N]
bizstd meta-json <file>
```

### rebuild

Re-encodes every frame at another compression level, keeping the frame
boundaries. Level 3 by default.

```bash
bizstd rebuild archive.bizstd --level 19   # for a file that has stopped growing
bizstd rebuild archive.bizstd --level 0    # store the bytes with no compression
```

`--header-area` widens the header zone at the same time, which is what a file
that has run out of room for another frame needs.

Level 0 is worth one warning, and the tool prints it: the frame index can no
longer be reconstructed from the data, because raw frames do not announce their
own length the way zstd frames do. `fix` still repairs the counters; it cannot
recover the boundaries.

### verify and fix

`verify` reads every frame, checks the checksums, the counters and the record
alignment. The exit code is the answer:

| Code | Means |
|---|---|
| 0 | sound |
| 1 | the file was read and something is wrong |
| 2 | the file could not be read at all |

That distinction is the point: a script that treats "corrupted" and "not there"
the same way will one day delete the wrong thing.

`fix` derives the system headers from the data and writes them back, then
verifies. It repairs a file whose counters or frame list disagree with its
bytes — and says so plainly when the headers were repaired and the data was
not, rather than reporting success.

### try-json and meta-json

```bash
bizstd try-json day.bizstd | jq 'select(.value > 100)'
bizstd meta-json day.bizstd | jq '.headers._schema'
```

**Nothing but JSON reaches standard output.** No progress, no summary, no
banner — anything worth saying goes to standard error. A tool that has to be
told not to corrupt its own output is a tool nobody pipes twice.

`try-json` prints one object per line, with the field names and types the file's
own schema declares. `meta-json` prints one object with the preamble, every
header and the frame index.

Integers that a JSON number cannot carry exactly — anything past 2^53, which
includes every nanosecond timestamp — are printed as strings. A reader that
parses into a double would otherwise round them silently, and a timestamp
missing its last digits looks perfectly reasonable.

## Exit codes and pipes

`try-json` closing early because of `| head` is not an error and does not say
so. Every other failure prints to standard error and exits non-zero.

MIT. Source, the format and the benchmarks: https://github.com/aliaksandr-master/bizstd_rs
