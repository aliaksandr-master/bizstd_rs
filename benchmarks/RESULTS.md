# bizstd benchmarks

Generated data: **2000000 records**, seed `20260817`, 61 MiB as fixed 32-byte records and 164 MiB as JSONL. Every number below is the **best of 3 runs**, not the mean.

Reproduce with `make bench`, or `cargo run --release -- --records 2000000 --seed 20260817`.

## Codecs, on the fixed-layout records

This is the choice the container makes on your behalf. The input is the raw record bytes — what a frame holds before it is compressed.

| codec | size | ratio | compress | decompress |
|---|---:|---:|---:|---:|
| `zstd-1` | 26.6 MiB | 2.30x | 394 MiB/s | 902 MiB/s |
| `zstd-3` | 23.9 MiB | 2.56x | 259 MiB/s | 820 MiB/s |
| `zstd-9` | 22.6 MiB | 2.70x | 56 MiB/s | 1073 MiB/s |
| `zstd-19` | 18.4 MiB | 3.32x | 3 MiB/s | 847 MiB/s |
| `gzip-1` | 26.0 MiB | 2.35x | 194 MiB/s | 330 MiB/s |
| `gzip-6` | 23.1 MiB | 2.64x | 25 MiB/s | 396 MiB/s |
| `gzip-9` | 23.0 MiB | 2.65x | 9 MiB/s | 410 MiB/s |
| `lz4` | 36.1 MiB | 1.69x | 548 MiB/s | 2974 MiB/s |
| `brotli-4` | 21.8 MiB | 2.80x | 76 MiB/s | 235 MiB/s |
| `brotli-9` | 18.6 MiB | 3.27x | 10 MiB/s | 276 MiB/s |
| `brotli-11` | 16.5 MiB | 3.69x | 1 MiB/s | 258 MiB/s |
| `xz-6` | 12.9 MiB | 4.73x | 3 MiB/s | 86 MiB/s |
| `xz-9` | 12.9 MiB | 4.74x | 2 MiB/s | 88 MiB/s |

## Formats, the same records stored six ways

**append** means records can be added to a finished file without rewriting it, and the file stays readable while that happens. **slice** means one part can be read without decoding everything before it.

| format | size | vs jsonl | write | read | append | slice |
|---|---:|---:|---:|---:|:---:|:---:|
| jsonl | 164.6 MiB | 1.00x | 1.24 s | 0.03 s | yes | no |
| jsonl + gzip | 20.9 MiB | 7.90x | 2.55 s | 0.26 s | yes | no |
| csv + zstd-3 | 21.1 MiB | 7.79x | 0.31 s | 0.08 s | no | no |
| parquet (zstd) | 16.9 MiB | 9.75x | 0.13 s | 0.04 s | no | yes |
| arrow ipc | 52.7 MiB | 3.12x | 0.02 s | 0.02 s | no | yes |
| bizstd (zstd-3) | 23.9 MiB | 6.89x | 0.44 s | 0.08 s | yes | yes |

## Environment

- os: `macos`
- arch: `aarch64`
- parallelism reported: 12

Everything here is single-threaded on purpose: the container compresses one frame at a time on the thread that is writing, so a parallel number would describe a program nobody is running.

