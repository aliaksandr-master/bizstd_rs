//! Codec comparison: the same bytes through every general-purpose compressor
//! the container could plausibly have been built on.
//!
//! What is being measured is not "which codec is best" — there is no such
//! thing — but what choosing zstd costs against each alternative on data of
//! this shape, so that a reader can decide whether the trade fits theirs.

use std::io::Write as _;
use std::time::Instant;

/// One codec's result on one input.
pub struct Measurement {
    /// Codec and level, as a reader would name it.
    pub name: String,
    /// Bytes after compression.
    pub compressed: usize,
    /// Compression throughput, MiB/s of input.
    pub compress_mib_s: f64,
    /// Decompression throughput, MiB/s of output.
    pub decompress_mib_s: f64,
}

impl Measurement {
    /// Uncompressed bytes per compressed byte. Higher is smaller on disk.
    #[must_use]
    pub fn ratio(&self, raw: usize) -> f64 {
        if self.compressed == 0 {
            return 0.0;
        }
        raw as f64 / self.compressed as f64
    }
}

/// Runs one codec and reports what it did.
///
/// Each direction is repeated and the **best** time is kept, not the mean.
/// This machine has other work on it, and a mean measures the neighbour as
/// much as the code; the best run is the one least disturbed by it. The report
/// says so out loud rather than presenting the numbers as a clean-room result.
fn measure(
    name: &str,
    raw: &[u8],
    repeats: usize,
    compress: impl Fn(&[u8]) -> Vec<u8>,
    decompress: impl Fn(&[u8]) -> Vec<u8>,
) -> Measurement {
    let mib = raw.len() as f64 / (1024.0 * 1024.0);

    let mut compressed = Vec::new();
    let mut best_compress = f64::MAX;
    for _round in 0..repeats {
        let started = Instant::now();
        compressed = compress(raw);
        best_compress = best_compress.min(started.elapsed().as_secs_f64());
    }

    let mut best_decompress = f64::MAX;
    for _round in 0..repeats {
        let started = Instant::now();
        let back = decompress(&compressed);
        best_decompress = best_decompress.min(started.elapsed().as_secs_f64());
        assert_eq!(back.len(), raw.len(), "{name}: round trip changed the length");
    }

    Measurement {
        name: name.to_owned(),
        compressed: compressed.len(),
        compress_mib_s: if best_compress > 0.0 { mib / best_compress } else { 0.0 },
        decompress_mib_s: if best_decompress > 0.0 { mib / best_decompress } else { 0.0 },
    }
}

/// Every codec, on one input.
pub fn run_all(raw: &[u8], repeats: usize) -> Vec<Measurement> {
    let mut out = Vec::new();

    for level in [1, 3, 9, 19] {
        out.push(measure(
            &format!("zstd-{level}"),
            raw,
            repeats,
            |bytes| zstd::stream::encode_all(bytes, level).unwrap_or_default(),
            |bytes| zstd::stream::decode_all(bytes).unwrap_or_default(),
        ));
    }

    for level in [1u32, 6, 9] {
        out.push(measure(
            &format!("gzip-{level}"),
            raw,
            repeats,
            |bytes| {
                let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
                let _ignored = encoder.write_all(bytes);
                encoder.finish().unwrap_or_default()
            },
            |bytes| {
                let mut out = Vec::new();
                let mut decoder = flate2::write::GzDecoder::new(&mut out);
                let _ignored = decoder.write_all(bytes);
                let _ignored = decoder.finish();
                out
            },
        ));
    }

    out.push(measure(
        "lz4",
        raw,
        repeats,
        |bytes| lz4_flex::block::compress_prepend_size(bytes),
        |bytes| lz4_flex::block::decompress_size_prepended(bytes).unwrap_or_default(),
    ));

    for quality in [4u32, 9, 11] {
        out.push(measure(
            &format!("brotli-{quality}"),
            raw,
            repeats,
            |bytes| {
                let mut out = Vec::new();
                let mut encoder = brotli::CompressorWriter::new(&mut out, 4096, quality, 22);
                let _ignored = encoder.write_all(bytes);
                drop(encoder);
                out
            },
            |bytes| {
                let mut out = Vec::new();
                let mut decoder = brotli::DecompressorWriter::new(&mut out, 4096);
                let _ignored = decoder.write_all(bytes);
                drop(decoder);
                out
            },
        ));
    }

    for level in [6u32, 9] {
        out.push(measure(
            &format!("xz-{level}"),
            raw,
            repeats,
            |bytes| {
                let mut encoder = xz2::write::XzEncoder::new(Vec::new(), level);
                let _ignored = encoder.write_all(bytes);
                encoder.finish().unwrap_or_default()
            },
            |bytes| {
                let mut decoder = xz2::write::XzDecoder::new(Vec::new());
                let _ignored = decoder.write_all(bytes);
                decoder.finish().unwrap_or_default()
            },
        ));
    }

    out
}
