//! `bizstd` — the command line for container files.
//!
//! Seven commands, and the split between them is about what they do to a file
//! rather than about how they are implemented:
//!
//! - `rebuild` rewrites it,
//! - `verify` and `fix` check and repair it,
//! - `inspect` describes it to a person,
//! - `try-json`, `try-csv` and `meta-json` turn it into something else.
//!
//! **The converting commands write nothing but their format to standard
//! output.** No progress, no summary, no banner. Anything worth saying goes to
//! standard error, so `bizstd try-json file | jq` works without a flag saying
//! "and please be quiet" — a tool that needs to be told not to corrupt its own
//! output is a tool nobody pipes twice. `inspect` is the opposite and says so
//! by writing for a person, not for a pipe.
//!
//! Exit codes carry the answer for the commands that have one: `0` sound, `1`
//! problems found, `2` the file could not be read at all. That is what makes
//! `verify` usable from a script without parsing its prose.
//!
//! Arguments are parsed by hand. There are seven commands and a handful of
//! options; a parser dependency would be larger than the program and would put
//! a version-resolution problem between a user and `cargo install`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use bizstd::{Container, FieldValue, HOT_LEVEL, NO_COMPRESSION, read_field, split_records};

/// What the process returns.
mod exit {
    /// The file is sound, or the work was done.
    pub const OK: i32 = 0;
    /// The file was read and something is wrong with it.
    pub const PROBLEMS: i32 = 1;
    /// The file could not be read, or the command line made no sense.
    pub const UNUSABLE: i32 = 2;
}

const USAGE: &str = "\
bizstd — work with container files

USAGE
  bizstd <command> <file> [options]

COMMANDS
  rebuild <file> [--level N] [--header-area N]
      Re-encode every frame at another level, keeping the boundaries. Default
      level 3. Run it with --level 19 for size, with --level 0 to store the
      bytes with no compression at all. --header-area widens the header zone,
      which is what a file that has run out of room for another frame needs.

  verify <file>
      Check headers, frames, checksums and record alignment. Exit 0 when
      sound, 1 when problems were found, 2 when the file cannot be read.

  fix <file>
      Derive the system headers from the data and write them back. Repairs a
      file whose counters or frame list disagree with its bytes; cannot repair
      the bytes themselves.

  inspect <file>
      Describe the file to a person: the preamble, the schema, the counters,
      every header, and a table of frames with what each one holds. Written to
      be read, not piped.

  try-json <file> [--limit N]
      Every record as JSON, one object per line, decoded by the file's own
      schema. Only JSON reaches standard output.

  try-csv <file> [--limit N] [--no-header]
      Every record as CSV, one row per record, with the schema's field names as
      the header row. Only CSV reaches standard output.

  meta-json <file>
      The headers and the frame index as one JSON object. Only JSON reaches
      standard output.

OPTIONS
  --level N        zstd level. 0 stores the bytes with no compression at all.
  --header-area N  size of the header zone, in bytes, when rewriting.
  --limit N        stop after N records.
  --no-header      omit the header row from try-csv.
  -h, --help       this text.
  -V, --version    the version.
";

fn main() {
    let code = run();
    std::process::exit(code);
}

/// The whole program, so that `main` only owns the exit code.
fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return exit::UNUSABLE;
    };

    match command {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return exit::OK;
        }
        "-V" | "--version" | "version" => {
            println!("bizstd {}", env!("CARGO_PKG_VERSION"));
            return exit::OK;
        }
        _other => {}
    }

    let Some(path) = args.get(1).map(PathBuf::from) else {
        fail(&format!("{command}: no file given"));
        return exit::UNUSABLE;
    };
    let options = args.get(2..).unwrap_or_default();

    match command {
        "rebuild" => match (
            option_i32(options, "--level"),
            option_u32(options, "--header-area"),
        ) {
            (Ok(level), Ok(area)) => rebuild(&path, level.unwrap_or(HOT_LEVEL), area),
            (Err(text), _) | (_, Err(text)) => {
                fail(&text);
                exit::UNUSABLE
            }
        },
        "verify" => verify(&path),
        "fix" => fix(&path),
        "inspect" => inspect(&path),
        "try-json" => match option_usize(options, "--limit") {
            Ok(limit) => try_json(&path, limit),
            Err(text) => {
                fail(&text);
                exit::UNUSABLE
            }
        },
        "try-csv" => match option_usize(options, "--limit") {
            Ok(limit) => try_csv(&path, limit, options.iter().any(|o| o == "--no-header")),
            Err(text) => {
                fail(&text);
                exit::UNUSABLE
            }
        },
        "meta-json" => meta_json(&path),
        other => {
            fail(&format!("unknown command {other:?}"));
            eprint!("{USAGE}");
            exit::UNUSABLE
        }
    }
}

/// Everything that is not the answer goes to standard error.
fn fail(text: &str) {
    let _ignored = writeln!(std::io::stderr(), "bizstd: {text}");
}

/// Says what happened, on standard error, for the commands that rewrite.
fn note(text: &str) {
    let _ignored = writeln!(std::io::stderr(), "{text}");
}

fn option_value<'a>(options: &'a [String], name: &str) -> Result<Option<&'a str>, String> {
    let mut iter = options.iter();
    while let Some(argument) = iter.next() {
        if argument == name {
            return iter
                .next()
                .map(|value| Some(value.as_str()))
                .ok_or_else(|| format!("{name} needs a value"));
        }
        if let Some(inline) = argument.strip_prefix(&format!("{name}=")) {
            return Ok(Some(inline));
        }
    }
    Ok(None)
}

fn option_i32(options: &[String], name: &str) -> Result<Option<i32>, String> {
    match option_value(options, name)? {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|_error| format!("{name} wants a number, got {text:?}")),
    }
}

fn option_u32(options: &[String], name: &str) -> Result<Option<u32>, String> {
    match option_value(options, name)? {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|_error| format!("{name} wants a number, got {text:?}")),
    }
}

fn option_usize(options: &[String], name: &str) -> Result<Option<usize>, String> {
    match option_value(options, name)? {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|_error| format!("{name} wants a number, got {text:?}")),
    }
}

/// Bytes as a number a person reads without counting digits.
#[expect(
    clippy::cast_precision_loss,
    reason = "the point of this function is an approximate number a person reads"
)]
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len().saturating_sub(1) {
        value /= 1024.0;
        unit = unit.saturating_add(1);
    }
    let name = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{bytes} {name}")
    } else {
        format!("{value:.1} {name}")
    }
}

fn rebuild(path: &Path, level: i32, header_area: Option<u32>) -> i32 {
    let outcome = match header_area {
        Some(area) => bizstd::repack_with_header_area(path, level, area),
        None => bizstd::repack(path, level),
    };
    match outcome {
        Ok(report) => {
            note(&format!(
                "re-encoded {} frame(s) at level {level}: {} -> {}",
                report.frames,
                human(report.bytes_before),
                human(report.bytes_after),
            ));
            if level == NO_COMPRESSION {
                note(
                    "stored without compression; rebuilding headers from the data is no longer possible",
                );
            }
            exit::OK
        }
        Err(error) => {
            fail(&error.to_string());
            exit::UNUSABLE
        }
    }
}

fn verify(path: &Path) -> i32 {
    match bizstd::validate(path, None) {
        Ok(report) => {
            if report.problems.is_empty() {
                note(&format!(
                    "sound: {} frame(s), {} record(s)",
                    report.frames, report.records
                ));
                return exit::OK;
            }
            for problem in &report.problems {
                note(&format!("problem: {problem}"));
            }
            note(&format!("{} problem(s) found", report.problems.len()));
            exit::PROBLEMS
        }
        Err(error) => {
            fail(&error.to_string());
            exit::UNUSABLE
        }
    }
}

fn fix(path: &Path) -> i32 {
    match bizstd::rebuild_headers(path, true) {
        Ok(report) => {
            if report.differences.is_empty() {
                note("nothing to fix: the headers already agree with the data");
                return exit::OK;
            }
            for difference in &report.differences {
                note(&format!("fixed: {difference}"));
            }
            // A repaired header zone does not mean repaired bytes, and saying
            // "fixed" without checking would be the more comforting lie.
            match bizstd::validate(path, None) {
                Ok(check) if check.problems.is_empty() => {
                    note("the file now validates clean");
                    exit::OK
                }
                Ok(check) => {
                    for problem in &check.problems {
                        note(&format!("still wrong: {problem}"));
                    }
                    note("the headers were repaired; the data was not");
                    exit::PROBLEMS
                }
                Err(error) => {
                    fail(&error.to_string());
                    exit::UNUSABLE
                }
            }
        }
        Err(error) => {
            fail(&error.to_string());
            exit::UNUSABLE
        }
    }
}

/// JSON string escaping, for the two commands that print JSON.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ignored = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out
}

/// One decoded field as a JSON value.
///
/// A `u64` beyond `2^53` cannot survive a JSON number: every reader that
/// parses into a double silently rounds it, which for a nanosecond timestamp
/// means losing the last few digits without a word. Those go out as strings.
fn json_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Unsigned(number) => {
            if *number > (1u64 << 53) {
                format!("\"{number}\"")
            } else {
                number.to_string()
            }
        }
        FieldValue::Signed(number) => {
            if number.unsigned_abs() > (1u64 << 53) {
                format!("\"{number}\"")
            } else {
                number.to_string()
            }
        }
        FieldValue::Float(number) => {
            if number.is_finite() {
                format!("{number}")
            } else {
                // JSON has no infinity and no NaN. Null is the honest answer;
                // printing `NaN` produces a document nothing can parse.
                "null".to_owned()
            }
        }
        // `FieldValue` is non-exhaustive: a variant added upstream must not
        // stop this compiling, and rendering an unfamiliar one as its own
        // Display output is better than refusing to print the record.
        _other => format!("\"{value}\""),
    }
}

fn try_json(path: &Path, limit: Option<usize>) -> i32 {
    let mut container = match Container::open_read(path) {
        Ok(container) => container,
        Err(error) => {
            fail(&error.to_string());
            return exit::UNUSABLE;
        }
    };
    let schema = container.schema().clone();
    let layout = schema.layout;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut written = 0usize;

    let emit = |record: &[u8], out: &mut dyn std::io::Write| -> std::io::Result<()> {
        let mut line = String::from("{");
        for (index, field) in schema.fields.iter().enumerate() {
            if index > 0 {
                line.push(',');
            }
            line.push('"');
            line.push_str(&escape(&field.name));
            line.push_str("\":");
            match read_field(record, field) {
                Some(value) => line.push_str(&json_value(&value)),
                None => line.push_str("null"),
            }
        }
        line.push('}');
        writeln!(out, "{line}")
    };

    let frames = container.frames().len();
    for index in 0..frames {
        let raw = match container.read_frame_at(index) {
            Ok(raw) => raw,
            Err(error) => {
                fail(&error.to_string());
                return exit::UNUSABLE;
            }
        };
        let (records, _leftover) = split_records(&raw, layout);
        for record in records {
            if limit.is_some_and(|stop| written >= stop) {
                let _ignored = out.flush();
                return exit::OK;
            }
            if emit(record, &mut out).is_err() {
                // A closed pipe is what `| head` looks like from here, and it
                // is not an error worth a message or a non-zero exit.
                return exit::OK;
            }
            written = written.saturating_add(1);
        }
    }

    match container.read_tail() {
        Ok(tail) => {
            let (records, _leftover) = split_records(&tail, layout);
            for record in records {
                if limit.is_some_and(|stop| written >= stop) {
                    break;
                }
                if emit(record, &mut out).is_err() {
                    return exit::OK;
                }
                written = written.saturating_add(1);
            }
        }
        Err(error) => {
            fail(&error.to_string());
            return exit::UNUSABLE;
        }
    }

    let _ignored = out.flush();
    exit::OK
}

/// Walks the records, calling `emit` for each, until the limit is reached.
///
/// Both converting commands need the same walk and differ only in what they
/// write, so the walk is written once. Frames go by position, and the tail
/// comes last because that is the order the file holds them in.
fn each_record(
    container: &mut Container,
    limit: Option<usize>,
    mut emit: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> i32 {
    let layout = container.schema().layout;
    let mut written = 0usize;
    let frames = container.frames().len();

    for index in 0..frames {
        let raw = match container.read_frame_at(index) {
            Ok(raw) => raw,
            Err(error) => {
                fail(&error.to_string());
                return exit::UNUSABLE;
            }
        };
        let (records, _leftover) = split_records(&raw, layout);
        for record in records {
            if limit.is_some_and(|stop| written >= stop) {
                return exit::OK;
            }
            if emit(record).is_err() {
                // A closed pipe is what `| head` looks like from here, and it
                // is not an error worth a message or a non-zero exit.
                return exit::OK;
            }
            written = written.saturating_add(1);
        }
    }

    match container.read_tail() {
        Ok(tail) => {
            let (records, _leftover) = split_records(&tail, layout);
            for record in records {
                if limit.is_some_and(|stop| written >= stop) {
                    break;
                }
                if emit(record).is_err() {
                    return exit::OK;
                }
                written = written.saturating_add(1);
            }
            exit::OK
        }
        Err(error) => {
            fail(&error.to_string());
            exit::UNUSABLE
        }
    }
}

/// One field as a CSV cell, quoted only when it has to be.
///
/// A value that carries a comma, a quote or a newline and is written plainly
/// turns one row into two, and the reader on the other end has no way to know.
fn csv_cell(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

fn try_csv(path: &Path, limit: Option<usize>, no_header: bool) -> i32 {
    let mut container = match Container::open_read(path) {
        Ok(container) => container,
        Err(error) => {
            fail(&error.to_string());
            return exit::UNUSABLE;
        }
    };
    let schema = container.schema().clone();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    if !no_header {
        let header: Vec<String> = schema
            .fields
            .iter()
            .map(|field| csv_cell(&field.name))
            .collect();
        if writeln!(out, "{}", header.join(",")).is_err() {
            return exit::OK;
        }
    }

    let code = each_record(&mut container, limit, |record| {
        let cells: Vec<String> = schema
            .fields
            .iter()
            .map(|field| match read_field(record, field) {
                // An empty cell rather than a zero: a record too short for the
                // field its schema declares is missing it, and CSV has a way
                // of saying that.
                None => String::new(),
                Some(value) => csv_cell(&value.to_string()),
            })
            .collect();
        writeln!(out, "{}", cells.join(","))
    });
    let _ignored = out.flush();
    code
}

/// A ratio as a person reads it, or a dash when there is nothing to compare.
#[expect(
    clippy::cast_precision_loss,
    reason = "a ratio is read by a person, not multiplied by anything"
)]
fn ratio(raw: u64, stored: u64) -> String {
    if stored == 0 || raw == 0 {
        return "—".to_owned();
    }
    format!("{:.2}x", raw as f64 / stored as f64)
}

/// The schema, one field per line, in declaration order.
fn write_schema(out: &mut String, schema: &bizstd::Schema) {
    let layout = match schema.layout {
        bizstd::RecordLayout::Fixed(size) => format!("fixed, {size} B per record"),
        bizstd::RecordLayout::Prefixed => "length-prefixed".to_owned(),
    };
    let _ignored = writeln!(out, "\nschema  {}  ({layout})", schema.name);
    for field in &schema.fields {
        let _ignored = writeln!(
            out,
            "  {:>4}  {:<10} {}",
            field.offset, field.ty, field.name
        );
    }
}

/// The frame index as a table, checksums included: that is the column worth
/// having in front of you when a file is suspected of being damaged.
fn write_frames(out: &mut String, frames: &[bizstd::Frame]) {
    if frames.is_empty() {
        let _ignored = writeln!(out, "\nno closed frames yet");
        return;
    }
    let _ignored = writeln!(out, "\nframes");
    let _ignored = writeln!(out, "     #     id       offset         len   checksum");
    for (index, frame) in frames.iter().enumerate() {
        let _ignored = writeln!(
            out,
            "  {index:>4}  {:>5}  {:>11}  {:>10}   {:016x}",
            frame.id, frame.offset, frame.len, frame.hash
        );
    }
}

/// The headers, the container's apart from the caller's.
///
/// Showing them together would blur the one distinction that matters here:
/// what the format put in the file, and what you did.
fn write_headers(out: &mut String, headers: &bizstd::Headers) {
    let (system, application): (Vec<_>, Vec<_>) = headers
        .pairs()
        .iter()
        .partition(|(key, _value)| key.starts_with('_'));

    let _ignored = writeln!(out, "\nsystem headers");
    for (key, value) in system {
        // The frame index and the preview are long by design; a terminal full
        // of one header hides the thirteen others.
        let shown = match value.char_indices().nth(67) {
            Some((cut, _character)) => format!("{}…", value.get(..cut).unwrap_or_default()),
            None => value.clone(),
        };
        let _ignored = writeln!(out, "  {key:<20} {shown}");
    }

    if application.is_empty() {
        let _ignored = writeln!(out, "\nno application headers");
        return;
    }
    let _ignored = writeln!(out, "\napplication headers");
    for (key, value) in application {
        let _ignored = writeln!(out, "  {key:<20} {value}");
    }
}

fn inspect(path: &Path) -> i32 {
    let mut container = match Container::open_read(path) {
        Ok(container) => container,
        Err(error) => {
            fail(&error.to_string());
            return exit::UNUSABLE;
        }
    };
    let (preamble, headers) = match bizstd::peek_headers(path) {
        Ok(pair) => pair,
        Err(error) => {
            fail(&error.to_string());
            return exit::UNUSABLE;
        }
    };
    let file_size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let schema = container.schema().clone();
    let frames = container.frames().to_vec();

    let mut out = String::new();
    let _ignored = writeln!(out, "{}", path.display());
    let _ignored = writeln!(out, "  size          {}", human(file_size));
    let _ignored = writeln!(
        out,
        "  format        version {}, header zone {} B, flags {:#04b}",
        preamble.version, preamble.header_area, preamble.flags
    );
    let _ignored = writeln!(
        out,
        "  compression   {} (level {})",
        headers.get("_compression").unwrap_or("?"),
        headers.get("_compression_level").unwrap_or("?"),
    );
    let _ignored = writeln!(
        out,
        "  sealed        {}",
        headers.get("_sealed").unwrap_or("?")
    );

    write_schema(&mut out, &schema);

    // --- what is in it ------------------------------------------------------
    let records: u64 = headers.get("_records").unwrap_or("0").parse().unwrap_or(0);
    let bytes_raw: u64 = headers
        .get("_bytes_raw")
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let stored: u64 = frames.iter().map(|frame| frame.len).sum();
    let tail = container
        .read_tail()
        .map(|tail| tail.len() as u64)
        .unwrap_or(0);
    let tail_records = match container.read_tail() {
        Ok(bytes) => split_records(&bytes, schema.layout).0.len() as u64,
        Err(_error) => 0,
    };

    let _ignored = writeln!(out, "\ncontents");
    let _ignored = writeln!(out, "  frames        {}", frames.len());
    let _ignored = writeln!(
        out,
        "  records       {} closed{}",
        records,
        if tail_records > 0 {
            format!(", {tail_records} in the open tail")
        } else {
            String::new()
        }
    );
    let _ignored = writeln!(
        out,
        "  raw bytes     {} in frames, {} in the tail",
        human(bytes_raw),
        human(tail)
    );
    let _ignored = writeln!(
        out,
        "  stored        {} ({} of the raw)",
        human(stored),
        ratio(bytes_raw, stored)
    );

    write_frames(&mut out, &frames);
    write_headers(&mut out, &headers);

    // --- a look at the data -------------------------------------------------
    let _ignored = writeln!(out, "\nfirst records");
    let mut shown = 0usize;
    let code = each_record(&mut container, Some(3), |record| {
        let cells: Vec<String> = schema
            .fields
            .iter()
            .map(|field| match read_field(record, field) {
                Some(value) => format!("{}={value}", field.name),
                None => format!("{}=<short>", field.name),
            })
            .collect();
        shown = shown.saturating_add(1);
        writeln!(out, "  {}", cells.join("  ")).map_err(std::io::Error::other)
    });
    if shown == 0 {
        let _ignored = writeln!(out, "  (none)");
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ignored = handle.write_all(out.as_bytes());
    code
}

fn meta_json(path: &Path) -> i32 {
    let (preamble, headers) = match bizstd::peek_headers(path) {
        Ok(pair) => pair,
        Err(error) => {
            fail(&error.to_string());
            return exit::UNUSABLE;
        }
    };
    let frames =
        bizstd::parse_frames(headers.get("_frames").unwrap_or_default()).unwrap_or_default();

    let mut out = String::from("{\"preamble\":{");
    let _ignored = write!(out, "\"version\":{},", preamble.version);
    let _ignored = write!(out, "\"flags\":{},", preamble.flags);
    let _ignored = write!(out, "\"headerArea\":{}", preamble.header_area);
    out.push_str("},\"headers\":{");
    for (index, (key, value)) in headers.pairs().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ignored = write!(out, "\"{}\":\"{}\"", escape(key), escape(value));
    }
    out.push_str("},\"frames\":[");
    for (index, frame) in frames.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        // Offsets and lengths are strings for the same reason record fields
        // are: a file larger than 9 petabytes is not the point, a reader that
        // rounds is.
        let _ignored = write!(
            out,
            "{{\"id\":\"{}\",\"offset\":\"{}\",\"len\":\"{}\",\"hash\":\"{:016x}\"}}",
            frame.id, frame.offset, frame.len, frame.hash
        );
    }
    out.push_str("]}");

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    if writeln!(handle, "{out}").is_err() {
        return exit::OK;
    }
    exit::OK
}
