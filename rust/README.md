# bizstd — the Rust crate

**B**lazing-fast **I**nsert **ZSTD** — an append-only container for records that
arrive continuously and are read back in bulk.

This is the reference implementation and the one every other language binding
is built on. For the format itself, the packages for other languages and the
measurements behind the choice of zstd, see [the repository
root](https://github.com/aliaksandr-master/bizstd_rs).

A file is a fixed binary preamble, a text header zone that is edited in place,
and a data section made of independent zstd frames followed by an uncompressed
tail. Records are appended to the tail as they arrive; when a period ends the
tail is compressed into a frame and the header zone is updated. Both steps are
crash-safe.

One dependency (`zstd`), no configuration, no runtime, `#![forbid(unsafe_code)]`.

```toml
[dependencies]
bizstd = "2.1"
```

## Why it exists

Continuous data has two conflicting demands. Writing wants an append with no
seek and no rewrite; reading wants compression and the ability to skip. Rolling
one file per period gives compression but leaves thousands of tiny files; one
compressed stream gives a single file but has to be read from the beginning.

bizstd splits the difference: each closed period is a self-contained zstd frame
that can be read without touching the others, while the period being written is
plain uncompressed bytes at the end of the same file. Nothing is rewritten, and
the file stays readable at every instant, including during a crash.

## The file

```text
preamble (16 bytes, binary):
  magic "BIZSTD"   6 bytes
  version          u8
  flags            u8      (bit 0: data compressed)
  header_area_len  u32 LE  (usually 4096)
  reserved         4 bytes

header zone (ASCII text inside header_area_len bytes):
  key:value\n      key `_?[a-z0-9][a-z0-9_]*`, split on the FIRST ':'
  \n               an empty line ends the headers
  zero padding to the end of the zone

data (at offset 16 + header_area_len):
  zstd frames of closed periods, then the raw uncompressed tail of the
  period being written
```

The header zone is a fixed-size region, which is what makes it editable in
place: counters, the frame list and the preview are rewritten without moving a
single byte of data.

Keys beginning with `_` are system headers and form a closed list owned by the
crate: `_schema`, `_schema_fields`, `_schema_hash`, `_record`, `_source`,
`_created_at`, `_writer`, `_compression`, `_compression_level`, `_records`,
`_bytes_raw`, `_frames`, `_preview`, `_sealed`. Every one of them is a
rebuildable cache — the data section is the truth, and `rebuild_headers` derives
them again from it. Application keys must not start with `_`, and the container
never interprets them.

## Usage

```rust,no_run
use bizstd::{Container, DEFAULT_HEADER_AREA, FieldSpec, HOT_LEVEL, RecordLayout, Schema};

fn main() -> bizstd::Result<()> {
    let schema = Schema {
        name: "samples@1".to_owned(),
        fields: vec![
            FieldSpec { name: "time_nanos".to_owned(), ty: "u64".to_owned(), offset: 0 },
            FieldSpec { name: "value".to_owned(), ty: "f64".to_owned(), offset: 8 },
        ],
        layout: RecordLayout::Fixed(16),
    };

    let mut file = Container::create(
        "samples.bizstd".as_ref(),
        &schema,
        "example source",         // _source: where the records came from
        "example writer",         // _writer: what produced the file
        0,                        // _created_at, milliseconds since the epoch
        DEFAULT_HEADER_AREA,
        &[("stream", "alpha")],   // application headers
    )?;

    let mut record = [0u8; 16];
    record[..8].copy_from_slice(&1_u64.to_le_bytes());
    record[8..].copy_from_slice(&2.5_f64.to_le_bytes());
    file.append_record(&record)?;
    file.close_frame(0, HOT_LEVEL)?;
    Ok(())
}
```

Reading back:

```rust,no_run
use bizstd::{Container, split_records};

fn main() -> bizstd::Result<()> {
    let mut file = Container::open_read("samples.bizstd".as_ref())?;
    println!("{} records in {} frames", file.records(), file.frames().len());

    for frame in file.frames().to_vec() {
        let bytes = file.read_frame(frame.id)?;
        let (records, leftover) = split_records(&bytes, file.schema().layout);
        assert_eq!(leftover, 0);
        for record in records {
            // fixed-size records, laid out by the schema
            let _ = record;
        }
    }

    // The period still being written, uncompressed.
    let _tail = file.read_tail()?;
    Ok(())
}
```

Opening a file that a writer left behind mid-period is
`Container::open_append`: it replays a complete seal journal, discards an
incomplete one, and cuts a torn record off the raw tail before handing the file
back.

Headers without reading data — the cheap way to decide whether a file is worth
opening at all:

```rust,no_run
fn main() -> bizstd::Result<()> {
    let (preamble, headers) = bizstd::peek_headers("samples.bizstd".as_ref())?;
    println!("v{} schema {:?}", preamble.version, headers.get("_schema"));
    Ok(())
}
```

## Integrity

Every frame is compressed with zstd's frame checksum enabled, so any decoder
catches a damaged frame on the way out. On top of that `_frames` records the
XXH64 of each frame's compressed bytes, which lets `validate` find damage
without decompressing and tell you which frame it is. Both are additive: a file
written before checksums existed has no fourth field in its index, reads
exactly as before, and gains one when `rebuild_headers` next runs over it.

## Compression levels

Two are named because two are used: `HOT_LEVEL` (3) closes a frame on the write
path, where the cost is paid while data is arriving, and `COLD_LEVEL` (19) is
for repacking a file offline once it stops being written to. `repack` moves a
file from one to the other and reports what it saved.

Any level `zstd` accepts works; these two are simply the ends that matter.

## Crash safety

Appending is a write to the end of the file, so a crash loses at most the
records buffered since the last flush and leaves a possibly torn record on
disk — which the next `open_append` cuts off.

Closing a frame changes two places at once, so it goes through a redo journal
(`<file>.seal`): the compressed frame and the new header zone become durable in
the journal first; then the raw tail is overwritten in place, the file is
truncated and the zone rewritten; only then is the journal removed. Interrupting
at any instant leaves either the old file or the new one, never a mixture.

## Concurrency

**One writer per file, and enforcing that is your job.** The container takes no
lock. Two writers on the same path both recover the tail, both buffer records
and both write where the other is not looking; nothing detects it and nothing
reports it, and the damage surfaces when the data is read back. Whatever you
already use to decide who owns a file — a supervisor, a lease, a directory per
process — keep using it.

Readers are unrestricted among themselves, and `Container::open_read` never
writes. A reader alongside a writer cannot corrupt anything, but it is looking
at a file in motion: the tail grows under it and closing a frame changes the
data and the header zone together. Only the frames already closed when the read
started are a stable answer.

## Maintenance

| Function | What it does |
|---|---|
| `validate` | reads every frame, checks the counters and the record alignment, optionally runs a caller's check over each record |
| `rebuild_headers` | derives the system headers from the data section and, if asked, writes them back |
| `repack` | rewrites the frames at another compression level, next to the original, then swaps atomically |
| `peek_headers` | preamble and headers only, without touching the data |

Sweeping every frame goes through `read_frame_at(index)` rather than
`read_frame(id)`: ids are the caller's, and callers repeat them — partition by
hour and a midnight spill closes under hour 0 after hour 23. `read_frame(id)`
stays for when you mean one particular frame.

Every one of them treats the data section as the truth and the headers as a
cache, which is what makes a header zone damaged by a half-written update
recoverable rather than fatal.

## Errors

Every fallible function returns `Result<T>`, which is
`std::result::Result<T, bizstd::Error>`. The variants are the distinctions a
caller can act on rather than a catalogue of everything that can go wrong:

| Variant | Means |
|---|---|
| `Io` | an operating-system failure, with the path when one is known |
| `Malformed` | the file is not a well-formed container |
| `Usage` | the calling code asked for something it may not |
| `ZoneFull` | the header zone cannot hold the headers — see below |
| `Compression` | zstd refused |

`Error` is `#[non_exhaustive]`, so a later version can name a new failure
without breaking code that matches on this one.

## The header zone fills up

The frame list lives in the header zone, and the zone has a fixed size chosen
when the file is created. Each closed frame adds an entry, so a file written to
for long enough eventually cannot record another one: `close_frame` returns
`Error::ZoneFull` and keeps returning it.

This is a property of the layout, not a bug to be worked around silently. What
to do about it:

- `max_frames_for(header_area)` says how many frames a zone of a given size
  holds, so the size can be chosen for the intended lifetime up front;
- `repack_with_header_area` rebuilds a file with a larger zone, which is the
  way out for a file that is already there;
- at the default 4096 bytes, expect a few hundred frames — comfortable for one
  frame an hour over a couple of weeks, and not enough for a year.

## Compatibility

The container version lives in the preamble. A reader refuses a version it does
not know rather than guessing. Within a version, new system headers and new
preamble flags may appear; a reader ignores flags it does not understand only
when the format says the flag is additive.

Minimum supported Rust version: **1.85**, the first release with the 2024
edition this crate is written in. It is verified on every run of the test suite
in CI, not asserted — a floor nobody builds against is a guess.

## Contributing

Issues and pull requests are welcome. The one thing worth knowing before opening
either is that `rust/check.sh` is the whole bar: format, lints with warnings as
errors, build, tests, documentation and the registry archive. `make dev` at the
repository root runs it for every language at once.

## Licence

MIT. See [LICENSE](LICENSE).
