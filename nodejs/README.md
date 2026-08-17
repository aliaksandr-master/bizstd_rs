# bizstd for Node.js

An append-only container for records that arrive continuously and are read back
in bulk.

```bash
npm install bizstd
```

A real native addon, built with napi-rs — not a runtime FFI binding. There is
no foreign-function library to install alongside it, and the per-platform
binaries ship as their own packages that npm resolves automatically. A machine
with no prebuilt binary fails at install with a clear message rather than at
the first call.

## Writing

```js
const bizstd = require('bizstd')

const schema = {
  name: 'samples@1',
  fields: [
    { name: 'timeNanos', ty: 'u64', offset: 0 },
    { name: 'value', ty: 'f64', offset: 8 },
  ],
  fixedSize: 16,
}

const file = bizstd.create('samples.bizstd', schema, { source: 'sensor', writer: 'demo' })
for (const record of incoming) file.append(record)
file.closeFrame(0)
```

## Reading

```js
const file = bizstd.openRead('samples.bizstd')
console.log(file.headers._schema, file.recordCount)
for (const record of file) {
  // one record at a time; one frame decompressed at a time
}
```

Frames are addressed by position, not by id: ids belong to the writer and
writers repeat them. `file.frame(0)` is the first frame; `file.frameById(3n)`
is there for when you mean one particular frame.

## Errors

Every failure is an instance of `BizstdError`, and the subclasses are the
distinctions worth acting on:

| Class | `code` | Means |
|---|---|---|
| `BizstdMalformedError` | `BIZSTD_MALFORMED` | the file is not a well-formed container |
| `BizstdUsageError` | `BIZSTD_USAGE` | the calling code asked for something it may not |
| `BizstdZoneFullError` | `BIZSTD_ZONE_FULL` | the header zone is full — repack with a larger one |
| `BizstdCompressionError` | `BIZSTD_COMPRESSION` | zstd refused |
| `BizstdIOError` | `BIZSTD_IO` | the operating system said no |

```js
try {
  file.closeFrame(hour)
} catch (error) {
  if (error instanceof bizstd.BizstdZoneFullError) {
    bizstd.repack(path, bizstd.HOT_LEVEL, 64 * 1024)
  }
}
```

## Types

TypeScript declarations ship with the package: generated ones for the addon,
hand-written ones for the wrapper, and a test that compares both against what
is actually exported. A declaration that has drifted is worse than none — the
checker agrees, confidently, with code that will fail.

Counts and frame fields are `bigint`, because a file can hold more records than
a double can count exactly.

## Concurrency

One writer per file, and enforcing that is your job — nothing here takes a
lock, and two writers on one path corrupt it silently. Readers are unrestricted
among themselves.

## Supported

Node 18 and up. Prebuilt binaries: Linux x86-64 and aarch64 (glibc and musl),
macOS x86-64 and arm64, Windows x86-64.

MIT. Source, the format specification and the benchmarks behind the choice of
zstd: https://github.com/aliaksandr-master/bizstd_rs
