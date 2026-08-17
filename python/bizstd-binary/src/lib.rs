//! The compiled half of the Python package.
//!
//! This layer does one thing: move values across the language boundary and
//! turn `bizstd::Error` into the exception that matches it. Every convenience
//! — context managers, iterators, dataclasses — belongs in the pure-Python
//! package, where it can be read, patched and released without a build matrix.
//!
//! The exception types are part of the contract and are defined here so that
//! both halves agree on them: a caller distinguishing a corrupted file from a
//! full header zone must be able to write `except BizstdZoneFullError` rather than
//! matching on the text of a message.

use std::path::PathBuf;

use bizstd::{Container, Error, FieldSpec, Frame, Preamble, RecordLayout, Schema};
use pyo3::exceptions::PyOSError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use pyo3::{create_exception, wrap_pyfunction};

create_exception!(_native, BizstdError, pyo3::exceptions::PyException, "Base of every error this package raises.");
create_exception!(_native, BizstdMalformedError, BizstdError, "The file is not a well-formed container.");
create_exception!(_native, BizstdUsageError, BizstdError, "The calling code asked for something it may not.");
create_exception!(_native, BizstdZoneFullError, BizstdError, "The header zone cannot hold the headers.");
create_exception!(_native, BizstdCompressionError, BizstdError, "zstd refused to compress or decompress.");

/// Maps a crate error onto the exception a caller can catch.
///
/// The variants are not flattened into one type: the whole reason the Rust
/// side has an enum is that a caller acts differently on a full header zone
/// than on a corrupted file, and collapsing them here would throw that away
/// at the boundary.
fn to_py(error: &Error) -> PyErr {
    let text = error.to_string();
    match error {
        Error::Io { .. } => PyOSError::new_err(text),
        Error::Malformed(_) => BizstdMalformedError::new_err(text),
        Error::Usage(_) => BizstdUsageError::new_err(text),
        Error::ZoneFull { .. } => BizstdZoneFullError::new_err(text),
        Error::Compression(_) => BizstdCompressionError::new_err(text),
        _other => BizstdError::new_err(text),
    }
}

fn map<T>(result: bizstd::Result<T>) -> PyResult<T> {
    result.map_err(|error| to_py(&error))
}

/// How a record's bytes are delimited inside a frame.
#[pyclass(module = "bizstd_binary._native", name = "RecordLayout", frozen, eq)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PyRecordLayout {
    /// Bytes per record, or `None` when records carry a length prefix.
    #[pyo3(get)]
    fixed_size: Option<u32>,
}

#[pymethods]
impl PyRecordLayout {
    /// Records of exactly `size` bytes.
    #[staticmethod]
    fn fixed(size: u32) -> Self {
        Self { fixed_size: Some(size) }
    }

    /// Records preceded by a little-endian `u16` length.
    #[staticmethod]
    fn prefixed() -> Self {
        Self { fixed_size: None }
    }

    fn __repr__(&self) -> String {
        match self.fixed_size {
            Some(size) => format!("RecordLayout.fixed({size})"),
            None => "RecordLayout.prefixed()".to_owned(),
        }
    }
}

impl From<PyRecordLayout> for RecordLayout {
    fn from(value: PyRecordLayout) -> Self {
        value.fixed_size.map_or(Self::Prefixed, Self::Fixed)
    }
}

/// One field of a schema.
#[pyclass(module = "bizstd_binary._native", name = "FieldSpec", frozen, eq)]
#[derive(Clone, PartialEq, Eq)]
pub struct PyFieldSpec {
    /// The field's name.
    #[pyo3(get)]
    name: String,
    /// Its type, as the format spells it: `u8`, `u16`, `u32`, `u64`, `i64`,
    /// `f64` or `uuid`.
    #[pyo3(get)]
    ty: String,
    /// Byte offset inside the record.
    #[pyo3(get)]
    offset: u32,
}

#[pymethods]
impl PyFieldSpec {
    #[new]
    fn new(name: String, ty: String, offset: u32) -> Self {
        Self { name, ty, offset }
    }

    fn __repr__(&self) -> String {
        format!("FieldSpec(name={:?}, ty={:?}, offset={})", self.name, self.ty, self.offset)
    }
}

/// What the records in a file look like.
#[pyclass(module = "bizstd_binary._native", name = "Schema", frozen, eq)]
#[derive(Clone, PartialEq, Eq)]
pub struct PySchema {
    /// The schema's name, `name@version` by convention.
    #[pyo3(get)]
    name: String,
    /// Its fields, in declaration order.
    #[pyo3(get)]
    fields: Vec<PyFieldSpec>,
    /// How records are delimited.
    #[pyo3(get)]
    layout: PyRecordLayout,
}

#[pymethods]
impl PySchema {
    #[new]
    fn new(name: String, fields: Vec<PyFieldSpec>, layout: PyRecordLayout) -> Self {
        Self { name, fields, layout }
    }

    /// The FNV-1a 64 fingerprint over the field list, as the file stores it.
    fn hash_hex(&self) -> String {
        Schema::from(self.clone()).hash_hex()
    }

    fn __repr__(&self) -> String {
        format!("Schema(name={:?}, fields={} field(s))", self.name, self.fields.len())
    }
}

impl From<PySchema> for Schema {
    fn from(value: PySchema) -> Self {
        Self {
            name: value.name,
            fields: value
                .fields
                .into_iter()
                .map(|field| FieldSpec { name: field.name, ty: field.ty, offset: field.offset })
                .collect(),
            layout: value.layout.into(),
        }
    }
}

impl From<&Schema> for PySchema {
    fn from(value: &Schema) -> Self {
        Self {
            name: value.name.clone(),
            fields: value
                .fields
                .iter()
                .map(|field| PyFieldSpec {
                    name: field.name.clone(),
                    ty: field.ty.clone(),
                    offset: field.offset,
                })
                .collect(),
            layout: match value.layout {
                RecordLayout::Fixed(size) => PyRecordLayout { fixed_size: Some(size) },
                RecordLayout::Prefixed => PyRecordLayout { fixed_size: None },
            },
        }
    }
}

/// One closed frame in the data section.
#[pyclass(module = "bizstd_binary._native", name = "Frame", frozen, eq)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PyFrame {
    /// The id the writer gave it. Ids may repeat.
    #[pyo3(get)]
    id: u64,
    /// Byte offset from the start of the data section.
    #[pyo3(get)]
    offset: u64,
    /// Compressed length.
    #[pyo3(get)]
    len: u64,
    /// XXH64 of the compressed bytes, `0` when the file predates checksums.
    #[pyo3(get)]
    hash: u64,
}

impl From<Frame> for PyFrame {
    fn from(value: Frame) -> Self {
        Self { id: value.id, offset: value.offset, len: value.len, hash: value.hash }
    }
}

#[pymethods]
impl PyFrame {
    fn __repr__(&self) -> String {
        format!("Frame(id={}, offset={}, len={}, hash={:#018x})", self.id, self.offset, self.len, self.hash)
    }
}

/// The fixed binary head of a file.
#[pyclass(module = "bizstd_binary._native", name = "Preamble", frozen, eq)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PyPreamble {
    /// Container format version.
    #[pyo3(get)]
    version: u8,
    /// Flag bits; bit 0 means the data section is compressed.
    #[pyo3(get)]
    flags: u8,
    /// Size of the header zone in bytes.
    #[pyo3(get)]
    header_area: u32,
}

impl From<Preamble> for PyPreamble {
    fn from(value: Preamble) -> Self {
        Self { version: value.version, flags: value.flags, header_area: value.header_area }
    }
}

#[pymethods]
impl PyPreamble {
    fn __repr__(&self) -> String {
        format!("Preamble(version={}, flags={}, header_area={})", self.version, self.flags, self.header_area)
    }
}

/// What `validate` found.
#[pyclass(module = "bizstd_binary._native", name = "ValidateReport", frozen)]
pub struct PyValidateReport {
    /// Everything wrong, empty when the file is sound.
    #[pyo3(get)]
    problems: Vec<String>,
    /// Closed frames checked.
    #[pyo3(get)]
    frames: usize,
    /// Records seen in frames and the tail.
    #[pyo3(get)]
    records: u64,
}

/// What `rebuild_headers` found and did.
#[pyclass(module = "bizstd_binary._native", name = "RebuildReport", frozen)]
pub struct PyRebuildReport {
    /// Header values that disagreed with the data.
    #[pyo3(get)]
    differences: Vec<String>,
    /// Whether the zone was rewritten.
    #[pyo3(get)]
    fixed: bool,
}

/// What `repack` measured.
#[pyclass(module = "bizstd_binary._native", name = "RepackReport", frozen)]
pub struct PyRepackReport {
    /// File size before, bytes.
    #[pyo3(get)]
    bytes_before: u64,
    /// File size after, bytes.
    #[pyo3(get)]
    bytes_after: u64,
    /// Frames re-encoded.
    #[pyo3(get)]
    frames: usize,
}

/// A container file, open for reading or appending.
#[pyclass(module = "bizstd_binary._native", name = "Container", unsendable)]
pub struct PyContainer {
    inner: Container,
}

#[pymethods]
impl PyContainer {
    /// Creates a file: preamble and header zone, no data yet.
    #[staticmethod]
    #[pyo3(signature = (path, schema, source, writer, created_at_millis=0, header_area=bizstd::DEFAULT_HEADER_AREA, user=None))]
    fn create(
        path: PathBuf,
        schema: PySchema,
        source: &str,
        writer: &str,
        created_at_millis: u64,
        header_area: u32,
        user: Option<Vec<(String, String)>>,
    ) -> PyResult<Self> {
        let pairs = user.unwrap_or_default();
        let borrowed: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let schema = Schema::from(schema);
        let inner = map(Container::create(
            &path,
            &schema,
            source,
            writer,
            created_at_millis,
            header_area,
            &borrowed,
        ))?;
        Ok(Self { inner })
    }

    /// Opens a file read-only. Nothing is recovered and nothing is written.
    #[staticmethod]
    fn open_read(path: PathBuf) -> PyResult<Self> {
        Ok(Self { inner: map(Container::open_read(&path))? })
    }

    /// Opens a file for appending, replaying a pending seal journal and
    /// cutting a torn record off the tail first.
    ///
    /// Takes no lock: one writer per file, and enforcing that is the caller's.
    #[staticmethod]
    fn open_append(path: PathBuf) -> PyResult<Self> {
        Ok(Self { inner: map(Container::open_append(&path))? })
    }

    /// Every header, system and application alike.
    fn headers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (key, value) in self.inner.headers().pairs() {
            dict.set_item(key, value)?;
        }
        Ok(dict)
    }

    /// The schema the file declares.
    fn schema(&self) -> PySchema {
        PySchema::from(self.inner.schema())
    }

    /// The closed frames, in file order.
    fn frames(&self) -> Vec<PyFrame> {
        self.inner.frames().iter().copied().map(PyFrame::from).collect()
    }

    /// Records ever appended, the unflushed buffer included.
    fn records(&self) -> u64 {
        self.inner.records()
    }

    /// Appends one record body.
    fn append_record(&mut self, body: &[u8]) -> PyResult<()> {
        map(self.inner.append_record(body))
    }

    /// Writes whatever is buffered.
    fn flush_data(&mut self) -> PyResult<()> {
        map(self.inner.flush_data())
    }

    /// Rewrites the header zone in place.
    fn flush_headers(&mut self) -> PyResult<()> {
        map(self.inner.flush_headers())
    }

    /// Compresses the raw tail into one frame, crash-safe.
    fn close_frame(&mut self, frame_id: u64, level: i32) -> PyResult<()> {
        map(self.inner.close_frame(frame_id, level))
    }

    /// Closes the tail and marks the file finished.
    fn seal(&mut self, frame_id: u64, level: i32) -> PyResult<()> {
        map(self.inner.seal(frame_id, level))
    }

    /// Sets an application header. Keys may not start with `_`.
    fn set_user_header(&mut self, key: &str, value: &str) -> PyResult<()> {
        map(self.inner.set_user_header(key, value))
    }

    /// Decompresses the frame with this id. Ids may repeat; prefer
    /// `read_frame_at` when sweeping.
    fn read_frame<'py>(&mut self, py: Python<'py>, frame_id: u64) -> PyResult<Bound<'py, PyBytes>> {
        let raw = map(self.inner.read_frame(frame_id))?;
        Ok(PyBytes::new(py, &raw))
    }

    /// Decompresses the frame at this position in the list.
    fn read_frame_at<'py>(&mut self, py: Python<'py>, index: usize) -> PyResult<Bound<'py, PyBytes>> {
        let raw = map(self.inner.read_frame_at(index))?;
        Ok(PyBytes::new(py, &raw))
    }

    /// The uncompressed tail, whole records only.
    fn read_tail<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let raw = map(self.inner.read_tail())?;
        Ok(PyBytes::new(py, &raw))
    }

    fn __repr__(&self) -> String {
        format!("Container(records={}, frames={})", self.inner.records(), self.inner.frames().len())
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&mut self, _args: &Bound<'_, pyo3::types::PyTuple>) -> PyResult<bool> {
        // Flushing here rather than swallowing: a context manager that exits
        // without writing the buffer is the same silent loss the Rust side
        // added a Drop for.
        map(self.inner.flush_data())?;
        Ok(false)
    }
}

/// Preamble and headers without touching the data section.
#[pyfunction]
fn peek_headers<'py>(py: Python<'py>, path: PathBuf) -> PyResult<(PyPreamble, Bound<'py, PyDict>)> {
    let (preamble, headers) = map(bizstd::peek_headers(&path))?;
    let dict = PyDict::new(py);
    for (key, value) in headers.pairs() {
        dict.set_item(key, value)?;
    }
    Ok((PyPreamble::from(preamble), dict))
}

/// Reads every frame, checks the counters, the alignment and the checksums.
#[pyfunction]
fn validate(path: PathBuf) -> PyResult<PyValidateReport> {
    let report = map(bizstd::validate(&path, None))?;
    Ok(PyValidateReport { problems: report.problems, frames: report.frames, records: report.records })
}

/// Derives the system headers from the data, and writes them back when asked.
#[pyfunction]
#[pyo3(signature = (path, fix=false))]
fn rebuild_headers(path: PathBuf, fix: bool) -> PyResult<PyRebuildReport> {
    let report = map(bizstd::rebuild_headers(&path, fix))?;
    Ok(PyRebuildReport { differences: report.differences, fixed: report.fixed })
}

/// Re-encodes every frame at another level, atomically.
#[pyfunction]
#[pyo3(signature = (path, level=bizstd::COLD_LEVEL, header_area=None))]
fn repack(path: PathBuf, level: i32, header_area: Option<u32>) -> PyResult<PyRepackReport> {
    let report = match header_area {
        Some(area) => map(bizstd::repack_with_header_area(&path, level, area))?,
        None => map(bizstd::repack(&path, level))?,
    };
    Ok(PyRepackReport {
        bytes_before: report.bytes_before,
        bytes_after: report.bytes_after,
        frames: report.frames,
    })
}

/// Splits raw bytes into records. Returns the records and the torn leftover.
#[pyfunction]
fn split_records<'py>(
    py: Python<'py>,
    data: &[u8],
    layout: PyRecordLayout,
) -> (Vec<Bound<'py, PyBytes>>, usize) {
    let (records, leftover) = bizstd::split_records(data, layout.into());
    (records.into_iter().map(|record| PyBytes::new(py, record)).collect(), leftover)
}

/// XXH64 with seed 0 — the hash the frame checksums use.
#[pyfunction]
fn xxh64(data: &[u8]) -> u64 {
    bizstd::xxh64(data)
}

/// Roughly how many frames a header zone of this size can list.
#[pyfunction]
fn max_frames_for(header_area: u32) -> u64 {
    bizstd::max_frames_for(header_area)
}

/// The extension module. Everything above, plus the constants a caller needs
/// to avoid hard-coding numbers the format owns.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyContainer>()?;
    module.add_class::<PySchema>()?;
    module.add_class::<PyFieldSpec>()?;
    module.add_class::<PyRecordLayout>()?;
    module.add_class::<PyFrame>()?;
    module.add_class::<PyPreamble>()?;
    module.add_class::<PyValidateReport>()?;
    module.add_class::<PyRebuildReport>()?;
    module.add_class::<PyRepackReport>()?;

    module.add_function(wrap_pyfunction!(peek_headers, module)?)?;
    module.add_function(wrap_pyfunction!(validate, module)?)?;
    module.add_function(wrap_pyfunction!(rebuild_headers, module)?)?;
    module.add_function(wrap_pyfunction!(repack, module)?)?;
    module.add_function(wrap_pyfunction!(split_records, module)?)?;
    module.add_function(wrap_pyfunction!(xxh64, module)?)?;
    module.add_function(wrap_pyfunction!(max_frames_for, module)?)?;

    module.add("BizstdError", module.py().get_type::<BizstdError>())?;
    module.add("BizstdMalformedError", module.py().get_type::<BizstdMalformedError>())?;
    module.add("BizstdUsageError", module.py().get_type::<BizstdUsageError>())?;
    module.add("BizstdZoneFullError", module.py().get_type::<BizstdZoneFullError>())?;
    module.add("BizstdCompressionError", module.py().get_type::<BizstdCompressionError>())?;

    module.add("VERSION", bizstd::VERSION)?;
    module.add("EXTENSION", bizstd::EXTENSION)?;
    module.add("DEFAULT_HEADER_AREA", bizstd::DEFAULT_HEADER_AREA)?;
    module.add("MAX_HEADER_AREA", bizstd::MAX_HEADER_AREA)?;
    module.add("HOT_LEVEL", bizstd::HOT_LEVEL)?;
    module.add("COLD_LEVEL", bizstd::COLD_LEVEL)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
