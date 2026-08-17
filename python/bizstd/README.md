# bizstd

An append-only container for records that arrive continuously and are read back
in bulk.

```bash
pip install bizstd
```

A file is a fixed binary preamble, a text header zone edited in place, and a
data section of independent zstd frames followed by an uncompressed tail.
Records are appended to the tail as they arrive; when a period ends the tail is
compressed into a frame. Both steps are crash-safe, and every frame carries a
checksum.

## Writing

```python
import bizstd

schema = bizstd.Schema(
    "samples@1",
    [
        bizstd.FieldSpec("time_nanos", "u64", 0),
        bizstd.FieldSpec("value", "f64", 8),
    ],
    bizstd.RecordLayout.fixed(16),
)

with bizstd.create("samples.bizstd", schema, source="sensor", writer="demo") as file:
    for record in incoming:
        file.append(record)
    file.close_frame(0)
```

## Reading

```python
with bizstd.open_read("samples.bizstd") as file:
    print(file.headers["_schema"], file.record_count)
    for record in file:
        ...
```

Frames are addressed by position, not by id: ids belong to the writer and
writers repeat them. `file.frame(0)` is the first frame; `file.frame_by_id(3)`
is there for when you mean one particular frame.

## Errors

Every failure is a subclass of `BizstdError`, and the subclasses are the
distinctions worth acting on:

| Exception | Means |
|---|---|
| `BizstdMalformedError` | the file is not a well-formed container |
| `BizstdUsageError` | the calling code asked for something it may not |
| `BizstdZoneFullError` | the header zone is full — repack with a larger one |
| `BizstdCompressionError` | zstd refused |
| `OSError` | the operating system said no |

## Typing

Fully typed, `py.typed` in both packages, checked with `mypy --strict`. The
extension module ships hand-written stubs that a test compares against the real
module, so they cannot drift quietly.

## Concurrency

One writer per file, and enforcing that is your job — nothing here takes a
lock, and two writers on one path corrupt it silently. Readers are
unrestricted among themselves.

## Packages

`bizstd` is pure Python and depends on `bizstd-binary`, which is the compiled
extension and nothing else. The split means a platform without a prebuilt wheel
fails with a clear message instead of hunting for a compiler, and it means this
package can be read and patched without a build matrix.

MIT. Source, the format specification and the benchmarks behind the choice of
zstd: https://github.com/aliaksandr-master/bizstd_rs
