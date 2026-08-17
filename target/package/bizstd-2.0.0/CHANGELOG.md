# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/1.1.0/), and
the versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.0.0] - 2026-08-15

Everything below came out of using 1.0.0 against real files. Two of the changes
alter behaviour rather than adding to it, which is what makes this a major
version rather than a minor one.

### Fixed

- **`split_records` returns the body of a length-prefixed record, without its
  two-byte length prefix.** It used to hand back the prefix as part of the
  record, which shifted every field read by two: a schema declaring a field at
  offset 0 read the length instead, and timestamps came out centuries wrong.
  Callers that compensated by adding 2 to every offset must remove that
  compensation. Fixed-size layouts are unaffected.

### Added

- Frame checksums. `Frame` carries `hash`, the XXH64 of the compressed bytes,
  and `_frames` records it as an optional fourth field; frames are also
  compressed with zstd's own frame checksum enabled. `validate` reports a
  mismatch, and `rebuild_headers` fills the field in for a file written before
  it existed. An index without the field round-trips unchanged, so old files
  stay byte-identical until something rewrites them.
- `xxh64`, the hash the checksums use.
- `Container::read_frame_at`, reading a frame by its position in the list.
  Frame ids repeat legitimately — a caller partitioning by hour closes a
  midnight spill under hour 0 after hour 23 — so a sweep meaning "every frame"
  must go by position. `validate` now does.

### Changed

- **`Frame` gained a public field**, so struct literals naming every field need
  `hash` — `Frame { id, offset, len, hash: 0 }` for an unrecorded one.
- Frames written from now on carry a checksum inside the compressed stream, so
  any decoder verifies them. Files written by 1.0.0 remain readable.


## [1.0.0] - 2026-08-14

The first published version. Container format version 1.

### Added

- The container: 16-byte preamble, fixed-size editable text header zone, and a
  data section of independent zstd frames followed by an uncompressed tail.
- `Container` — create, open for reading, open for appending with recovery,
  append a record, flush, close a frame, seal a file, set application headers.
- `peek_headers` — preamble and headers without touching the data section.
- Schema in the headers: named fields with types and offsets, a record layout
  that is either fixed-size or length-prefixed, and an FNV-1a 64 fingerprint
  over the field list.
- Crash safety: closing a frame goes through a redo journal, and opening for
  append replays a complete one, discards an incomplete one, and cuts a torn
  record off the raw tail.
- Maintenance: `validate` with an optional per-record check, `rebuild_headers`
  to derive the system headers from the data, and `repack` to rewrite the
  frames at another compression level.
- `HOT_LEVEL` (3) for closing frames on the write path and `COLD_LEVEL` (19)
  for offline repacking.
- `Error`, a `#[non_exhaustive]` enum distinguishing an I/O failure, a
  malformed file, a caller mistake, a full header zone and a compression
  failure, with `Result<T>` as the crate's result type.
- `repack_with_header_area` and `max_frames_for`, the way to size and resize
  the header zone that bounds how many frames a file can hold.
- Ceilings on everything read from a file before it is used as an allocation
  size: `MAX_HEADER_AREA` for the zone and `MAX_FRAME_RAW_BYTES` for what one
  frame may decompress to.

### Known limitations

- No locking. One writer per file at a time, enforced by the caller; two
  writers on the same path corrupt it silently.

[Unreleased]: https://github.com/aliaksandr-master/bizstd_rs/compare/v2.0.0...HEAD
[2.0.0]: https://github.com/aliaksandr-master/bizstd_rs/releases/tag/v2.0.0
[1.0.0]: https://github.com/aliaksandr-master/bizstd_rs/releases/tag/v1.0.0
