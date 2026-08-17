# Why zstd, and what it costs

The container compresses each frame with zstd. This directory is why, measured
rather than asserted, on data anyone can regenerate.

The numbers are in [RESULTS.md](RESULTS.md); `make bench` produces it. What
follows is the reading of them, including the two places where the choice
loses.

**zstd is not the smallest and not the fastest.** It is the only one of the
candidates that is close to both at once, and that is the whole argument.

## The measurement

2,000,000 generated records, 61 MiB as fixed 32-byte records, 164 MiB as JSONL.
The generator is deterministic from a seed: timestamps march in irregular
steps, values tick around a drifting level, categorical fields come from a
small set and neighbours usually share them. Real continuously-arriving data
looks like that. Uniform random data does not, compresses to nothing under
every codec, and would make the comparison meaningless.

Every figure is the **best of three runs**, not the mean. The machine these
were taken on had a data collector running throughout, load average around 20
on 12 cores. A mean would have measured the neighbour as much as the code; the
best run is the one least disturbed by it. Absolute throughput on an idle
machine will be higher — the ratios between the candidates are what this is for.

## Codecs

Taking zstd-3, the level the write path uses, as the point of comparison:

| Compared with | Size | Speed |
|---|---|---|
| `lz4` | zstd is **34% smaller** (23.9 vs 36.1 MiB) | lz4 compresses 2.1x faster, decompresses 3.6x faster |
| `gzip-6` | about the same size (23.9 vs 23.1 MiB) | zstd compresses **10x faster**, decompresses 2.1x faster |
| `brotli-9` | brotli is **22% smaller** | zstd compresses **26x faster** |
| `xz-6` | xz is **46% smaller** (12.9 MiB) | zstd compresses **86x faster**, decompresses 9.5x faster |

Read down the middle column and zstd loses twice. Read across and the losses
have prices attached.

**xz wins on size and cannot be used here.** 3 MiB/s of compression on the
thread that is also accepting records means a frame close blocks the writer for
seconds. That is fine for an archive nobody is writing to and disqualifying for
the path this format exists for. It is the right answer for cold storage, and
`repack` exists partly so a file can be moved to a slower, denser level once it
stops being written — although even then zstd-19 at 3.32x is usually the better
trade than xz at 4.73x, because decompression stays at 847 MiB/s instead of
dropping to 86.

**lz4 wins on speed and costs half the disk.** At 1.69x against 2.56x, storing
a year of data means buying 50% more disk. If your bottleneck is CPU on the
write path and disk is free, lz4 is the better choice and this format will use
it the day someone asks — nothing in the container assumes zstd beyond a flag
in the preamble and a header naming the codec.

**gzip is the one with no argument left.** Same size as zstd, ten times slower
to compress, twice as slow to read. It survives on ubiquity, and inside a
container that carries its codec in a header, ubiquity is not needed.

Where the levels land: **zstd-3 on the write path** (259 MiB/s, 2.56x) and
**zstd-19 for repacking** (3.32x, at 3 MiB/s — acceptable when nothing is
waiting). zstd-9 is a poor middle: 2.70x for a 4.6x drop in write speed
against zstd-3.

## Formats

| Format | Size | Append | Slice |
|---|---:|:---:|:---:|
| jsonl | 164.6 MiB | yes | no |
| jsonl + gzip | 20.9 MiB | yes | no |
| csv + zstd-3 | 21.1 MiB | no | no |
| **parquet (zstd)** | **16.9 MiB** | no | yes |
| arrow ipc | 52.7 MiB | no | yes |
| bizstd (zstd-3) | 23.9 MiB | yes | yes |

**Parquet is 29% smaller than this format and reads faster.** If your data is
complete before you write it, use Parquet. It is a better columnar format than
anything here, it is supported everywhere, and this project has no ambition to
compete with it.

The column that decides is **append**. A Parquet file is finished when its
footer is written: adding records means rewriting the file, and a file without
a footer is not a Parquet file at all. Arrow IPC is the same. Both are formats
for data that has stopped moving.

The formats that can be appended to — JSONL and gzipped JSONL — cannot be
sliced: reaching the last hour of a 50 GiB file means decompressing the first
49. That is the trade this container refuses to make.

bizstd is the only row with **yes** in both columns, and it pays 29% more disk
than Parquet for it. Whether that is a good trade depends entirely on whether
your data is still arriving.

## What this does not measure

- **Query performance.** Parquet's column pruning and predicate pushdown are
  real and are not represented by "read every record". If you filter more than
  you scan, that gap widens in Parquet's favour.
- **Concurrent readers and writers.** Measured single-threaded, one process.
- **Large scale.** 61 MiB fits in page cache. Numbers at hundreds of gigabytes
  are dominated by the disk, not the codec.
- **Other data shapes.** Text-heavy records, high-cardinality strings and
  incompressible blobs all move these ratios. Regenerate with your own data
  before trusting any of it — that is what the seed and the generator are for.

## Reproducing

```bash
make bench                                    # the defaults above
cd benchmarks && cargo run --release -- --records 10000000 --seed 7
```

The benchmark crate depends on arrow, parquet, brotli, xz and lz4. None of them
reach the published package: the library depends on zstd alone, and this
directory exists to show what that costs.
