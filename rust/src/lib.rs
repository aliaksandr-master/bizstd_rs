//! bizstd — "Blazing-fast Insert ZSTD compressed file format".
//!
//! A self-describing binary container: a fixed 16-byte preamble, a
//! fixed-size text header zone updated in place, and a data section of
//! independent zstd frames followed by an uncompressed crash-safe tail.
//!
//! ```text
//! preamble (16 bytes, binary):
//!   magic "BIZSTD"   6 bytes
//!   version          u8
//!   flags            u8      (bit 0: data compressed)
//!   header_area_len  u32 LE  (usually 4096)
//!   reserved         4 bytes
//!
//! header zone (ASCII text inside header_area_len bytes):
//!   key:value\n      key `_?[a-z0-9][a-z0-9_]*`, split on the FIRST ':'
//!   \n               an empty line ends the headers
//!   zero padding to the end of the zone
//!
//! data (at offset 16 + header_area_len):
//!   zstd frames of closed hours, then the raw uncompressed tail of the
//!   hour being written
//! ```
//!
//! System keys start with `_` and are a closed list owned by this file:
//! `_schema`, `_schema_fields`, `_schema_hash`, `_record`, `_source`,
//! `_created_at`, `_writer`, `_compression`, `_compression_level`,
//! `_records`, `_bytes_raw`, `_frames`, `_preview`, `_sealed`. Every one of
//! them is a rebuildable cache — the data section is the truth. The
//! counters `_records` and `_bytes_raw` cover closed frames only; records
//! still sitting in the raw tail join them when their frame closes.
//! Application keys must NOT start with `_`; the container never
//! interprets them.
//!
//! Values escape exactly two characters: `\n` and `\\`. Header values built
//! by this file contain neither, so their round trip is byte-identical.
//!
//! Closing a frame is crash-safe through a redo journal (`<file>.seal`):
//! the compressed frame and the new header zone become durable in the
//! journal first, then the raw tail is overwritten in place, the file is
//! truncated and the zone rewritten, and only then the journal is removed.
//! [`Container::open_append`] replays a complete journal and discards an
//! incomplete one, then truncates a torn record off the raw tail — a crash
//! at any instant loses at most the buffered records of the current write.
//!
//! The crate is deliberately small — `std` and `zstd` only, no other
//! dependency and nothing to configure — so that reading a container needs
//! this crate and nothing else.
//!
//! # Concurrency
//!
//! **One writer per file, and enforcing that is the caller's job.** Nothing
//! here takes a lock. Two handles opened with [`Container::open_append`] on
//! the same path will both recover the tail, both buffer records, and both
//! write at offsets the other does not know about; the file that results is
//! not one either of them wrote. There is no error for this and no detection —
//! only the guarantee that it will not be noticed until the data is read back.
//!
//! Readers are unrestricted among themselves: [`Container::open_read`] never
//! writes. A reader running alongside a writer is safe in the sense that it
//! cannot corrupt anything, but what it sees is a file in motion — the tail
//! grows under it, and a frame being closed changes both the data and the
//! header zone. Reading a file that is being written to gives a consistent
//! answer only for the frames already closed when the read started.
//!
//! # Example
//!
//! ```no_run
//! use bizstd::{Container, DEFAULT_HEADER_AREA, FieldSpec, HOT_LEVEL, RecordLayout, Schema};
//!
//! # fn main() -> bizstd::Result<()> {
//! let schema = Schema {
//!     name: "samples@1".to_owned(),
//!     fields: vec![
//!         FieldSpec { name: "time_nanos".to_owned(), ty: "u64".to_owned(), offset: 0 },
//!         FieldSpec { name: "value".to_owned(), ty: "f64".to_owned(), offset: 8 },
//!     ],
//!     layout: RecordLayout::Fixed(16),
//! };
//!
//! let mut file = Container::create(
//!     "samples.bizstd".as_ref(),
//!     &schema,
//!     "example",
//!     "example writer",
//!     0,
//!     DEFAULT_HEADER_AREA,
//!     &[("stream", "alpha")],
//! )?;
//!
//! let mut record = [0u8; 16];
//! record[..8].copy_from_slice(&1_u64.to_le_bytes());
//! record[8..].copy_from_slice(&2.5_f64.to_le_bytes());
//! file.append_record(&record)?;
//! file.seal(0, HOT_LEVEL)?;
//! # Ok(())
//! # }
//! ```

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The README is compiled as a doctest so its examples cannot rot into
/// something that no longer builds. It is not part of the public API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct Readme;

/// Everything this crate can fail with.
///
/// The variants are the distinctions a caller can act on: a malformed file is
/// worth quarantining, a full header zone is worth repacking, and a usage
/// mistake is worth fixing in the calling code. Marked `#[non_exhaustive]` so
/// that a later version can name a new failure without breaking the callers
/// that match on this one.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An I/O failure with the path it happened on.
    Io {
        /// The file involved, when one is known.
        path: Option<PathBuf>,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The file is not a well-formed container: bad magic, unknown version,
    /// unparsable headers, a frame list that does not match the bytes.
    Malformed(String),
    /// The caller asked for something the container cannot do: writing to a
    /// read-only handle, a record that does not match the schema, a header
    /// key that is not theirs to set.
    Usage(String),
    /// The header zone cannot hold the headers. Distinguishable on purpose:
    /// it is the one failure a long-lived writer hits by growing normally,
    /// and the way out is [`repack_with_header_area`], not a code change.
    ZoneFull {
        /// Bytes the headers need.
        needed: usize,
        /// Bytes the zone holds.
        capacity: usize,
    },
    /// zstd refused to compress or decompress.
    Compression(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path: Some(path), source } => write!(out, "{}: {source}", path.display()),
            Self::Io { path: None, source } => write!(out, "{source}"),
            Self::Malformed(text) | Self::Usage(text) | Self::Compression(text) => write!(out, "{text}"),
            Self::ZoneFull { needed, capacity } => {
                write!(out, "headers need {needed} bytes, the zone holds {capacity}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _other => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io { path: None, source }
    }
}

/// The result type every fallible function in this crate returns.
pub type Result<T> = std::result::Result<T, Error>;

/// An I/O failure that knows which file it happened on.
fn io_at(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |source| Error::Io { path: Some(path.to_owned()), source }
}

/// Shorthand for the two string-carrying variants.
fn malformed(text: impl Into<String>) -> Error {
    Error::Malformed(text.into())
}

fn usage(text: impl Into<String>) -> Error {
    Error::Usage(text.into())
}

/// Refuses a header zone the file is not big enough to hold.
///
/// Both numbers come from the file, so believing either one on its own is how
/// a twenty-byte file asks for a gigabyte.
fn check_zone_fits(preamble: Preamble, file_len: u64) -> Result<()> {
    let needed = (PREAMBLE_BYTES as u64).saturating_add(u64::from(preamble.header_area));
    if file_len < needed {
        return Err(malformed(format!(
            "header zone claims {} bytes but the file is {file_len} bytes",
            preamble.header_area
        )));
    }
    Ok(())
}

/// Refuses a frame list that points outside the file.
fn check_frames_fit(frames: &[Frame], data_start: u64, file_len: u64) -> Result<()> {
    for frame in frames {
        let end = data_start
            .checked_add(frame.offset)
            .and_then(|start| start.checked_add(frame.len))
            .ok_or_else(|| malformed(format!("frame {} overflows a u64 offset", frame.id)))?;
        if end > file_len {
            return Err(malformed(format!("frame {} ends at {end} but the file is {file_len} bytes", frame.id)));
        }
    }
    Ok(())
}

/// A length read from a file, narrowed to this platform's `usize`.
fn to_usize(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_error| malformed(format!("{value} bytes does not fit in this platform's usize")))
}

/// The preamble magic, exactly six bytes.
pub const MAGIC: [u8; 6] = *b"BIZSTD";
/// The container version this file writes.
pub const VERSION: u8 = 1;
/// The preamble's fixed size.
pub const PREAMBLE_BYTES: usize = 16;
/// The default header zone size.
pub const DEFAULT_HEADER_AREA: u32 = 4096;
/// Preamble flag bit 0: the data section carries zstd frames.
pub const FLAG_COMPRESSED: u8 = 1;
/// The file extension, without the dot.
pub const EXTENSION: &str = "bizstd";
/// The zstd level for closing frames on the hot path.
pub const HOT_LEVEL: i32 = 3;
/// The zstd level for offline repacking of cold files.
pub const COLD_LEVEL: i32 = 19;
/// No compression at all: frames are stored as the bytes that were appended.
///
/// Useful when the data does not compress, when the reader cares about latency
/// more than about disk, or when something else downstream compresses anyway.
/// A frame stored this way is recognised on the way back by the absence of the
/// zstd magic, so a file may hold both kinds and still read correctly.
///
/// One thing is given up, and it is worth knowing before choosing this:
/// [`rebuild_headers`] finds frame boundaries by walking zstd frames, which
/// announce their own length. Raw frames do not, so a file written at this
/// level depends on `_frames` for its boundaries and cannot have them
/// reconstructed from the data if that header is lost.
pub const NO_COMPRESSION: i32 = 0;
/// How many records land in `_preview`.
const PREVIEW_RECORDS: usize = 3;
/// The `_preview` value budget in bytes.
const PREVIEW_BUDGET: usize = 1024;
/// Appended records are buffered up to this many bytes between writes.
const WRITE_BUFFER_BYTES: usize = 64 * 1024;
/// The redo journal's magic.
const JOURNAL_MAGIC: [u8; 6] = *b"BJRNL2";
/// Magic, payload length and payload checksum.
const JOURNAL_HEAD_BYTES: usize = 22;
/// The largest header zone a file may declare. A `u32` read straight from a
/// 20-byte file would otherwise ask the allocator for 4 GiB, and an allocation
/// that cannot be served aborts the process rather than returning an error.
pub const MAX_HEADER_AREA: u32 = 16 * 1024 * 1024;
/// The largest raw size one frame may decompress to. Without a ceiling a
/// hostile file compresses a few kilobytes into a demand for all the memory
/// there is.
pub const MAX_FRAME_RAW_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The little-endian magic that starts every zstd frame.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// FNV-1a 64 — the schema fingerprint. A fingerprint, not a signature.
#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Whether the text is a legal header key: `_?[a-z0-9][a-z0-9_]*`.
#[must_use]
pub fn key_is_valid(key: &str) -> bool {
    let body = key.strip_prefix('_').unwrap_or(key);
    let mut chars = body.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Escapes the two characters a value may not carry raw: `\n` and `\`.
#[must_use]
pub fn escape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Reverses [`escape_value`].
#[must_use]
pub fn unescape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The 16-byte binary preamble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preamble {
    /// Container format version.
    pub version: u8,
    /// Bit 0: data compressed.
    pub flags: u8,
    /// The header zone size in bytes.
    pub header_area: u32,
}

impl Preamble {
    /// The 16 bytes.
    #[must_use]
    pub fn encode(self) -> [u8; PREAMBLE_BYTES] {
        let mut out = [0u8; PREAMBLE_BYTES];
        out.iter_mut()
            .zip(MAGIC.iter())
            .for_each(|(slot, byte)| *slot = *byte);
        if let Some(slot) = out.get_mut(6) {
            *slot = self.version;
        }
        if let Some(slot) = out.get_mut(7) {
            *slot = self.flags;
        }
        if let Some(area) = out.get_mut(8..12) {
            area.copy_from_slice(&self.header_area.to_le_bytes());
        }
        out
    }

    /// Reads the preamble back.
    ///
    /// # Errors
    /// Returns a message when the magic, version or zone size is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.get(0..6) != Some(MAGIC.as_slice()) {
            return Err(malformed("not a bizstd file: bad magic"));
        }
        let version = bytes.get(6).copied().ok_or_else(|| malformed("preamble too short"))?;
        if version != VERSION {
            return Err(malformed(format!("unsupported bizstd version {version}")));
        }
        let flags = bytes.get(7).copied().ok_or_else(|| malformed("preamble too short"))?;
        let header_area = bytes
            .get(8..12)
            .and_then(|four| four.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| malformed("preamble too short"))?;
        if header_area == 0 {
            return Err(malformed("header zone size is zero"));
        }
        if header_area > MAX_HEADER_AREA {
            return Err(malformed(format!(
                "header zone claims {header_area} bytes, the ceiling is {MAX_HEADER_AREA}"
            )));
        }
        Ok(Self { version, flags, header_area })
    }
}

/// The ordered key:value pairs of the header zone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    pairs: Vec<(String, String)>,
}

impl Headers {
    /// The value of a key, unescaped.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(name, _value)| name == key)
            .map(|(_name, value)| value.as_str())
    }

    /// Sets a key, replacing an existing value, keeping first-set order.
    ///
    /// # Errors
    /// Returns a message when the key breaks the grammar.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if !key_is_valid(key) {
            return Err(usage(format!("invalid header key {key:?}")));
        }
        match self.pairs.iter_mut().find(|(name, _value)| name == key) {
            Some((_name, slot)) => value.clone_into(slot),
            None => self.pairs.push((key.to_owned(), value.to_owned())),
        }
        Ok(())
    }

    /// Sets an application key; the leading `_` is reserved for the container.
    ///
    /// # Errors
    /// Returns a message on a `_`-prefixed or ungrammatical key.
    pub fn set_user(&mut self, key: &str, value: &str) -> Result<()> {
        if key.starts_with('_') {
            return Err(usage(format!("user header key {key:?} may not start with '_'")));
        }
        self.set(key, value)
    }

    /// Every pair in order.
    #[must_use]
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// Parses a header zone: lines to the first empty line, zeros ignored.
    ///
    /// # Errors
    /// Returns a message on non-UTF-8 text, a missing terminator, or a line
    /// without a colon.
    pub fn parse(zone: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(zone).map_err(|error| malformed(format!("header zone is not UTF-8: {error}")))?;
        let mut pairs = Vec::new();
        let mut terminated = false;
        for line in text.split('\n') {
            if line.is_empty() || line.starts_with('\0') {
                terminated = true;
                break;
            }
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| malformed(format!("header line without ':': {line:?}")))?;
            if !key_is_valid(key) {
                return Err(usage(format!("invalid header key {key:?}")));
            }
            pairs.push((key.to_owned(), unescape_value(value)));
        }
        if !terminated {
            return Err(malformed("header zone has no empty-line terminator"));
        }
        Ok(Self { pairs })
    }

    /// Renders the zone: escaped lines, an empty line, zero padding.
    ///
    /// # Errors
    /// Returns a message when the text does not fit the zone.
    pub fn render(&self, area: usize) -> Result<Vec<u8>> {
        let mut text = String::new();
        for (key, value) in &self.pairs {
            text.push_str(key);
            text.push(':');
            text.push_str(&escape_value(value));
            text.push('\n');
        }
        text.push('\n');
        if text.len() > area {
            return Err(Error::ZoneFull { needed: text.len(), capacity: area });
        }
        let mut zone = text.into_bytes();
        zone.resize(area, 0);
        Ok(zone)
    }
}

/// How record boundaries are found in the raw byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordLayout {
    /// Every record is exactly this many bytes.
    Fixed(u32),
    /// Every record is a u16 LE body length followed by the body.
    Prefixed,
}

impl RecordLayout {
    /// The `_record` header value.
    #[must_use]
    pub fn render(self) -> String {
        match self {
            Self::Fixed(bytes) => format!("fixed:{bytes}"),
            Self::Prefixed => "prefixed".to_owned(),
        }
    }

    /// Parses the `_record` header value.
    ///
    /// # Errors
    /// Returns a message on an unknown shape or a zero size.
    pub fn parse(text: &str) -> Result<Self> {
        if text == "prefixed" {
            return Ok(Self::Prefixed);
        }
        if let Some(size) = text.strip_prefix("fixed:") {
            let bytes: u32 = size
                .parse()
                .map_err(|error| malformed(format!("bad record size {size:?}: {error}")))?;
            if bytes == 0 {
                return Err(malformed("record size is zero"));
            }
            return Ok(Self::Fixed(bytes));
        }
        Err(malformed(format!("unknown record layout {text:?}")))
    }
}

/// One field of a record, for `_schema_fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    /// The field name.
    pub name: String,
    /// The field type token: `u8`, `u16`, `u32`, `u64`, `i64` or `f64`.
    pub ty: String,
    /// Byte offset inside the record.
    pub offset: u32,
}

/// What the records are: a name, the fields, and the boundary rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// The `_schema` value, e.g. `samples@1`.
    pub name: String,
    /// The fields in offset order.
    pub fields: Vec<FieldSpec>,
    /// How record boundaries are found.
    pub layout: RecordLayout,
}

impl Schema {
    /// The `_schema_fields` value; `_schema_hash` is FNV-1a 64 of exactly
    /// this string, so no separate canonical form exists.
    #[must_use]
    pub fn fields_value(&self) -> String {
        self.fields
            .iter()
            .map(|field| format!("{}:{}:{}", field.name, field.ty, field.offset))
            .collect::<Vec<_>>()
            .join(";")
    }

    /// The `_schema_hash` value.
    #[must_use]
    pub fn hash_hex(&self) -> String {
        format!("{:016x}", fnv1a64(self.fields_value().as_bytes()))
    }

    /// Reads the schema back from parsed headers.
    ///
    /// # Errors
    /// Returns a message when a schema header is missing or malformed.
    pub fn from_headers(headers: &Headers) -> Result<Self> {
        let name = headers
            .get("_schema")
            .ok_or_else(|| malformed("missing _schema"))?
            .to_owned();
        let layout = RecordLayout::parse(headers.get("_record").ok_or_else(|| malformed("missing _record"))?)?;
        let mut fields = Vec::new();
        let fields_value = headers
            .get("_schema_fields")
            .ok_or_else(|| malformed("missing _schema_fields"))?;
        for token in fields_value.split(';').filter(|token| !token.is_empty()) {
            let mut parts = token.splitn(3, ':');
            let field_name = parts.next().unwrap_or_default();
            let ty = parts.next().unwrap_or_default();
            let offset: u32 = parts
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|error| malformed(format!("bad field token {token:?}: {error}")))?;
            if field_name.is_empty() || ty.is_empty() {
                return Err(malformed(format!("bad field token {token:?}")));
            }
            fields.push(FieldSpec { name: field_name.to_owned(), ty: ty.to_owned(), offset });
        }
        Ok(Self { name, fields, layout })
    }
}

/// One closed zstd frame in the data section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The caller's frame id — for time-partitioned data, the period index.
    pub id: u64,
    /// Byte offset relative to the start of the data section.
    pub offset: u64,
    /// Compressed length in bytes.
    pub len: u64,
    /// XXH64 of the compressed bytes, `0` when the file predates checksums.
    /// Zero is a possible-but-astronomically-unlikely real hash; treating it
    /// as "unrecorded" trades that corner for a byte-cheap upgrade path on
    /// files written before the field existed.
    pub hash: u64,
}

/// Renders `_frames`: `id,offset,len,xxh64;...` (hash in hex; the fourth
/// field is omitted while unrecorded, so an index written before checksums
/// existed round-trips byte-for-byte).
#[must_use]
pub fn render_frames(frames: &[Frame]) -> String {
    frames
        .iter()
        .map(|frame| {
            if frame.hash == 0 {
                format!("{},{},{}", frame.id, frame.offset, frame.len)
            } else {
                format!("{},{},{},{:016x}", frame.id, frame.offset, frame.len, frame.hash)
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Parses `_frames`. The fourth field, the frame checksum, is optional:
/// files written before it existed carry triples.
///
/// # Errors
/// Returns a message on a malformed triple.
pub fn parse_frames(text: &str) -> Result<Vec<Frame>> {
    let mut frames = Vec::new();
    for token in text.split(';').filter(|token| !token.is_empty()) {
        let mut parts = token.splitn(4, ',');
        let id: u64 = parts
            .next()
            .unwrap_or_default()
            .parse()
            .map_err(|error| malformed(format!("bad frame {token:?}: {error}")))?;
        let offset: u64 = parts
            .next()
            .unwrap_or_default()
            .parse()
            .map_err(|error| malformed(format!("bad frame {token:?}: {error}")))?;
        let len: u64 = parts
            .next()
            .unwrap_or_default()
            .parse()
            .map_err(|error| malformed(format!("bad frame {token:?}: {error}")))?;
        let hash = match parts.next() {
            None | Some("") => 0,
            Some(hex) => u64::from_str_radix(hex, 16).map_err(|error| malformed(format!("bad frame {token:?}: {error}")))?,
        };
        frames.push(Frame { id, offset, len, hash });
    }
    Ok(frames)
}

/// XXH64 of `data` with seed 0 — the per-frame checksum.
///
/// Written out rather than pulled from a dependency: it is forty lines, and
/// the alternative is a second dependency in a crate whose selling point is
/// having one. Constants and steps follow the xxHash specification, and the
/// test vectors pin the implementation to it.
#[must_use]
pub fn xxh64(data: &[u8]) -> u64 {
    const P1: u64 = 0x9E37_79B1_85EB_CA87;
    const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
    const P3: u64 = 0x1656_67B1_9E37_79F9;
    const P4: u64 = 0x85EB_CA77_C2B2_AE63;
    const P5: u64 = 0x27D4_EB2F_1656_67C5;

    let read_u64 = |chunk: &[u8]| -> u64 { chunk.try_into().map_or(0, u64::from_le_bytes) };
    let round = |accumulator: u64, lane: u64| -> u64 {
        accumulator
            .wrapping_add(lane.wrapping_mul(P2))
            .rotate_left(31)
            .wrapping_mul(P1)
    };

    let mut remainder = data;
    let mut hash = if data.len() >= 32 {
        let mut v1 = P1.wrapping_add(P2);
        let mut v2 = P2;
        let mut v3 = 0u64;
        let mut v4 = 0u64.wrapping_sub(P1);
        while remainder.len() >= 32 {
            let (block, rest) = remainder.split_at(32);
            v1 = round(v1, read_u64(block.get(0..8).unwrap_or_default()));
            v2 = round(v2, read_u64(block.get(8..16).unwrap_or_default()));
            v3 = round(v3, read_u64(block.get(16..24).unwrap_or_default()));
            v4 = round(v4, read_u64(block.get(24..32).unwrap_or_default()));
            remainder = rest;
        }
        let mut merged = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        for lane in [v1, v2, v3, v4] {
            merged = (merged ^ round(0, lane)).wrapping_mul(P1).wrapping_add(P4);
        }
        merged
    } else {
        P5
    };
    hash = hash.wrapping_add(data.len() as u64);

    while remainder.len() >= 8 {
        let (chunk, rest) = remainder.split_at(8);
        hash = (hash ^ round(0, read_u64(chunk)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
        remainder = rest;
    }
    if remainder.len() >= 4 {
        let (chunk, rest) = remainder.split_at(4);
        let lane = u64::from(chunk.try_into().map_or(0, u32::from_le_bytes));
        hash = (hash ^ lane.wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        remainder = rest;
    }
    for byte in remainder {
        hash = (hash ^ u64::from(*byte).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }

    hash ^= hash >> 33;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(P3);
    hash ^= hash >> 32;
    hash
}

/// Compresses one frame with the zstd frame checksum enabled: four bytes of
/// XXH64 inside the frame, verified by every decoder that reads it.
///
/// At [`NO_COMPRESSION`] the bytes are stored as they are. Nothing marks them:
/// a frame is compressed if it starts with the zstd magic and stored plainly
/// if it does not, which is what lets one file hold both.
fn compress_frame(raw: &[u8], level: i32) -> Result<Vec<u8>> {
    if level == NO_COMPRESSION {
        return Ok(raw.to_vec());
    }
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), level).map_err(Error::from)?;
    encoder.include_checksum(true).map_err(Error::from)?;
    encoder.write_all(raw).map_err(Error::from)?;
    encoder.finish().map_err(Error::from)
}

/// Splits raw bytes into records; the second value is the torn leftover.
#[must_use]
pub fn split_records(bytes: &[u8], layout: RecordLayout) -> (Vec<&[u8]>, usize) {
    match layout {
        RecordLayout::Fixed(size) => {
            let size = size as usize;
            if size == 0 {
                return (Vec::new(), bytes.len());
            }
            let records: Vec<&[u8]> = bytes.chunks_exact(size).collect();
            let leftover = bytes.len().checked_rem(size).unwrap_or(0);
            (records, leftover)
        }
        RecordLayout::Prefixed => {
            let mut records = Vec::new();
            let mut position = 0usize;
            while let Some(prefix) = bytes
                .get(position..position.saturating_add(2))
                .and_then(|two| two.try_into().ok())
                .map(u16::from_le_bytes)
            {
                let body_start = position.saturating_add(2);
                let body_end = body_start.saturating_add(prefix as usize);
                // The BODY, without its length prefix: a record's fields sit
                // at the schema's offsets, and a slice that still carried the
                // two prefix bytes shifted every field read by two.
                let Some(record) = bytes.get(body_start..body_end) else {
                    break;
                };
                records.push(record);
                position = body_end;
            }
            (records, bytes.len().saturating_sub(position))
        }
    }
}

/// RFC 3339 UTC from epoch milliseconds — civil-from-days, Hinnant.
#[must_use]
pub fn rfc3339_utc(millis: u64) -> String {
    let seconds = millis.checked_div(1000).unwrap_or(0);
    let days = i64::try_from(seconds.checked_div(86_400).unwrap_or(0)).unwrap_or(0);
    let second_of_day = seconds.checked_rem(86_400).unwrap_or(0);
    #[expect(
        clippy::integer_division,
        reason = "the civil-calendar algorithm is defined in integer divisions"
    )]
    let (year, month, day) = {
        let shifted = days.saturating_add(719_468);
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted.rem_euclid(146_097);
        let year_of_era = day_of_era
            .saturating_sub(day_of_era / 1460)
            .saturating_add(day_of_era / 36_524)
            .saturating_sub(day_of_era / 146_096)
            / 365;
        let day_of_year = day_of_era.saturating_sub(
            365i64
                .saturating_mul(year_of_era)
                .saturating_add(year_of_era / 4)
                .saturating_sub(year_of_era / 100),
        );
        let month_index = (5i64.saturating_mul(day_of_year).saturating_add(2)) / 153;
        let day = day_of_year
            .saturating_sub((153i64.saturating_mul(month_index).saturating_add(2)) / 5)
            .saturating_add(1);
        let month = if month_index < 10 {
            month_index.saturating_add(3)
        } else {
            month_index.saturating_sub(9)
        };
        let year = year_of_era
            .saturating_add(era.saturating_mul(400))
            .saturating_add(i64::from(month <= 2));
        (year, month, day)
    };
    let hour = second_of_day.checked_div(3600).unwrap_or(0);
    let minute = second_of_day
        .checked_rem(3600)
        .unwrap_or(0)
        .checked_div(60)
        .unwrap_or(0);
    let second = second_of_day.checked_rem(60).unwrap_or(0);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Reads just the preamble and header zone — the cheap peek the catalogue
/// does per file, one bounded read, never touching the data section.
///
/// # Errors
/// Returns a message when the file cannot be opened or the head does not
/// parse.
pub fn peek_headers(path: &Path) -> Result<(Preamble, Headers)> {
    let mut file = File::open(path).map_err(io_at(path))?;
    let mut preamble_bytes = [0u8; PREAMBLE_BYTES];
    file.read_exact(&mut preamble_bytes).map_err(Error::from)?;
    let preamble = Preamble::decode(&preamble_bytes)?;
    let file_len = file.metadata().map_err(io_at(path))?.len();
    check_zone_fits(preamble, file_len)?;
    let mut zone = vec![0u8; preamble.header_area as usize];
    file.read_exact(&mut zone).map_err(Error::from)?;
    let headers = Headers::parse(&zone)?;
    Ok((preamble, headers))
}

/// A `.bizstd` file, open for reading or appending.
#[derive(Debug)]
pub struct Container {
    path: PathBuf,
    file: File,
    preamble: Preamble,
    headers: Headers,
    schema: Schema,
    frames: Vec<Frame>,
    data_start: u64,
    file_len: u64,
    /// Records ever appended, the raw tail included.
    records: u64,
    /// Raw bytes ever appended, the raw tail included.
    bytes_raw: u64,
    /// Records inside closed frames — what `_records` publishes. Tail
    /// records join it when their frame closes; a crash before that leaves
    /// them on disk uncounted, and reopening recounts them by scan.
    records_closed: u64,
    /// Raw bytes inside closed frames — what `_bytes_raw` publishes.
    bytes_closed: u64,
    pending: Vec<u8>,
    writable: bool,
}

impl Container {
    /// Creates a new file: preamble and header zone, no data yet.
    ///
    /// `user` pairs are application headers; their keys may not start
    /// with `_`.
    ///
    /// # Errors
    /// Returns a message when the file exists, a key is illegal, or the
    /// headers overflow the zone.
    pub fn create(path: &Path, schema: &Schema, source: &str, writer: &str, created_at_millis: u64, header_area: u32, user: &[(&str, &str)]) -> Result<Self> {
        let mut headers = Headers::default();
        headers.set("_schema", &schema.name)?;
        headers.set("_schema_fields", &schema.fields_value())?;
        headers.set("_schema_hash", &schema.hash_hex())?;
        headers.set("_record", &schema.layout.render())?;
        headers.set("_source", source)?;
        headers.set("_created_at", &rfc3339_utc(created_at_millis))?;
        headers.set("_writer", writer)?;
        headers.set("_compression", "zstd")?;
        headers.set("_compression_level", &HOT_LEVEL.to_string())?;
        headers.set("_records", "0")?;
        headers.set("_bytes_raw", "0")?;
        headers.set("_frames", "")?;
        headers.set("_preview", "")?;
        headers.set("_sealed", "false")?;
        for (key, value) in user {
            headers.set_user(key, value)?;
        }
        // A new file expects to compress; a frame closed at NO_COMPRESSION and
        // a repack that drops compression correct this. The flag is
        // informational either way: a reader decides frame by frame, from the
        // bytes, which is the only source that cannot be stale.
        let preamble = Preamble { version: VERSION, flags: FLAG_COMPRESSED, header_area };
        let zone = headers.render(header_area as usize)?;

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(io_at(path))?;
        file.write_all(&preamble.encode()).map_err(Error::from)?;
        file.write_all(&zone).map_err(Error::from)?;
        file.sync_data().map_err(Error::from)?;
        let data_start = (PREAMBLE_BYTES as u64).saturating_add(u64::from(header_area));
        Ok(Self {
            path: path.to_owned(),
            file,
            preamble,
            headers,
            schema: schema.clone(),
            frames: Vec::new(),
            data_start,
            file_len: data_start,
            records: 0,
            bytes_raw: 0,
            records_closed: 0,
            bytes_closed: 0,
            pending: Vec::new(),
            writable: true,
        })
    }

    /// Opens a file read-only. No recovery is attempted.
    ///
    /// # Errors
    /// Returns a message when the preamble or headers do not parse.
    pub fn open_read(path: &Path) -> Result<Self> {
        Self::open_inner(path, false)
    }

    /// Opens a file for appending, replaying the seal journal and cutting a
    /// torn record off the raw tail first.
    ///
    /// This takes no lock. Opening the same file for appending twice, in one
    /// process or in two, corrupts it silently — see the crate documentation.
    ///
    /// # Errors
    /// Returns a message when the preamble or headers do not parse, or
    /// recovery cannot write.
    pub fn open_append(path: &Path) -> Result<Self> {
        replay_journal(path)?;
        let mut container = Self::open_inner(path, true)?;
        container.truncate_torn_tail()?;
        Ok(container)
    }

    fn open_inner(path: &Path, writable: bool) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .map_err(io_at(path))?;
        let mut preamble_bytes = [0u8; PREAMBLE_BYTES];
        file.read_exact(&mut preamble_bytes).map_err(Error::from)?;
        let preamble = Preamble::decode(&preamble_bytes)?;
        let file_len = file.metadata().map_err(io_at(path))?.len();
        check_zone_fits(preamble, file_len)?;
        let mut zone = vec![0u8; preamble.header_area as usize];
        file.read_exact(&mut zone).map_err(Error::from)?;
        let headers = Headers::parse(&zone)?;
        let schema = Schema::from_headers(&headers)?;
        let frames = parse_frames(headers.get("_frames").unwrap_or_default())?;
        let records = headers.get("_records").unwrap_or("0").parse().unwrap_or(0);
        let bytes_raw = headers.get("_bytes_raw").unwrap_or("0").parse().unwrap_or(0);
        let data_start = (PREAMBLE_BYTES as u64).saturating_add(u64::from(preamble.header_area));
        check_frames_fit(&frames, data_start, file_len)?;
        Ok(Self {
            path: path.to_owned(),
            file,
            preamble,
            headers,
            schema,
            frames,
            data_start,
            file_len,
            records,
            bytes_raw,
            records_closed: records,
            bytes_closed: bytes_raw,
            pending: Vec::new(),
            writable,
        })
    }

    /// The parsed headers.
    #[must_use]
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// The schema the file declares.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The closed frames.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// Records ever appended, counting the unflushed buffer.
    #[must_use]
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Where the raw tail begins, absolute.
    #[must_use]
    fn tail_start(&self) -> u64 {
        let frames_end = self
            .frames
            .last()
            .map_or(0, |frame| frame.offset.saturating_add(frame.len));
        self.data_start.saturating_add(frames_end)
    }

    /// Appends one record body. `Prefixed` layouts get the u16 LE length
    /// written by the container; `Fixed` bodies must match the size exactly.
    ///
    /// # Errors
    /// Returns a message on a size mismatch or a write failure.
    pub fn append_record(&mut self, body: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(usage("file is open read-only"));
        }
        match self.schema.layout {
            RecordLayout::Fixed(size) => {
                if body.len() != size as usize {
                    return Err(usage(format!("record is {} bytes, the schema says {size}", body.len())));
                }
                self.pending.extend_from_slice(body);
                self.bytes_raw = self.bytes_raw.saturating_add(body.len() as u64);
            }
            RecordLayout::Prefixed => {
                let len = u16::try_from(body.len()).map_err(|_error| usage(format!("record of {} bytes overflows the u16 prefix", body.len())))?;
                self.pending.extend_from_slice(&len.to_le_bytes());
                self.pending.extend_from_slice(body);
                self.bytes_raw = self.bytes_raw.saturating_add(body.len() as u64).saturating_add(2);
            }
        }
        self.records = self.records.saturating_add(1);
        if self.pending.len() >= WRITE_BUFFER_BYTES {
            self.flush_data()?;
        }
        Ok(())
    }

    /// Writes the buffered records to disk.
    ///
    /// # Errors
    /// Returns the write failure.
    pub fn flush_data(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.file.seek(SeekFrom::Start(self.file_len)).map_err(Error::from)?;
        self.file.write_all(&self.pending).map_err(Error::from)?;
        self.file_len = self.file_len.saturating_add(self.pending.len() as u64);
        self.pending.clear();
        Ok(())
    }

    /// Closes the raw tail into one zstd frame, crash-safe.
    ///
    /// The compressed frame and the new header zone go to the `<file>.seal`
    /// journal first; then the tail is overwritten in place, the file
    /// truncated and the zone rewritten; then the journal is removed. A
    /// crash anywhere is repaired by [`Container::open_append`]. An empty
    /// tail is a no-op.
    ///
    /// # Errors
    /// Returns a message on any write failure.
    pub fn close_frame(&mut self, frame_id: u64, level: i32) -> Result<()> {
        if !self.writable {
            return Err(usage("file is open read-only"));
        }
        self.flush_data()?;
        let tail_start = self.tail_start();
        let raw_len = self.file_len.saturating_sub(tail_start);
        if raw_len == 0 {
            return Ok(());
        }
        let mut raw = vec![0u8; to_usize(raw_len)?];
        self.file.seek(SeekFrom::Start(tail_start)).map_err(Error::from)?;
        self.file.read_exact(&mut raw).map_err(Error::from)?;

        let preview = if self.headers.get("_preview").unwrap_or_default().is_empty() {
            Some(self.render_preview(&raw))
        } else {
            None
        };

        let compressed = compress_frame(&raw, level)?;

        // Everything below is assembled on copies. A frame pushed onto
        // `self.frames` before the zone is known to render would move
        // `tail_start()` past bytes the file does not have, and every later
        // write would land at the wrong offset — one recoverable error turning
        // the handle into a source of corruption for the rest of its life.
        let frame = Frame {
            id: frame_id,
            offset: tail_start.saturating_sub(self.data_start),
            len: compressed.len() as u64,
            hash: xxh64(&compressed),
        };
        let mut frames = self.frames.clone();
        frames.push(frame);
        let records_closed = self.records;
        let bytes_closed = self.bytes_raw;
        let mut headers = self.headers.clone();
        if let Some(preview) = preview {
            headers.set("_preview", &preview)?;
        }
        headers.set("_frames", &render_frames(&frames))?;
        headers.set("_records", &records_closed.to_string())?;
        headers.set("_bytes_raw", &bytes_closed.to_string())?;
        headers.set("_compression_level", &level.to_string())?;
        headers.set("_compression", if level == NO_COMPRESSION { "none" } else { "zstd" })?;
        let zone = headers.render(self.preamble.header_area as usize)?;

        write_journal(&self.path, tail_start, &zone, &compressed)?;
        splice(&mut self.file, tail_start, &zone, &compressed)?;

        // Durable. Only now does the in-memory view move.
        self.frames = frames;
        self.headers = headers;
        self.records_closed = records_closed;
        self.bytes_closed = bytes_closed;
        self.file_len = tail_start.saturating_add(compressed.len() as u64);
        remove_journal(&self.path);
        Ok(())
    }

    /// Closes the tail and marks the file finished (`_sealed:true`).
    ///
    /// # Errors
    /// Returns a message on any write failure.
    pub fn seal(&mut self, frame_id: u64, level: i32) -> Result<()> {
        self.close_frame(frame_id, level)?;
        self.headers.set("_sealed", "true")?;
        self.flush_headers()
    }

    /// Sets an application header and rewrites the zone.
    ///
    /// # Errors
    /// Returns a message on a `_`-prefixed key or a write failure.
    pub fn set_user_header(&mut self, key: &str, value: &str) -> Result<()> {
        self.headers.set_user(key, value)?;
        self.flush_headers()
    }

    /// Rewrites the header zone in place.
    ///
    /// # Errors
    /// Returns a message when the zone overflows or the write fails.
    pub fn flush_headers(&mut self) -> Result<()> {
        if !self.writable {
            return Err(usage("file is open read-only"));
        }
        self.headers.set("_records", &self.records_closed.to_string())?;
        self.headers.set("_bytes_raw", &self.bytes_closed.to_string())?;
        let zone = self.headers.render(self.preamble.header_area as usize)?;
        self.file
            .seek(SeekFrom::Start(PREAMBLE_BYTES as u64))
            .map_err(Error::from)?;
        self.file.write_all(&zone).map_err(Error::from)?;
        self.file.sync_data().map_err(Error::from)?;
        Ok(())
    }

    /// Decompresses one closed frame.
    ///
    /// # Errors
    /// Returns a message on an unknown id or a decode failure.
    pub fn read_frame(&mut self, frame_id: u64) -> Result<Vec<u8>> {
        let frame = self
            .frames
            .iter()
            .find(|frame| frame.id == frame_id)
            .copied()
            .ok_or_else(|| usage(format!("no frame {frame_id}")))?;
        self.read_frame_body(frame)
    }

    /// Decompresses the frame at a POSITION in the frame list.
    ///
    /// Frame ids repeat legitimately — a caller that partitions by hour closes
    /// a spill under hour 0 after hour 23 — so a sweep meaning "every frame"
    /// must go by position. [`Container::read_frame`] stays for callers that
    /// mean "the one with this id".
    ///
    /// # Errors
    /// Returns an error on a bad index or a decode failure.
    pub fn read_frame_at(&mut self, index: usize) -> Result<Vec<u8>> {
        let frame = self
            .frames
            .get(index)
            .copied()
            .ok_or_else(|| usage(format!("no frame at position {index}")))?;
        self.read_frame_body(frame)
    }

    fn read_frame_body(&mut self, frame: Frame) -> Result<Vec<u8>> {
        check_frames_fit(std::slice::from_ref(&frame), self.data_start, self.file_len)?;
        let mut compressed = vec![0u8; to_usize(frame.len)?];
        self.file
            .seek(SeekFrom::Start(self.data_start.saturating_add(frame.offset)))
            .map_err(Error::from)?;
        self.file.read_exact(&mut compressed).map_err(Error::from)?;
        decode_bounded(&compressed)
    }

    /// Reads the raw uncompressed tail, whole records only.
    ///
    /// # Errors
    /// Returns the read failure.
    pub fn read_tail(&mut self) -> Result<Vec<u8>> {
        let tail_start = self.tail_start();
        let len = self.file_len.saturating_sub(tail_start);
        let mut raw = vec![0u8; to_usize(len)?];
        if !raw.is_empty() {
            self.file.seek(SeekFrom::Start(tail_start)).map_err(Error::from)?;
            self.file.read_exact(&mut raw).map_err(Error::from)?;
        }
        let (_records, leftover) = split_records(&raw, self.schema.layout);
        raw.truncate(raw.len().saturating_sub(leftover));
        Ok(raw)
    }

    /// Renders the first records of the first frame for `_preview`.
    fn render_preview(&self, raw: &[u8]) -> String {
        let (records, _leftover) = split_records(raw, self.schema.layout);
        let mut rendered = Vec::new();
        for record in records.iter().take(PREVIEW_RECORDS) {
            let mut values = Vec::new();
            for field in &self.schema.fields {
                values.push(render_field(record, field));
            }
            rendered.push(values.join(","));
        }
        let preview = rendered.join(";");
        if preview.len() > PREVIEW_BUDGET {
            rendered.truncate(1);
            return rendered.join(";");
        }
        preview
    }

    fn truncate_torn_tail(&mut self) -> Result<()> {
        let tail_start = self.tail_start();
        let len = self.file_len.saturating_sub(tail_start);
        if len == 0 {
            return Ok(());
        }
        let mut raw = vec![0u8; to_usize(len)?];
        self.file.seek(SeekFrom::Start(tail_start)).map_err(Error::from)?;
        self.file.read_exact(&mut raw).map_err(Error::from)?;
        let (records, leftover) = split_records(&raw, self.schema.layout);
        if leftover > 0 {
            let keep = self.file_len.saturating_sub(leftover as u64);
            self.file.set_len(keep).map_err(Error::from)?;
            self.file_len = keep;
        }
        // Header counters stop at the last frame close; the tail on disk is
        // what survived after it.
        self.records = self
            .headers
            .get("_records")
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0)
            .saturating_add(records.len() as u64);
        // The same correction for bytes. Restoring one counter and not the
        // other leaves `_bytes_raw` quietly wrong from the next frame close
        // onwards, and nothing checks it until a rebuild.
        let surviving = raw.len().saturating_sub(leftover) as u64;
        self.bytes_raw = self
            .headers
            .get("_bytes_raw")
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0)
            .saturating_add(surviving);
        Ok(())
    }
}

impl Drop for Container {
    /// Writes whatever is still buffered.
    ///
    /// `append_record` counts a record the moment it is handed over but keeps
    /// it in memory until the buffer fills, so a handle dropped on an early
    /// return — the most ordinary shape an error path has — used to lose up to
    /// the buffer size without a word. Dropping cannot report a failure, so a
    /// caller who needs to know still calls `flush_data` or `close_frame`; this
    /// only makes the silent case rare instead of routine.
    fn drop(&mut self) {
        if self.writable && !self.pending.is_empty() {
            let _ignored = self.flush_data();
        }
    }
}

/// Reads one field of a record for the preview.
fn render_field(record: &[u8], field: &FieldSpec) -> String {
    let offset = field.offset as usize;
    let read = |width: usize| record.get(offset..offset.saturating_add(width));
    match field.ty.as_str() {
        "u8" => record.get(offset).map(ToString::to_string),
        "u16" => read(2)
            .and_then(|b| b.try_into().ok())
            .map(|b| u16::from_le_bytes(b).to_string()),
        "u32" => read(4)
            .and_then(|b| b.try_into().ok())
            .map(|b| u32::from_le_bytes(b).to_string()),
        "u64" => read(8)
            .and_then(|b| b.try_into().ok())
            .map(|b| u64::from_le_bytes(b).to_string()),
        "i64" => read(8)
            .and_then(|b| b.try_into().ok())
            .map(|b| i64::from_le_bytes(b).to_string()),
        "f64" => read(8)
            .and_then(|b| b.try_into().ok())
            .map(|b| f64::from_le_bytes(b).to_string()),
        "uuid" => read(16).map(|bytes| {
            bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ignored = write!(hex, "{byte:02x}");
                hex
            })
        }),
        _other => None,
    }
    .unwrap_or_else(|| "?".to_owned())
}

/// The journal path next to a container file.
fn journal_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(ToOwned::to_owned).unwrap_or_default();
    name.push(".seal");
    path.with_file_name(name)
}

/// Writes the seal journal durably: magic, payload length, checksum, then
/// the tail offset, the zone and the frame bytes.
///
/// The length and the checksum are what make a torn journal recognisable. A
/// crash during this write leaves a prefix on disk, and a prefix is a
/// perfectly plausible journal as far as the fields go — without a way to
/// tell, replay would splice a truncated frame over a raw tail that is still
/// intact, destroying the very records the journal exists to protect.
fn write_journal(path: &Path, tail_start: u64, zone: &[u8], compressed: &[u8]) -> Result<()> {
    let journal = journal_path(path);
    let mut payload = Vec::with_capacity(16usize.saturating_add(zone.len()).saturating_add(compressed.len()));
    payload.extend_from_slice(&tail_start.to_le_bytes());
    payload.extend_from_slice(&(zone.len() as u64).to_le_bytes());
    payload.extend_from_slice(zone);
    payload.extend_from_slice(compressed);

    let mut body = Vec::with_capacity(JOURNAL_HEAD_BYTES.saturating_add(payload.len()));
    body.extend_from_slice(&JOURNAL_MAGIC);
    body.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    body.extend_from_slice(&fnv1a64(&payload).to_le_bytes());
    body.extend_from_slice(&payload);

    let mut file = File::create(&journal).map_err(io_at(&journal))?;
    file.write_all(&body).map_err(Error::from)?;
    file.sync_all().map_err(Error::from)?;
    // The bytes are durable; the directory entry that names them is not, and
    // on a crash a journal nobody can find is a journal that never existed.
    sync_parent_dir(&journal);
    Ok(())
}

/// Flushes the directory entry of a freshly created file.
///
/// Best effort on purpose: on platforms where a directory cannot be opened
/// this does nothing, and the guarantee degrades to "the file's contents are
/// durable" rather than failing a write that otherwise succeeded.
fn sync_parent_dir(path: &Path) {
    // Written as nested `if let` rather than a let-chain on purpose: the chain
    // form needs a newer compiler than anything else here does, and the
    // language features the code uses are what set the supported version.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ignored = dir.sync_all();
        }
    }
}

/// Overwrites the raw tail with the compressed frame and rewrites the zone.
fn splice(file: &mut File, tail_start: u64, zone: &[u8], compressed: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(tail_start)).map_err(Error::from)?;
    file.write_all(compressed).map_err(Error::from)?;
    file.set_len(tail_start.saturating_add(compressed.len() as u64))
        .map_err(Error::from)?;
    file.seek(SeekFrom::Start(PREAMBLE_BYTES as u64))
        .map_err(Error::from)?;
    file.write_all(zone).map_err(Error::from)?;
    file.sync_data().map_err(Error::from)?;
    Ok(())
}

fn remove_journal(path: &Path) {
    let _ignored = std::fs::remove_file(journal_path(path));
}

/// Replays a complete seal journal, discards an incomplete one.
///
/// The journal holds everything the interrupted [`Container::close_frame`]
/// was about to make true, so redoing it is idempotent.
fn replay_journal(path: &Path) -> Result<()> {
    let journal = journal_path(path);
    let Ok(body) = std::fs::read(&journal) else {
        return Ok(());
    };
    let parsed = (|| {
        if body.get(0..6) != Some(JOURNAL_MAGIC.as_slice()) {
            return None;
        }
        let payload_len = body
            .get(6..14)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
            .and_then(|len| usize::try_from(len).ok())?;
        let checksum = body
            .get(14..JOURNAL_HEAD_BYTES)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)?;
        // Length first, then checksum: a torn write leaves a short body, and a
        // body of the right length can still have been written by a different
        // version or scrambled by the filesystem.
        let payload = body.get(JOURNAL_HEAD_BYTES..JOURNAL_HEAD_BYTES.checked_add(payload_len)?)?;
        if body.len() != JOURNAL_HEAD_BYTES.checked_add(payload_len)? || fnv1a64(payload) != checksum {
            return None;
        }
        let tail_start = payload
            .get(0..8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)?;
        let zone_len = payload
            .get(8..16)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)?;
        let zone_end = 16usize.checked_add(usize::try_from(zone_len).ok()?)?;
        let zone = payload.get(16..zone_end)?;
        let compressed = payload.get(zone_end..)?;
        if compressed.is_empty() {
            return None;
        }
        Some((tail_start, zone.to_vec(), compressed.to_vec()))
    })();
    let Some((tail_start, zone, compressed)) = parsed else {
        // Incomplete journal: the crash happened before it became durable,
        // so the main file still carries the intact raw tail.
        let _ignored = std::fs::remove_file(&journal);
        return Ok(());
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io_at(path))?;
    splice(&mut file, tail_start, &zone, &compressed)?;
    let _ignored = std::fs::remove_file(&journal);
    Ok(())
}

/// What [`validate`] found.
#[derive(Debug, Default)]
pub struct ValidateReport {
    /// Everything wrong, empty when the file is sound.
    pub problems: Vec<String>,
    /// Closed frames checked.
    pub frames: usize,
    /// Records seen in frames and the tail.
    pub records: u64,
}

/// Checks a file: headers, schema hash, frame decode, record boundaries,
/// counters. `check` receives every record and returns a problem or `None`;
/// pass `None` to skip content checks. At most 20 problems are collected.
///
/// # Errors
/// Returns a message when the file cannot be opened at all.
pub fn validate(path: &Path, mut check: Option<&mut dyn FnMut(&[u8]) -> Option<String>>) -> Result<ValidateReport> {
    // A pending journal is part of the file's state. Reporting on the bytes
    // without it describes a file that will look different the moment a writer
    // opens it.
    replay_journal(path)?;
    let mut container = Container::open_read(path)?;
    let mut report = ValidateReport::default();
    let problem = |report: &mut ValidateReport, text: String| {
        if report.problems.len() < 20 {
            report.problems.push(text);
        }
    };

    let declared_hash = container.headers.get("_schema_hash").unwrap_or_default().to_owned();
    let actual_hash = container.schema.hash_hex();
    if declared_hash != actual_hash {
        problem(&mut report, format!("_schema_hash is {declared_hash}, the fields hash to {actual_hash}"));
    }

    let mut frame_records_total: u64 = 0;
    // Positionally, never by id: ids repeat legitimately when a caller
    // partitions by hour and a spill closes under hour 0 after hour 23, and an
    // id-keyed sweep would read the first twin twice and never see the second.
    let frame_count = container.frames.len();
    for index in 0..frame_count {
        let frame = container
            .frames
            .get(index)
            .copied()
            .unwrap_or(Frame { id: 0, offset: 0, len: 0, hash: 0 });
        let frame_id = frame.id;
        report.frames = report.frames.saturating_add(1);
        if frame.hash != 0 {
            match read_at(
                &mut container.file,
                container.data_start.saturating_add(frame.offset),
                to_usize(frame.len)?,
            ) {
                Ok(bytes) => {
                    let actual = xxh64(&bytes);
                    if actual != frame.hash {
                        problem(
                            &mut report,
                            format!(
                                "frame {frame_id}: checksum mismatch, _frames says {:016x}, the bytes hash to {actual:016x}",
                                frame.hash
                            ),
                        );
                    }
                }
                Err(error) => problem(&mut report, format!("frame {frame_id}: {error}")),
            }
        }
        match container.read_frame_at(index) {
            Ok(raw) => {
                let (records, leftover) = split_records(&raw, container.schema.layout);
                if leftover > 0 {
                    problem(&mut report, format!("frame {frame_id}: {leftover} torn bytes"));
                }
                frame_records_total = frame_records_total.saturating_add(records.len() as u64);
                report.records = report.records.saturating_add(records.len() as u64);
                if let Some(callback) = check.as_mut() {
                    for record in records {
                        if let Some(text) = callback(record) {
                            problem(&mut report, format!("frame {frame_id}: {text}"));
                        }
                    }
                }
            }
            Err(error) => problem(&mut report, format!("frame {frame_id}: {error}")),
        }
    }

    match container.read_tail() {
        Ok(raw) => {
            let (records, _leftover) = split_records(&raw, container.schema.layout);
            report.records = report.records.saturating_add(records.len() as u64);
            if let Some(callback) = check.as_mut() {
                for record in records {
                    if let Some(text) = callback(record) {
                        problem(&mut report, format!("tail: {text}"));
                    }
                }
            }
        }
        Err(error) => problem(&mut report, format!("tail: {error}")),
    }

    // `_records` covers closed frames only; tail records are the not yet
    // closed remainder on top and never a mismatch.
    let declared_records: u64 = container.headers.get("_records").unwrap_or("0").parse().unwrap_or(0);
    if declared_records != frame_records_total {
        problem(
            &mut report,
            format!("_records says {declared_records}, the frames hold {frame_records_total}"),
        );
    }
    Ok(report)
}

/// What [`rebuild_headers`] found and did.
#[derive(Debug, Default)]
pub struct RebuildReport {
    /// Header values that disagreed with the data, `key: stored -> actual`.
    pub differences: Vec<String>,
    /// Whether the zone was rewritten.
    pub fixed: bool,
}

/// Rescans the data section by walking zstd frames, recomputes `_frames`,
/// `_records` and `_bytes_raw`, and rewrites the zone when `fix` is set.
///
/// Frame ids cannot be recovered from the bytes alone, so ids of frames
/// already present keep their stored value and newly discovered frames are
/// numbered on from the largest known id.
///
/// # Errors
/// Returns a message when the file cannot be opened or the fix cannot
/// write.
pub fn rebuild_headers(path: &Path, fix: bool) -> Result<RebuildReport> {
    replay_journal(path)?;
    let mut container = Container::open_inner(path, fix)?;
    let data_len = container.file_len.saturating_sub(container.data_start);

    let mut actual_frames: Vec<Frame> = Vec::new();
    let mut records: u64 = 0;
    let mut bytes_raw: u64 = 0;
    let mut position = 0usize;
    let mut next_id = container
        .frames
        .iter()
        .map(|frame| frame.id.saturating_add(1))
        .max()
        .unwrap_or(0);
    while (position as u64) < data_len {
        let absolute = container.data_start.saturating_add(position as u64);
        if read_at(&mut container.file, absolute, 4)? != ZSTD_MAGIC.as_slice() {
            break;
        }
        // One frame at a time, read in growing windows rather than by loading
        // the whole data section: the format is meant for files that outgrow
        // memory, so a maintenance pass that does not is a maintenance pass
        // nobody can run on the files that need it.
        let (consumed, raw) = decode_frame_at(&mut container.file, absolute, data_len.saturating_sub(position as u64))?;
        // Recomputing the hash here is what upgrades a file written before the
        // field existed: `rebuild_headers(path, true)` rewrites `_frames` with
        // the fourth field filled in.
        let hash = xxh64(&read_at(&mut container.file, absolute, consumed)?);
        let (frame_records, leftover) = split_records(&raw, container.schema.layout);
        if leftover > 0 {
            return Err(malformed(format!(
                "frame at offset {position}: {leftover} torn bytes inside a closed frame"
            )));
        }
        records = records.saturating_add(frame_records.len() as u64);
        bytes_raw = bytes_raw.saturating_add(raw.len() as u64);
        let id = container.frames.get(actual_frames.len()).map_or_else(
            || {
                let id = next_id;
                next_id = next_id.saturating_add(1);
                id
            },
            |frame| frame.id,
        );
        actual_frames.push(Frame { id, offset: position as u64, len: consumed as u64, hash });
        position = position.saturating_add(consumed);
    }
    // The raw tail is deliberately left out of the counters: `_records`
    // and `_bytes_raw` publish closed frames only.
    let mut report = RebuildReport::default();
    let stored_frames = render_frames(&container.frames);
    let found_frames = render_frames(&actual_frames);
    if stored_frames != found_frames {
        report
            .differences
            .push(format!("_frames: {stored_frames:?} -> {found_frames:?}"));
    }
    let stored_records = container.headers.get("_records").unwrap_or("0").to_owned();
    if stored_records != records.to_string() {
        report
            .differences
            .push(format!("_records: {stored_records} -> {records}"));
    }
    let stored_raw = container.headers.get("_bytes_raw").unwrap_or("0").to_owned();
    if stored_raw != bytes_raw.to_string() {
        report
            .differences
            .push(format!("_bytes_raw: {stored_raw} -> {bytes_raw}"));
    }

    if fix && !report.differences.is_empty() {
        container.frames = actual_frames;
        container.records = records;
        container.bytes_raw = bytes_raw;
        container.records_closed = records;
        container.bytes_closed = bytes_raw;
        container.headers.set("_frames", &found_frames)?;
        container.flush_headers()?;
        report.fixed = true;
    }
    Ok(report)
}

/// Reads exactly `len` bytes at an absolute offset, short reads included.
fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; len];
    file.seek(SeekFrom::Start(offset)).map_err(Error::from)?;
    let mut filled = 0usize;
    while filled < len {
        let read = file
            .read(buffer.get_mut(filled..).unwrap_or_default())
            .map_err(Error::from)?;
        if read == 0 {
            buffer.truncate(filled);
            return Ok(buffer);
        }
        filled = filled.saturating_add(read);
    }
    Ok(buffer)
}

/// Decodes the frame starting at `offset`, reading it in growing windows.
///
/// A zstd frame does not announce its compressed length, so the window starts
/// small and doubles until the frame decodes. The cost is re-reading and
/// re-decoding a frame at most twice over; the gain is that memory is bounded
/// by one frame instead of by the file.
fn decode_frame_at(file: &mut File, offset: u64, available: u64) -> Result<(usize, Vec<u8>)> {
    let mut window = 256usize * 1024;
    loop {
        let want = to_usize((window as u64).min(available))?;
        let chunk = read_at(file, offset, want)?;
        match decode_one_frame(&chunk) {
            Ok(result) => return Ok(result),
            Err(error) => {
                if chunk.len() as u64 >= available {
                    return Err(error);
                }
                window = window
                    .checked_mul(2)
                    .ok_or_else(|| malformed("frame is impossibly large"))?;
            }
        }
    }
}

/// Decompresses one frame, refusing to produce more than
/// [`MAX_FRAME_RAW_BYTES`].
///
/// A compressor is free to turn a few kilobytes into terabytes, and a reader
/// that trusts it hands the process to whoever wrote the file.
fn decode_bounded(compressed: &[u8]) -> Result<Vec<u8>> {
    // A frame that does not begin with the zstd magic was stored uncompressed.
    // Deciding by the bytes rather than by a header keeps the two kinds
    // interchangeable inside one file, and keeps a reader from having to trust
    // a header that may have been rewritten.
    if compressed.get(0..4) != Some(ZSTD_MAGIC.as_slice()) {
        return Ok(compressed.to_vec());
    }
    let (_consumed, raw) = decode_one_frame(compressed)?;
    Ok(raw)
}

/// Decodes exactly one zstd frame, returning (consumed bytes, raw bytes).
fn decode_one_frame(input: &[u8]) -> Result<(usize, Vec<u8>)> {
    let mut context = zstd::zstd_safe::DCtx::create();
    let mut in_buffer = zstd::zstd_safe::InBuffer::around(input);
    let mut raw = Vec::new();
    let mut scratch = vec![0u8; 128 * 1024];
    loop {
        let mut out_buffer = zstd::zstd_safe::OutBuffer::around(scratch.as_mut_slice());
        let hint = context
            .decompress_stream(&mut out_buffer, &mut in_buffer)
            .map_err(|code| Error::Compression(zstd::zstd_safe::get_error_name(code).to_owned()))?;
        raw.extend_from_slice(out_buffer.as_slice());
        if raw.len() as u64 > MAX_FRAME_RAW_BYTES {
            return Err(malformed(format!("frame decompresses to more than {MAX_FRAME_RAW_BYTES} bytes")));
        }
        if hint == 0 {
            return Ok((in_buffer.pos(), raw));
        }
        if out_buffer.pos() == 0 && in_buffer.pos() == input.len() {
            return Err(malformed("truncated zstd frame"));
        }
    }
}

/// What [`repack`] measured.
#[derive(Debug, Default)]
pub struct RepackReport {
    /// The file size before, bytes.
    pub bytes_before: u64,
    /// The file size after, bytes.
    pub bytes_after: u64,
    /// Frames re-encoded.
    pub frames: usize,
}

/// Re-encodes every closed frame at `level`, transactionally per file.
///
/// The new version is assembled next to the original (`<file>.repack`) and
/// atomically renamed over it; a failure at any point leaves the original
/// untouched. Frame boundaries and ids survive; the raw tail, when present,
/// is carried over as is.
///
/// # Errors
/// Returns a message on any read, encode or write failure.
pub fn repack(path: &Path, level: i32) -> Result<RepackReport> {
    let area = peek_headers(path)?.0.header_area;
    repack_with_header_area(path, level, area)
}

/// Re-encodes every closed frame at `level` and gives the rebuilt file a
/// header zone of `header_area` bytes.
///
/// This is the way out of [`Error::ZoneFull`]. The frame list grows with every
/// frame closed and the zone it lives in cannot move, so a file written to for
/// long enough eventually cannot record another frame. Repacking rebuilds the
/// file from scratch, which is the one moment the zone can be a different size.
/// [`max_frames_for`] says how far a given size goes.
///
/// The new version is assembled next to the original (`<file>.repack`) and
/// atomically renamed over it; a failure at any point leaves the original
/// untouched. Frames are re-encoded and written one at a time, so the memory
/// this needs is the size of the largest frame rather than the size of the
/// file. Frame boundaries and ids survive; the raw tail, when present, is
/// carried over as is.
///
/// # Errors
/// Returns a message on any read, encode or write failure, and
/// [`Error::ZoneFull`] when the headers do not fit the requested zone.
pub fn repack_with_header_area(path: &Path, level: i32, header_area: u32) -> Result<RepackReport> {
    replay_journal(path)?;
    if header_area == 0 || header_area > MAX_HEADER_AREA {
        return Err(usage(format!("header zone of {header_area} bytes is outside 1..={MAX_HEADER_AREA}")));
    }
    let mut container = Container::open_read(path)?;
    let mut report = RepackReport { bytes_before: container.file_len, ..RepackReport::default() };

    let staging = {
        let mut name = path.file_name().map(ToOwned::to_owned).unwrap_or_default();
        name.push(".repack");
        path.with_file_name(name)
    };
    // The flag has to follow the level, not the file it came from: leaving it
    // set on a file that was just rewritten uncompressed makes the preamble
    // claim something the bytes contradict.
    let preamble = Preamble {
        version: VERSION,
        flags: if level == NO_COMPRESSION { 0 } else { FLAG_COMPRESSED },
        header_area,
    };

    let result = (|| -> Result<(u64, usize)> {
        let mut out = File::create(&staging).map_err(io_at(&staging))?;
        out.write_all(&preamble.encode()).map_err(Error::from)?;
        // A placeholder of the right size: the real zone needs the frame list,
        // and the frame list needs the frames written. The zone has a fixed
        // size and a fixed position, so it can be filled in at the end.
        out.write_all(&vec![0u8; header_area as usize]).map_err(Error::from)?;

        let mut new_frames: Vec<Frame> = Vec::new();
        let mut offset: u64 = 0;
        let frame_list = container.frames.clone();
        for frame in frame_list {
            let raw = container.read_frame(frame.id)?;
            let compressed = compress_frame(&raw, level)?;
            out.write_all(&compressed).map_err(Error::from)?;
            new_frames.push(Frame {
                id: frame.id,
                offset,
                len: compressed.len() as u64,
                hash: xxh64(&compressed),
            });
            offset = offset.saturating_add(compressed.len() as u64);
        }
        let frames_written = new_frames.len();

        let tail = container.read_tail()?;
        out.write_all(&tail).map_err(Error::from)?;

        let mut headers = container.headers.clone();
        headers.set("_frames", &render_frames(&new_frames))?;
        headers.set("_compression_level", &level.to_string())?;
        headers.set("_compression", if level == NO_COMPRESSION { "none" } else { "zstd" })?;
        let zone = headers.render(header_area as usize)?;
        out.seek(SeekFrom::Start(PREAMBLE_BYTES as u64))
            .map_err(Error::from)?;
        out.write_all(&zone).map_err(Error::from)?;
        out.sync_all().map_err(Error::from)?;

        let len = out.metadata().map_err(Error::from)?.len();
        Ok((len, frames_written))
    })();

    match result {
        Ok((len, frames)) => {
            std::fs::rename(&staging, path).map_err(Error::from)?;
            sync_parent_dir(path);
            report.bytes_after = len;
            report.frames = frames;
            Ok(report)
        }
        Err(error) => {
            let _ignored = std::fs::remove_file(&staging);
            Err(error)
        }
    }
}

/// One field of one record, decoded according to the schema.
///
/// The format stores bytes and a description of them; this is the description
/// applied. Anything the schema names with a type this does not know comes
/// back as [`FieldValue::Unknown`] rather than as a guess.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FieldValue {
    /// An unsigned integer, whatever its declared width.
    Unsigned(u64),
    /// A signed integer.
    Signed(i64),
    /// A double.
    Float(f64),
    /// Sixteen bytes; `Display` renders them as hex.
    Uuid([u8; 16]),
    /// The schema declares a type this build does not decode.
    Unknown,
}

impl std::fmt::Display for FieldValue {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsigned(value) => write!(out, "{value}"),
            Self::Signed(value) => write!(out, "{value}"),
            Self::Float(value) => write!(out, "{value}"),
            Self::Uuid(bytes) => {
                for byte in bytes {
                    write!(out, "{byte:02x}")?;
                }
                Ok(())
            }
            Self::Unknown => write!(out, "?"),
        }
    }
}

/// Reads one field out of one record.
///
/// Returns `None` when the record is too short for the field the schema
/// declares — which is a real thing that happens to a file whose schema was
/// changed without rewriting the data, and is worth telling apart from a field
/// that is genuinely zero.
#[must_use]
pub fn read_field(record: &[u8], field: &FieldSpec) -> Option<FieldValue> {
    let offset = field.offset as usize;
    let take = |width: usize| record.get(offset..offset.checked_add(width)?);
    Some(match field.ty.as_str() {
        "u8" => FieldValue::Unsigned(u64::from(*record.get(offset)?)),
        "u16" => FieldValue::Unsigned(u64::from(u16::from_le_bytes(take(2)?.try_into().ok()?))),
        "u32" => FieldValue::Unsigned(u64::from(u32::from_le_bytes(take(4)?.try_into().ok()?))),
        "u64" => FieldValue::Unsigned(u64::from_le_bytes(take(8)?.try_into().ok()?)),
        "i64" => FieldValue::Signed(i64::from_le_bytes(take(8)?.try_into().ok()?)),
        "f64" => FieldValue::Float(f64::from_le_bytes(take(8)?.try_into().ok()?)),
        "uuid" => FieldValue::Uuid(take(16)?.try_into().ok()?),
        _other => FieldValue::Unknown,
    })
}

/// Roughly how many frames a header zone of `header_area` bytes can list.
///
/// The frame list is text, so the answer depends on how large the offsets
/// get: a file of a few megabytes spends fewer digits per frame than one of a
/// few terabytes. This assumes the largest offsets the given zone can lead to
/// and is therefore a floor, not an estimate — a real file fits at least this
/// many.
///
/// It exists because the alternative is finding the limit in production, on
/// the day a long-lived writer stops being able to close a frame.
#[must_use]
pub fn max_frames_for(header_area: u32) -> u64 {
    // The system headers other than `_frames`, generously: schema, source,
    // writer, timestamps, counters, and the preview at its full budget.
    const FIXED_OVERHEAD: u64 = 1600 + PREVIEW_BUDGET as u64;
    // `id,offset,len;` with room for 20-digit ids, offsets and lengths.
    const PER_FRAME: u64 = 64;
    u64::from(header_area)
        .saturating_sub(FIXED_OVERHEAD)
        .checked_div(PER_FRAME)
        .unwrap_or(0)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing test reports itself by panicking, and an index that is wrong is the assertion"
)]
mod tests {
    use super::*;

    fn sample_schema() -> Schema {
        Schema {
            name: "samples@1".to_owned(),
            fields: vec![
                FieldSpec {
                    name: "time_nanos".to_owned(),
                    ty: "u64".to_owned(),
                    offset: 0,
                },
                FieldSpec { name: "value".to_owned(), ty: "f64".to_owned(), offset: 8 },
                FieldSpec { name: "weight".to_owned(), ty: "f64".to_owned(), offset: 16 },
                FieldSpec { name: "flags".to_owned(), ty: "u8".to_owned(), offset: 24 },
            ],
            layout: RecordLayout::Fixed(25),
        }
    }

    fn record(time: u64, value: f64, weight: f64, flags: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(25);
        out.extend_from_slice(&time.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
        out.extend_from_slice(&weight.to_le_bytes());
        out.push(flags);
        out
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let mut root = std::env::temp_dir();
        root.push(format!("bizstd-{tag}-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_full_life_roundtrip_survives_reopen() {
        let root = scratch_dir("roundtrip");
        let path = root.join("day.bizstd");
        let schema = sample_schema();
        let mut file = Container::create(
            &path,
            &schema,
            "example source",
            "bizstd test",
            1_754_000_000_000,
            DEFAULT_HEADER_AREA,
            &[("stream", "alpha"), ("region", "north")],
        )
        .unwrap();
        for index in 0..100u64 {
            file.append_record(&record(index, 100.5, 0.25, 1)).unwrap();
        }
        file.close_frame(0, HOT_LEVEL).unwrap();
        for index in 100..150u64 {
            file.append_record(&record(index, 101.5, 0.5, 0)).unwrap();
        }
        file.seal(1, HOT_LEVEL).unwrap();
        drop(file);

        let mut back = Container::open_read(&path).unwrap();
        assert_eq!(back.headers().get("_sealed"), Some("true"));
        assert_eq!(back.headers().get("_records"), Some("150"));
        assert_eq!(back.headers().get("stream"), Some("alpha"));
        assert_eq!(back.frames().len(), 2);
        assert!(!back.headers().get("_preview").unwrap().is_empty());
        let first = back.read_frame(0).unwrap();
        let (records, leftover) = split_records(&first, back.schema().layout);
        assert_eq!((records.len(), leftover), (100, 0));
        let second = back.read_frame(1).unwrap();
        let (records, _leftover) = split_records(&second, back.schema().layout);
        assert_eq!(records.len(), 50);
        assert!(back.read_tail().unwrap().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn escaping_roundtrips_and_underscore_keys_are_fenced() {
        assert_eq!(unescape_value(&escape_value("a\\b\nc")), "a\\b\nc");
        let mut headers = Headers::default();
        assert!(headers.set_user("_records", "1").is_err());
        assert!(headers.set_user("note", "line one\nline two").is_ok());
        let zone = headers.render(256).unwrap();
        let back = Headers::parse(&zone).unwrap();
        assert_eq!(back.get("note"), Some("line one\nline two"));
        assert!(!key_is_valid("Note"));
        assert!(!key_is_valid("_"));
        assert!(key_is_valid("_schema"));
        assert!(key_is_valid("gap_hours"));
    }

    #[test]
    fn a_torn_tail_record_is_cut_on_reopen() {
        let root = scratch_dir("torn");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        file.append_record(&record(1, 1.0, 1.0, 0)).unwrap();
        file.flush_data().unwrap();
        drop(file);
        // A crash mid-write leaves half a record on disk.
        let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
        raw.write_all(&[0xAA; 10]).unwrap();
        drop(raw);

        let mut back = Container::open_append(&path).unwrap();
        let tail = back.read_tail().unwrap();
        let (records, leftover) = split_records(&tail, RecordLayout::Fixed(25));
        assert_eq!((records.len(), leftover), (1, 0));
        assert_eq!(back.records(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_complete_seal_journal_is_replayed() {
        let root = scratch_dir("journal");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..10u64 {
            file.append_record(&record(index, 2.0, 1.0, 0)).unwrap();
        }
        file.flush_data().unwrap();
        drop(file);

        // Build the journal by hand, as if the process died right after
        // writing it and before the splice.
        let mut reader = Container::open_read(&path).unwrap();
        let tail = reader.read_tail().unwrap();
        let tail_start = reader.tail_start();
        let compressed = zstd::stream::encode_all(tail.as_slice(), HOT_LEVEL).unwrap();
        let mut headers = reader.headers().clone();
        headers
            .set(
                "_frames",
                &render_frames(&[Frame { id: 0, offset: 0, len: compressed.len() as u64, hash: 0 }]),
            )
            .unwrap();
        headers.set("_records", "10").unwrap();
        headers.set("_bytes_raw", "250").unwrap();
        let zone = headers.render(DEFAULT_HEADER_AREA as usize).unwrap();
        write_journal(&path, tail_start, &zone, &compressed).unwrap();
        drop(reader);

        let mut back = Container::open_append(&path).unwrap();
        assert_eq!(back.frames().len(), 1, "the journal must have been replayed");
        assert_eq!(back.headers().get("_records"), Some("10"));
        let raw = back.read_frame(0).unwrap();
        let (records, _leftover) = split_records(&raw, RecordLayout::Fixed(25));
        assert_eq!(records.len(), 10);
        assert!(!journal_path(&path).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn prefixed_records_roundtrip() {
        let root = scratch_dir("prefixed");
        let path = root.join("book.bizstd");
        let schema = Schema {
            name: "book@1".to_owned(),
            // Offset 0, where the schema says the first field is. A splitter
            // that hands back the length prefix along with the body shifts
            // every field read by two, which is the defect this pins.
            fields: vec![FieldSpec { name: "n_levels".to_owned(), ty: "u16".to_owned(), offset: 0 }],
            layout: RecordLayout::Prefixed,
        };
        let mut file = Container::create(&path, &schema, "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        file.append_record(&[1, 2, 3]).unwrap();
        file.append_record(&[4, 5, 6, 7, 8]).unwrap();
        file.seal(0, HOT_LEVEL).unwrap();
        drop(file);

        let mut back = Container::open_read(&path).unwrap();
        let raw = back.read_frame(0).unwrap();
        let (records, leftover) = split_records(&raw, RecordLayout::Prefixed);
        assert_eq!(leftover, 0);
        assert_eq!(records.len(), 2);
        assert_eq!(*records.first().unwrap(), [1u8, 2, 3].as_slice(), "the body, prefix stripped");
        assert_eq!(*records.get(1).unwrap(), [4u8, 5, 6, 7, 8].as_slice());
        // The field the schema declares reads at its declared offset.
        let first = records.first().unwrap();
        let n_levels = u16::from_le_bytes(first.get(0..2).unwrap().try_into().unwrap());
        assert_eq!(n_levels, u16::from_le_bytes([1, 2]));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn validate_flags_a_lying_counter_and_rebuild_fixes_it() {
        let root = scratch_dir("rebuild");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..20u64 {
            file.append_record(&record(index, 3.0, 1.0, 0)).unwrap();
        }
        file.close_frame(0, HOT_LEVEL).unwrap();
        // Lie in the headers.
        file.records_closed = 999;
        file.flush_headers().unwrap();
        drop(file);

        let report = validate(&path, None).unwrap();
        assert!(report.problems.iter().any(|text| text.contains("_records")), "{:?}", report.problems);

        let rebuilt = rebuild_headers(&path, true).unwrap();
        assert!(rebuilt.fixed, "{rebuilt:?}");
        let clean = validate(&path, None).unwrap();
        assert!(clean.problems.is_empty(), "{:?}", clean.problems);
        assert_eq!(clean.records, 20);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn repack_changes_the_level_and_keeps_every_record() {
        let root = scratch_dir("repack");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..500u64 {
            file.append_record(&record(index, 4.0, 2.0, 1)).unwrap();
        }
        file.close_frame(0, HOT_LEVEL).unwrap();
        for index in 500..600u64 {
            file.append_record(&record(index, 5.0, 2.0, 0)).unwrap();
        }
        file.flush_data().unwrap();
        drop(file);

        let report = repack(&path, COLD_LEVEL).unwrap();
        assert_eq!(report.frames, 1);
        let mut back = Container::open_read(&path).unwrap();
        assert_eq!(back.headers().get("_compression_level"), Some("19"));
        let raw = back.read_frame(0).unwrap();
        let (records, _leftover) = split_records(&raw, RecordLayout::Fixed(25));
        assert_eq!(records.len(), 500, "frame records survive the repack");
        let tail = back.read_tail().unwrap();
        let (tail_records, _leftover) = split_records(&tail, RecordLayout::Fixed(25));
        assert_eq!(tail_records.len(), 100, "the raw tail is carried over");
        let clean = validate(&path, None).unwrap();
        assert!(clean.problems.is_empty(), "{:?}", clean.problems);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_content_callback_reaches_every_record() {
        let root = scratch_dir("content");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        file.append_record(&record(7, -1.0, 1.0, 0)).unwrap();
        file.seal(0, HOT_LEVEL).unwrap();
        drop(file);

        let mut negative = |bytes: &[u8]| -> Option<String> {
            let price = bytes.get(8..16)?.try_into().ok().map(f64::from_le_bytes)?;
            (price < 0.0).then(|| format!("negative price {price}"))
        };
        let report = validate(&path, Some(&mut negative)).unwrap();
        assert!(
            report.problems.iter().any(|text| text.contains("negative price")),
            "{:?}",
            report.problems
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rfc3339_renders_a_known_instant() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_754_000_000_000), "2025-07-31T22:13:20Z");
    }

    #[test]
    fn a_torn_journal_is_discarded_instead_of_replayed() {
        let root = scratch_dir("torn-journal");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..50u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.flush_data().unwrap();
        drop(file);

        // A journal written by a process that died halfway: every field parses,
        // the frame bytes are simply cut short.
        let zone = std::fs::read(&path).unwrap();
        let head = zone
            .get(PREAMBLE_BYTES..PREAMBLE_BYTES + DEFAULT_HEADER_AREA as usize)
            .unwrap();
        let tail_start = u64::from(DEFAULT_HEADER_AREA) + PREAMBLE_BYTES as u64;
        let compressed = zstd::stream::encode_all([7u8; 400].as_slice(), HOT_LEVEL).unwrap();
        write_journal(&path, tail_start, head, &compressed).unwrap();
        let journal = journal_path(&path);
        let whole = std::fs::read(&journal).unwrap();
        std::fs::write(&journal, whole.get(..whole.len() - 32).unwrap()).unwrap();

        let mut back = Container::open_append(&path).unwrap();
        assert!(!journal.exists(), "the torn journal is removed, not replayed");
        let tail = back.read_tail().unwrap();
        let (records, leftover) = split_records(&tail, RecordLayout::Fixed(25));
        assert_eq!((records.len(), leftover), (50, 0), "the intact tail survives");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_full_zone_is_its_own_error_and_repacking_widens_it() {
        let root = scratch_dir("zone-full");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        let mut closed = 0u64;
        let error = loop {
            file.append_record(&record(closed, 1.0, 1.0, 0)).unwrap();
            match file.close_frame(closed, HOT_LEVEL) {
                Ok(()) => closed += 1,
                Err(error) => break error,
            }
            assert!(closed < 10_000, "the zone has to fill up eventually");
        };
        assert!(matches!(error, Error::ZoneFull { .. }), "got {error:?}");
        assert!(closed >= max_frames_for(DEFAULT_HEADER_AREA), "the advertised floor holds");
        drop(file);

        let report = repack_with_header_area(&path, COLD_LEVEL, 64 * 1024).unwrap();
        assert_eq!(report.frames as u64, closed);
        let mut wider = Container::open_append(&path).unwrap();
        wider.append_record(&record(closed, 1.0, 1.0, 0)).unwrap();
        wider.close_frame(closed, HOT_LEVEL).unwrap();
        assert_eq!(wider.frames().len() as u64, closed + 1, "writing resumes after the widening");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_lying_header_area_is_refused_rather_than_allocated() {
        let root = scratch_dir("bounds");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        file.append_record(&record(1, 1.0, 1.0, 0)).unwrap();
        file.seal(0, HOT_LEVEL).unwrap();
        drop(file);

        let mut bytes = std::fs::read(&path).unwrap();
        // A header zone of 4 GiB in a file of a few hundred bytes.
        bytes.get_mut(8..12).unwrap().copy_from_slice(&u32::MAX.to_le_bytes());
        let hostile = root.join("hostile.bizstd");
        std::fs::write(&hostile, &bytes).unwrap();
        let error = peek_headers(&hostile).unwrap_err();
        assert!(matches!(error, Error::Malformed(_)), "got {error:?}");

        // And one that is within the ceiling but still larger than the file.
        bytes
            .get_mut(8..12)
            .unwrap()
            .copy_from_slice(&(1024u32 * 1024).to_le_bytes());
        std::fs::write(&hostile, &bytes).unwrap();
        assert!(matches!(peek_headers(&hostile).unwrap_err(), Error::Malformed(_)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn both_counters_survive_a_reopen_with_a_live_tail() {
        let root = scratch_dir("counters");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..100u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.flush_data().unwrap();
        drop(file);

        let mut back = Container::open_append(&path).unwrap();
        for index in 100..150u64 {
            back.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        back.close_frame(0, HOT_LEVEL).unwrap();
        assert_eq!(back.headers().get("_records"), Some("150"));
        assert_eq!(back.headers().get("_bytes_raw"), Some("3750"), "bytes recover like records do");
        drop(back);
        assert!(rebuild_headers(&path, false).unwrap().differences.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dropping_a_writer_still_writes_the_buffer() {
        let root = scratch_dir("drop");
        let path = root.join("day.bizstd");
        {
            let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
            for index in 0..10u64 {
                file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
            }
            // No flush, no close: the ordinary shape of an early return.
        }
        let mut back = Container::open_append(&path).unwrap();
        let tail = back.read_tail().unwrap();
        let (records, _leftover) = split_records(&tail, RecordLayout::Fixed(25));
        assert_eq!(records.len(), 10, "the buffer reached the disk");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn maintenance_replays_a_pending_journal_first() {
        let root = scratch_dir("maintenance-journal");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..40u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.flush_data().unwrap();
        drop(file);

        // Reproduce the state a crash between journal and splice leaves behind.
        let mut staged = Container::open_append(&path).unwrap();
        let tail_start = staged.tail_start();
        let raw = staged.read_tail().unwrap();
        let compressed = zstd::stream::encode_all(raw.as_slice(), HOT_LEVEL).unwrap();
        let mut headers = staged.headers().clone();
        headers
            .set(
                "_frames",
                &render_frames(&[Frame { id: 0, offset: 0, len: compressed.len() as u64, hash: 0 }]),
            )
            .unwrap();
        headers.set("_records", "40").unwrap();
        headers.set("_bytes_raw", &(40u64 * 25).to_string()).unwrap();
        let zone = headers.render(DEFAULT_HEADER_AREA as usize).unwrap();
        write_journal(&path, tail_start, &zone, &compressed).unwrap();
        drop(staged);

        let report = validate(&path, None).unwrap();
        assert!(!journal_path(&path).exists(), "validate replayed it");
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.frames, 1);
        assert_eq!(report.records, 40);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn xxh64_matches_the_reference_vectors() {
        // From the xxHash specification, seed 0.
        assert_eq!(xxh64(b""), 0xEF46_DB37_51D8_E999);
        assert_eq!(xxh64(b"abc"), 0x44BC_2CF5_AD77_0999);
        // Longer than 32 bytes, so the four-lane path runs.
        assert_eq!(xxh64(b"Nobody inspects the spammish repetition"), 0xFBCE_A83C_8A37_8BF1);
    }

    #[test]
    fn a_frame_index_written_before_checksums_round_trips_unchanged() {
        let old = "0,0,100;1,100,200";
        let frames = parse_frames(old).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|frame| frame.hash == 0), "unrecorded stays unrecorded");
        assert_eq!(render_frames(&frames), old, "an untouched old index must not change shape");

        let hashed = "0,0,100,00000000deadbeef";
        let parsed = parse_frames(hashed).unwrap();
        assert_eq!(parsed.first().unwrap().hash, 0xdead_beef);
        assert_eq!(render_frames(&parsed), hashed);
    }

    #[test]
    fn a_corrupted_frame_is_caught_by_its_checksum() {
        let root = scratch_dir("checksum");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..100u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.seal(0, HOT_LEVEL).unwrap();
        let frame = *file.frames().first().unwrap();
        assert_ne!(frame.hash, 0, "a frame written now records its hash");
        drop(file);
        assert!(validate(&path, None).unwrap().problems.is_empty());

        // Flip one byte inside the compressed frame.
        let mut bytes = std::fs::read(&path).unwrap();
        let target = to_usize(PREAMBLE_BYTES as u64 + u64::from(DEFAULT_HEADER_AREA) + frame.offset + 8).unwrap();
        *bytes.get_mut(target).unwrap() ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let report = validate(&path, None).unwrap();
        assert!(
            report.problems.iter().any(|text| text.contains("checksum mismatch")),
            "{:?}",
            report.problems
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn repeated_frame_ids_are_swept_by_position() {
        let root = scratch_dir("twins");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        // Hour 23, then a midnight spill closed under hour 0: two frames, and
        // one of the ids appears twice across the pair below.
        for index in 0..10u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.close_frame(0, HOT_LEVEL).unwrap();
        for index in 10..30u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.seal(0, HOT_LEVEL).unwrap();
        drop(file);

        let mut back = Container::open_read(&path).unwrap();
        assert_eq!(back.frames().len(), 2, "two frames, both with id 0");
        let first = back.read_frame_at(0).unwrap();
        let second = back.read_frame_at(1).unwrap();
        assert_eq!(split_records(&first, RecordLayout::Fixed(25)).0.len(), 10);
        assert_eq!(split_records(&second, RecordLayout::Fixed(25)).0.len(), 20);
        assert!(back.read_frame_at(2).is_err());
        drop(back);

        // The id-keyed read cannot tell them apart; validate must not rely on it.
        let report = validate(&path, None).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.frames, 2);
        assert_eq!(report.records, 30, "each twin counted once");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rebuilding_fills_in_a_missing_checksum() {
        let root = scratch_dir("upgrade");
        let path = root.join("day.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..20u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.seal(0, HOT_LEVEL).unwrap();
        let frame = *file.frames().first().unwrap();
        drop(file);

        // Rewrite the index the way a pre-checksum writer left it: a triple.
        let mut back = Container::open_append(&path).unwrap();
        back.headers
            .set("_frames", &format!("{},{},{}", frame.id, frame.offset, frame.len))
            .unwrap();
        back.flush_headers().unwrap();
        drop(back);
        let report = rebuild_headers(&path, true).unwrap();
        assert!(report.fixed, "the missing checksum is a difference worth fixing");
        let restored = Container::open_read(&path).unwrap();
        assert_eq!(restored.frames().first().unwrap().hash, frame.hash, "recomputed from the bytes");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn frames_stored_without_compression_read_back() {
        let root = scratch_dir("no-compression");
        let path = root.join("raw.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..100u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.seal(0, NO_COMPRESSION).unwrap();
        let frame = *file.frames().first().unwrap();
        drop(file);

        // Stored as they were appended: the frame is exactly the records.
        assert_eq!(frame.len, 100 * 25, "no compression means no change in size");
        assert_eq!(file_headers(&path)["_compression"], "none");

        let mut back = Container::open_read(&path).unwrap();
        let raw = back.read_frame_at(0).unwrap();
        let (records, leftover) = split_records(&raw, RecordLayout::Fixed(25));
        assert_eq!((records.len(), leftover), (100, 0));
        assert!(validate(&path, None).unwrap().problems.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn one_file_may_hold_both_kinds_of_frame() {
        let root = scratch_dir("mixed");
        let path = root.join("mixed.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..40u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.close_frame(0, NO_COMPRESSION).unwrap();
        for index in 40..90u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.seal(1, HOT_LEVEL).unwrap();
        let frames = file.frames().to_vec();
        drop(file);

        assert_eq!(frames.first().unwrap().len, 40 * 25, "the first frame is raw");
        assert!(frames.get(1).unwrap().len < 50 * 25, "the second is compressed");

        // Which kind a frame is comes from its bytes, so both read the same way.
        let mut back = Container::open_read(&path).unwrap();
        let mut total = 0;
        for index in 0..2 {
            let raw = back.read_frame_at(index).unwrap();
            total += split_records(&raw, RecordLayout::Fixed(25)).0.len();
        }
        assert_eq!(total, 90);
        assert!(validate(&path, None).unwrap().problems.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn repacking_can_drop_compression_and_put_it_back() {
        let root = scratch_dir("repack-levels");
        let path = root.join("levels.bizstd");
        let mut file = Container::create(&path, &sample_schema(), "s", "w", 0, DEFAULT_HEADER_AREA, &[]).unwrap();
        for index in 0..300u64 {
            file.append_record(&record(index, 1.0, 1.0, 0)).unwrap();
        }
        file.seal(0, COLD_LEVEL).unwrap();
        drop(file);
        let compressed_size = std::fs::metadata(&path).unwrap().len();

        let report = repack(&path, NO_COMPRESSION).unwrap();
        assert!(report.bytes_after > compressed_size, "dropping compression grows the file");
        assert_eq!(file_headers(&path)["_compression"], "none");
        assert_eq!(peek_headers(&path).unwrap().0.flags, 0, "the preamble stops claiming compression");
        assert!(validate(&path, None).unwrap().problems.is_empty());

        let report = repack(&path, COLD_LEVEL).unwrap();
        assert_eq!(report.bytes_after, compressed_size, "and putting it back returns the size");
        assert_eq!(file_headers(&path)["_compression"], "zstd");
        assert_eq!(peek_headers(&path).unwrap().0.flags, FLAG_COMPRESSED);
        assert!(validate(&path, None).unwrap().problems.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The headers of a file on disk, as a map, for the assertions above.
    fn file_headers(path: &Path) -> std::collections::HashMap<String, String> {
        peek_headers(path)
            .unwrap()
            .1
            .pairs()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    #[test]
    fn a_field_is_read_by_what_the_schema_says() {
        let schema = sample_schema();
        let body = record(1_700_000_000_000_000_000, 101.25, 3.5, 7);
        let values: Vec<FieldValue> = schema
            .fields
            .iter()
            .map(|field| read_field(&body, field).unwrap())
            .collect();
        assert_eq!(values[0], FieldValue::Unsigned(1_700_000_000_000_000_000));
        assert_eq!(values[1], FieldValue::Float(101.25));
        assert_eq!(values[2], FieldValue::Float(3.5));
        assert_eq!(values[3], FieldValue::Unsigned(7));

        // A record shorter than the schema promises says so rather than
        // guessing, which is what a file outlives its schema looks like.
        assert_eq!(read_field(&body[..4], schema.fields.get(1).unwrap()), None);

        let unknown = FieldSpec { name: "x".to_owned(), ty: "decimal".to_owned(), offset: 0 };
        assert_eq!(read_field(&body, &unknown), Some(FieldValue::Unknown));
    }
}
