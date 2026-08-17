//! The native half of the Node package.
//!
//! Like the Python binding, this layer only moves values across the boundary
//! and turns each error variant into something the caller can branch on.
//! Iteration, the friendly names and the typed error classes live in the
//! JavaScript wrapper, where they can be read and changed without a rebuild.
//!
//! Every error carries a `code` that JavaScript can switch on, because the
//! whole reason the Rust side has an error enum is that a full header zone and
//! a corrupted file call for different responses, and flattening them into one
//! message at the boundary throws that away.

#![deny(clippy::all)]

use std::path::PathBuf;

use bizstd::{Container as Inner, Error as Fault, RecordLayout};
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Maps an error variant onto a JS error whose `code` names the variant.
fn to_js(fault: &Fault) -> Error {
    let code = match fault {
        Fault::Io { .. } => "BIZSTD_IO",
        Fault::Malformed(_) => "BIZSTD_MALFORMED",
        Fault::Usage(_) => "BIZSTD_USAGE",
        Fault::ZoneFull { .. } => "BIZSTD_ZONE_FULL",
        Fault::Compression(_) => "BIZSTD_COMPRESSION",
        _other => "BIZSTD_ERROR",
    };
    Error::new(Status::GenericFailure, format!("{code}: {fault}"))
}

fn map<T>(result: bizstd::Result<T>) -> Result<T> {
    result.map_err(|fault| to_js(&fault))
}

/// One field of a schema.
#[napi(object)]
pub struct FieldSpec {
    /// The field's name.
    pub name: String,
    /// Its type as the format spells it: `u8`, `u16`, `u32`, `u64`, `i64`,
    /// `f64` or `uuid`.
    pub ty: String,
    /// Byte offset inside the record.
    pub offset: u32,
}

/// What the records in a file look like.
///
/// `fixedSize` decides the layout: a number means records of exactly that many
/// bytes, and leaving it out means each record is preceded by a little-endian
/// `u16` length.
#[napi(object)]
pub struct Schema {
    /// The schema's name, `name@version` by convention.
    pub name: String,
    /// Its fields, in declaration order.
    pub fields: Vec<FieldSpec>,
    /// Bytes per record, or absent for length-prefixed records.
    pub fixed_size: Option<u32>,
}

impl From<&Schema> for bizstd::Schema {
    fn from(value: &Schema) -> Self {
        Self {
            name: value.name.clone(),
            fields: value
                .fields
                .iter()
                .map(|field| bizstd::FieldSpec {
                    name: field.name.clone(),
                    ty: field.ty.clone(),
                    offset: field.offset,
                })
                .collect(),
            layout: value.fixed_size.map_or(RecordLayout::Prefixed, RecordLayout::Fixed),
        }
    }
}

/// One closed frame in the data section.
#[napi(object)]
pub struct Frame {
    /// The id the writer gave it. Ids may repeat.
    pub id: BigInt,
    /// Byte offset from the start of the data section.
    pub offset: BigInt,
    /// Compressed length.
    pub len: BigInt,
    /// XXH64 of the compressed bytes, `0n` when the file predates checksums.
    pub hash: BigInt,
}

impl From<bizstd::Frame> for Frame {
    fn from(value: bizstd::Frame) -> Self {
        Self {
            id: BigInt::from(value.id),
            offset: BigInt::from(value.offset),
            len: BigInt::from(value.len),
            hash: BigInt::from(value.hash),
        }
    }
}

/// The fixed binary head of a file.
#[napi(object)]
pub struct Preamble {
    /// Container format version.
    pub version: u8,
    /// Flag bits; bit 0 means the data section is compressed.
    pub flags: u8,
    /// Size of the header zone in bytes.
    pub header_area: u32,
}

/// What `validate` found.
#[napi(object)]
pub struct ValidateReport {
    /// Everything wrong, empty when the file is sound.
    pub problems: Vec<String>,
    /// Closed frames checked.
    pub frames: u32,
    /// Records seen in frames and the tail.
    pub records: BigInt,
}

/// What `rebuildHeaders` found and did.
#[napi(object)]
pub struct RebuildReport {
    /// Header values that disagreed with the data.
    pub differences: Vec<String>,
    /// Whether the zone was rewritten.
    pub fixed: bool,
}

/// What `repack` measured.
#[napi(object)]
pub struct RepackReport {
    /// File size before, bytes.
    pub bytes_before: BigInt,
    /// File size after, bytes.
    pub bytes_after: BigInt,
    /// Frames re-encoded.
    pub frames: u32,
}

/// Preamble and headers, without touching the data section.
#[napi(object)]
pub struct HeadOnly {
    /// The preamble.
    pub preamble: Preamble,
    /// Every header, system and application alike.
    pub headers: std::collections::HashMap<String, String>,
}

/// A container file, open for reading or appending.
#[napi]
pub struct Container {
    inner: Inner,
}

#[napi]
impl Container {
    /// Creates a file: preamble and header zone, no data yet.
    #[napi(factory)]
    pub fn create(
        path: String,
        schema: Schema,
        source: String,
        writer: String,
        created_at_millis: Option<BigInt>,
        header_area: Option<u32>,
        headers: Option<std::collections::HashMap<String, String>>,
    ) -> Result<Self> {
        let pairs: Vec<(String, String)> = headers.unwrap_or_default().into_iter().collect();
        let borrowed: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let created = created_at_millis.map_or(0, |value| value.get_u64().1);
        let inner = map(Inner::create(
            &PathBuf::from(path),
            &bizstd::Schema::from(&schema),
            &source,
            &writer,
            created,
            header_area.unwrap_or(bizstd::DEFAULT_HEADER_AREA),
            &borrowed,
        ))?;
        Ok(Self { inner })
    }

    /// Opens a file read-only. Nothing is recovered and nothing is written.
    #[napi(factory)]
    pub fn open_read(path: String) -> Result<Self> {
        Ok(Self { inner: map(Inner::open_read(&PathBuf::from(path)))? })
    }

    /// Opens a file for appending, replaying a pending seal journal and
    /// cutting a torn record off the tail first.
    ///
    /// Takes no lock: one writer per file, and enforcing that is the caller's.
    #[napi(factory)]
    pub fn open_append(path: String) -> Result<Self> {
        Ok(Self { inner: map(Inner::open_append(&PathBuf::from(path)))? })
    }

    /// Every header, system and application alike.
    #[napi]
    pub fn headers(&self) -> std::collections::HashMap<String, String> {
        self.inner
            .headers()
            .pairs()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// The closed frames, in file order.
    #[napi]
    pub fn frames(&self) -> Vec<Frame> {
        self.inner.frames().iter().copied().map(Frame::from).collect()
    }

    /// Bytes per record, or `null` for length-prefixed records.
    #[napi]
    pub fn fixed_size(&self) -> Option<u32> {
        match self.inner.schema().layout {
            RecordLayout::Fixed(size) => Some(size),
            RecordLayout::Prefixed => None,
        }
    }

    /// Records ever appended, the unflushed buffer included.
    #[napi]
    pub fn record_count(&self) -> BigInt {
        BigInt::from(self.inner.records())
    }

    /// Appends one record body.
    #[napi]
    pub fn append(&mut self, body: Buffer) -> Result<()> {
        map(self.inner.append_record(&body))
    }

    /// Writes whatever is buffered.
    #[napi]
    pub fn flush(&mut self) -> Result<()> {
        map(self.inner.flush_data())
    }

    /// Compresses the raw tail into one frame, crash-safe.
    #[napi]
    pub fn close_frame(&mut self, frame_id: BigInt, level: Option<i32>) -> Result<()> {
        map(self
            .inner
            .close_frame(frame_id.get_u64().1, level.unwrap_or(bizstd::HOT_LEVEL)))
    }

    /// Closes the tail and marks the file finished.
    #[napi]
    pub fn seal(&mut self, frame_id: BigInt, level: Option<i32>) -> Result<()> {
        map(self
            .inner
            .seal(frame_id.get_u64().1, level.unwrap_or(bizstd::HOT_LEVEL)))
    }

    /// Sets an application header. Keys may not start with `_`.
    #[napi]
    pub fn set_header(&mut self, key: String, value: String) -> Result<()> {
        map(self.inner.set_user_header(&key, &value))
    }

    /// Decompresses the frame at this position in the list.
    ///
    /// By position rather than by id: ids belong to the writer and writers
    /// repeat them.
    #[napi]
    pub fn read_frame_at(&mut self, index: u32) -> Result<Buffer> {
        let raw = map(self.inner.read_frame_at(index as usize))?;
        Ok(Buffer::from(raw))
    }

    /// Decompresses the first frame carrying this id.
    #[napi]
    pub fn read_frame(&mut self, frame_id: BigInt) -> Result<Buffer> {
        let raw = map(self.inner.read_frame(frame_id.get_u64().1))?;
        Ok(Buffer::from(raw))
    }

    /// The uncompressed tail, whole records only.
    #[napi]
    pub fn read_tail(&mut self) -> Result<Buffer> {
        let raw = map(self.inner.read_tail())?;
        Ok(Buffer::from(raw))
    }
}

/// Preamble and headers without touching the data section.
#[napi]
pub fn peek_headers(path: String) -> Result<HeadOnly> {
    let (preamble, headers) = map(bizstd::peek_headers(&PathBuf::from(path)))?;
    Ok(HeadOnly {
        preamble: Preamble {
            version: preamble.version,
            flags: preamble.flags,
            header_area: preamble.header_area,
        },
        headers: headers
            .pairs()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    })
}

/// Reads every frame, checks the counters, the alignment and the checksums.
#[napi]
pub fn validate(path: String) -> Result<ValidateReport> {
    let report = map(bizstd::validate(&PathBuf::from(path), None))?;
    Ok(ValidateReport {
        problems: report.problems,
        frames: u32::try_from(report.frames).unwrap_or(u32::MAX),
        records: BigInt::from(report.records),
    })
}

/// Derives the system headers from the data, and writes them back when asked.
#[napi]
pub fn rebuild_headers(path: String, fix: Option<bool>) -> Result<RebuildReport> {
    let report = map(bizstd::rebuild_headers(&PathBuf::from(path), fix.unwrap_or(false)))?;
    Ok(RebuildReport { differences: report.differences, fixed: report.fixed })
}

/// Re-encodes every frame at another level, atomically.
#[napi]
pub fn repack(path: String, level: Option<i32>, header_area: Option<u32>) -> Result<RepackReport> {
    let path = PathBuf::from(path);
    let level = level.unwrap_or(bizstd::COLD_LEVEL);
    let report = match header_area {
        Some(area) => map(bizstd::repack_with_header_area(&path, level, area))?,
        None => map(bizstd::repack(&path, level))?,
    };
    Ok(RepackReport {
        bytes_before: BigInt::from(report.bytes_before),
        bytes_after: BigInt::from(report.bytes_after),
        frames: u32::try_from(report.frames).unwrap_or(u32::MAX),
    })
}

/// Splits raw bytes into records. The second value is the torn leftover.
#[napi]
pub fn split_records(data: Buffer, fixed_size: Option<u32>) -> (Vec<Buffer>, u32) {
    let layout = fixed_size.map_or(RecordLayout::Prefixed, RecordLayout::Fixed);
    let (records, leftover) = bizstd::split_records(&data, layout);
    (
        records.into_iter().map(|record| Buffer::from(record.to_vec())).collect(),
        u32::try_from(leftover).unwrap_or(u32::MAX),
    )
}

/// XXH64 with seed 0 — the hash the frame checksums use.
#[napi]
pub fn xxh64(data: Buffer) -> BigInt {
    BigInt::from(bizstd::xxh64(&data))
}

/// Roughly how many frames a header zone of this size can list.
#[napi]
pub fn max_frames_for(header_area: u32) -> BigInt {
    BigInt::from(bizstd::max_frames_for(header_area))
}

/// Container format version this build writes.
#[napi]
pub const VERSION: u8 = bizstd::VERSION;
/// The file extension, without the dot.
#[napi]
pub const EXTENSION: &str = bizstd::EXTENSION;
/// The default header zone size.
#[napi]
pub const DEFAULT_HEADER_AREA: u32 = bizstd::DEFAULT_HEADER_AREA;
/// The largest header zone a file may declare.
#[napi]
pub const MAX_HEADER_AREA: u32 = bizstd::MAX_HEADER_AREA;
/// The zstd level for closing frames on the write path.
#[napi]
pub const HOT_LEVEL: i32 = bizstd::HOT_LEVEL;
/// The zstd level for repacking a file offline.
#[napi]
pub const COLD_LEVEL: i32 = bizstd::COLD_LEVEL;
