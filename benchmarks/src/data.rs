//! The data every measurement runs on.
//!
//! Generated rather than shipped, from a fixed seed, so the numbers can be
//! reproduced on someone else's machine without a download and without asking
//! whose data it was.
//!
//! The shape matters more than the values. Compression ratios on continuously
//! arriving records come almost entirely from three properties, and a
//! generator that misses them flatters every codec equally and tells you
//! nothing:
//!
//! - **timestamps march**, in small irregular steps;
//! - **numbers repeat and cluster**, because a price moves in ticks around a
//!   level rather than uniformly across the range of `f64`;
//! - **categorical fields are drawn from a small set**, and neighbouring
//!   records usually share them.
//!
//! A uniformly random record set is the opposite of all three, compresses to
//! nothing under every codec, and would make this whole directory a comparison
//! of noise.

/// One generated record, 32 bytes on the wire.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Nanoseconds since an arbitrary epoch, non-decreasing.
    pub time_nanos: u64,
    /// A value that moves in ticks around a slowly drifting level.
    pub value: f64,
    /// A size, heavily skewed towards small round numbers.
    pub size: f64,
    /// Which of a small set of streams this came from.
    pub stream: u16,
    /// A bitfield; most records share a value with their neighbour.
    pub flags: u8,
    /// Padding, so the record is a round 32 bytes and the fixed layout is
    /// honest about its size.
    pub reserved: u8,
}

impl Sample {
    /// The fixed record size the container is told about.
    pub const BYTES: usize = 32;

    /// Little-endian, field order as declared.
    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0..8].copy_from_slice(&self.time_nanos.to_le_bytes());
        out[8..16].copy_from_slice(&self.value.to_le_bytes());
        out[16..24].copy_from_slice(&self.size.to_le_bytes());
        out[24..26].copy_from_slice(&self.stream.to_le_bytes());
        out[26] = self.flags;
        out[27] = self.reserved;
        out
    }
}

/// xorshift64*, so the generator is deterministic without a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A float in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        // 53 bits, the mantissa's worth, which is the only part that survives.
        ((self.next_u64() >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 { 0 } else { self.next_u64() % bound }
    }
}

/// Generates `count` records with the seed given.
///
/// The same seed always produces the same records, on any platform: the
/// generator is integer arithmetic and the floats are derived from it.
#[must_use]
pub fn generate(count: usize, seed: u64) -> Vec<Sample> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(count);

    let mut time_nanos: u64 = 1_700_000_000_000_000_000;
    // A level that drifts slowly, with the value ticking around it. Prices,
    // sensor readings and counters all behave like this, and none of them
    // behave like a uniform draw.
    let mut level: f64 = 100.0;
    let mut stream: u16 = 0;
    let mut flags: u8 = 0;

    for index in 0..count {
        // Steps of roughly a millisecond, sometimes a burst, sometimes a gap.
        let step = match rng.below(100) {
            0..=79 => rng.below(2_000_000).saturating_add(100_000),
            80..=97 => rng.below(200_000).saturating_add(1_000),
            _other => rng.below(2_000_000_000).saturating_add(10_000_000),
        };
        time_nanos = time_nanos.saturating_add(step);

        // A random walk on the level, and a tick-quantised value around it.
        level += (rng.next_f64() - 0.5) * 0.05;
        let ticks = (rng.next_f64() * 40.0) as i64 - 20;
        let value = (level * 100.0).round() / 100.0 + (ticks as f64) * 0.01;

        // Sizes cluster on round numbers; that is what makes them compress.
        let size = match rng.below(10) {
            0..=4 => f64::from(1 + u32::try_from(rng.below(10)).unwrap_or(0)),
            5..=7 => f64::from(10 * (1 + u32::try_from(rng.below(10)).unwrap_or(0))),
            _other => (rng.next_f64() * 1000.0).round() / 100.0,
        };

        // Neighbours usually share a stream and flags, occasionally switch.
        if rng.below(100) < 8 {
            stream = u16::try_from(rng.below(64)).unwrap_or(0);
        }
        if rng.below(100) < 3 {
            flags = u8::try_from(rng.below(8)).unwrap_or(0);
        }

        out.push(Sample {
            time_nanos,
            value,
            size,
            stream,
            flags,
            reserved: u8::try_from(index % 251).unwrap_or(0),
        });
    }
    out
}

/// The same records as one contiguous fixed-layout buffer.
#[must_use]
pub fn to_fixed_bytes(samples: &[Sample]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * Sample::BYTES);
    for sample in samples {
        out.extend_from_slice(&sample.to_bytes());
    }
    out
}

/// The same records as newline-delimited JSON — the format most of this data
/// arrives in and the baseline everything else is measured against.
#[must_use]
pub fn to_jsonl(samples: &[Sample]) -> Vec<u8> {
    let mut out = Vec::new();
    for sample in samples {
        let line = serde_json::json!({
            "time_nanos": sample.time_nanos,
            "value": sample.value,
            "size": sample.size,
            "stream": sample.stream,
            "flags": sample.flags,
        });
        out.extend_from_slice(line.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

/// The same records as CSV with a header.
#[must_use]
pub fn to_csv(samples: &[Sample]) -> Vec<u8> {
    let mut out = Vec::from("time_nanos,value,size,stream,flags\n");
    for sample in samples {
        out.extend_from_slice(
            format!(
                "{},{},{},{},{}\n",
                sample.time_nanos, sample.value, sample.size, sample.stream, sample.flags
            )
            .as_bytes(),
        );
    }
    out
}
