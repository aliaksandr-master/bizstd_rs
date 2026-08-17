//! Container comparison: the same records stored as every format someone
//! would reasonably reach for instead of writing their own.
//!
//! Size is the easy half. The half that decides the design is what each format
//! costs to **append to** while data is still arriving, and what it costs to
//! read back one slice out of a large file. Parquet and Arrow IPC are
//! excellent at the second and cannot do the first at all: both are written
//! once, from a complete batch, and a file being appended to is not a valid
//! file of either format until it is finished.
//!
//! That is the trade this container exists for, so it is measured rather than
//! asserted.

use std::sync::Arc;
use std::time::Instant;

use arrow::array::{Float64Array, UInt8Array, UInt16Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bizstd::{Container, DEFAULT_HEADER_AREA, FieldSpec, HOT_LEVEL, RecordLayout, split_records};

use crate::data::{Sample, to_csv, to_fixed_bytes, to_jsonl};

/// What one format did with the records.
pub struct Measurement {
    /// The format, as a reader would name it.
    pub name: String,
    /// Bytes the format occupies on disk.
    pub bytes: usize,
    /// Time to write the whole set, seconds.
    pub write_seconds: f64,
    /// Time to read every record back, seconds.
    pub read_seconds: f64,
    /// Whether records can be appended to a finished file without rewriting
    /// it, and whether the file stays readable while that happens.
    pub appendable: bool,
    /// Whether one slice can be read without decoding everything before it.
    pub sliceable: bool,
}

fn timed<T>(work: impl FnOnce() -> T) -> (T, f64) {
    let started = Instant::now();
    let value = work();
    (value, started.elapsed().as_secs_f64())
}

/// The arrow schema matching [`Sample`].
fn arrow_schema() -> Schema {
    Schema::new(vec![
        Field::new("time_nanos", DataType::UInt64, false),
        Field::new("value", DataType::Float64, false),
        Field::new("size", DataType::Float64, false),
        Field::new("stream", DataType::UInt16, false),
        Field::new("flags", DataType::UInt8, false),
    ])
}

fn record_batch(samples: &[Sample]) -> RecordBatch {
    let schema = Arc::new(arrow_schema());
    let columns: Vec<arrow::array::ArrayRef> = vec![
        Arc::new(samples.iter().map(|s| s.time_nanos).collect::<UInt64Array>()),
        Arc::new(samples.iter().map(|s| s.value).collect::<Float64Array>()),
        Arc::new(samples.iter().map(|s| s.size).collect::<Float64Array>()),
        Arc::new(samples.iter().map(|s| s.stream).collect::<UInt16Array>()),
        Arc::new(samples.iter().map(|s| s.flags).collect::<UInt8Array>()),
    ];
    RecordBatch::try_new(schema, columns).expect("the columns match the schema")
}

/// Runs every format on the same records and reports what each did.
pub fn run_all(samples: &[Sample], dir: &std::path::Path) -> Vec<Measurement> {
    let mut out = Vec::new();

    // --- JSONL, the baseline everything arrives as -------------------------
    let (jsonl, write) = timed(|| to_jsonl(samples));
    let (count, read) = timed(|| jsonl.iter().filter(|byte| **byte == b'\n').count());
    assert_eq!(count, samples.len());
    out.push(Measurement {
        name: "jsonl".to_owned(),
        bytes: jsonl.len(),
        write_seconds: write,
        read_seconds: read,
        appendable: true,
        sliceable: false,
    });

    // --- JSONL + gzip, the usual next step ---------------------------------
    let (gz, write) = timed(|| {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        let _ignored = encoder.write_all(&jsonl);
        encoder.finish().unwrap_or_default()
    });
    let (_bytes, read) = timed(|| {
        use std::io::Write as _;
        let mut plain = Vec::new();
        let mut decoder = flate2::write::GzDecoder::new(&mut plain);
        let _ignored = decoder.write_all(&gz);
        let _ignored = decoder.finish();
        plain.len()
    });
    out.push(Measurement {
        name: "jsonl + gzip".to_owned(),
        bytes: gz.len(),
        write_seconds: write,
        read_seconds: read,
        // Concatenated gzip members are legal, so appending is possible — but
        // every reader then decompresses from the start to reach the end.
        appendable: true,
        sliceable: false,
    });

    // --- CSV + zstd --------------------------------------------------------
    let csv = to_csv(samples);
    let (csv_z, write) = timed(|| zstd::stream::encode_all(csv.as_slice(), 3).unwrap_or_default());
    let (_bytes, read) = timed(|| zstd::stream::decode_all(csv_z.as_slice()).unwrap_or_default().len());
    out.push(Measurement {
        name: "csv + zstd-3".to_owned(),
        bytes: csv_z.len(),
        write_seconds: write,
        read_seconds: read,
        appendable: false,
        sliceable: false,
    });

    // --- Parquet, zstd ------------------------------------------------------
    let batch = record_batch(samples);
    let (parquet_bytes, write) = timed(|| {
        let properties = parquet::file::properties::WriterProperties::builder()
            .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
            .build();
        let mut buffer = Vec::new();
        let mut writer =
            parquet::arrow::ArrowWriter::try_new(&mut buffer, batch.schema(), Some(properties)).expect("writer");
        writer.write(&batch).expect("write");
        writer.close().expect("close");
        buffer
    });
    let (rows, read) = timed(|| {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(parquet_bytes.clone()))
            .expect("reader")
            .build()
            .expect("build");
        reader
            .map(|batch| batch.map_or(0, |b: RecordBatch| b.num_rows()))
            .sum::<usize>()
    });
    assert_eq!(rows, samples.len());
    out.push(Measurement {
        name: "parquet (zstd)".to_owned(),
        bytes: parquet_bytes.len(),
        write_seconds: write,
        read_seconds: read,
        // A parquet file is finished when its footer is written. Appending
        // means rewriting the file, and a file without its footer is not a
        // parquet file at all.
        appendable: false,
        sliceable: true,
    });

    // --- Arrow IPC ----------------------------------------------------------
    let (arrow_bytes, write) = timed(|| {
        let mut buffer = Vec::new();
        {
            let mut writer =
                arrow::ipc::writer::FileWriter::try_new(&mut buffer, &arrow_schema()).expect("writer");
            writer.write(&batch).expect("write");
            writer.finish().expect("finish");
        }
        buffer
    });
    let (rows, read) = timed(|| {
        let reader = arrow::ipc::reader::FileReader::try_new(std::io::Cursor::new(arrow_bytes.clone()), None)
            .expect("reader");
        reader
            .map(|batch| batch.map_or(0, |b: RecordBatch| b.num_rows()))
            .sum::<usize>()
    });
    assert_eq!(rows, samples.len());
    out.push(Measurement {
        name: "arrow ipc".to_owned(),
        bytes: arrow_bytes.len(),
        write_seconds: write,
        read_seconds: read,
        appendable: false,
        sliceable: true,
    });

    // --- bizstd -------------------------------------------------------------
    let fixed = to_fixed_bytes(samples);
    let path = dir.join("bench.bizstd");
    let _ignored = std::fs::remove_file(&path);
    // One frame per 10% of the data: the shape a period-closing writer
    // produces, and what makes a slice cheap to reach.
    let per_frame = samples.len().div_ceil(10).max(1);
    let (bytes_written, write) = timed(|| {
        let schema = bizstd::Schema {
            name: "samples@1".to_owned(),
            fields: vec![
                FieldSpec { name: "time_nanos".to_owned(), ty: "u64".to_owned(), offset: 0 },
                FieldSpec { name: "value".to_owned(), ty: "f64".to_owned(), offset: 8 },
                FieldSpec { name: "size".to_owned(), ty: "f64".to_owned(), offset: 16 },
                FieldSpec { name: "stream".to_owned(), ty: "u16".to_owned(), offset: 24 },
                FieldSpec { name: "flags".to_owned(), ty: "u8".to_owned(), offset: 26 },
            ],
            layout: RecordLayout::Fixed(Sample::BYTES as u32),
        };
        let mut file = Container::create(&path, &schema, "benchmark", "bizstd-bench", 0, DEFAULT_HEADER_AREA, &[])
            .expect("create");
        for (index, sample) in samples.iter().enumerate() {
            file.append_record(&sample.to_bytes()).expect("append");
            if index.checked_rem(per_frame) == Some(per_frame.saturating_sub(1)) {
                file.close_frame((index / per_frame) as u64, HOT_LEVEL).expect("close");
            }
        }
        file.seal(u64::MAX, HOT_LEVEL).expect("seal");
        drop(file);
        std::fs::metadata(&path).map(|meta| meta.len() as usize).unwrap_or(0)
    });
    let (records, read) = timed(|| {
        let mut file = Container::open_read(&path).expect("open");
        let frames = file.frames().len();
        let mut total = 0usize;
        for index in 0..frames {
            let raw = file.read_frame_at(index).expect("frame");
            total += split_records(&raw, RecordLayout::Fixed(Sample::BYTES as u32)).0.len();
        }
        total
    });
    assert_eq!(records, samples.len(), "every record read back");
    assert_eq!(fixed.len(), samples.len() * Sample::BYTES);
    out.push(Measurement {
        name: "bizstd (zstd-3)".to_owned(),
        bytes: bytes_written,
        write_seconds: write,
        read_seconds: read,
        appendable: true,
        sliceable: true,
    });

    out
}
