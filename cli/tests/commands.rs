//! The commands, run as a person runs them: the built binary, a real file, and
//! the exit code and the streams it produced.
//!
//! Calling the library directly would test the library again. What is worth
//! testing here is the part only this crate owns — that `verify` exits 1 on a
//! broken file rather than 0, and that the JSON commands put nothing on
//! standard output but JSON.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "a failing test reports itself by panicking, and the fixture's arithmetic is bounded by the loop it is in"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bizstd::{Container, DEFAULT_HEADER_AREA, FieldSpec, HOT_LEVEL, RecordLayout, Schema};

/// The binary cargo just built, whatever the profile.
fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary knows where it is");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("bizstd")
}

fn run(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("the binary runs")
}

fn scratch(tag: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    root.push(format!("bizstd-cli-{tag}-{}", std::process::id()));
    let _ignored = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    root
}

fn sample(path: &Path, frames: u64, per_frame: u64) {
    let schema = Schema {
        name: "samples@1".to_owned(),
        fields: vec![
            FieldSpec {
                name: "time_nanos".to_owned(),
                ty: "u64".to_owned(),
                offset: 0,
            },
            FieldSpec {
                name: "value".to_owned(),
                ty: "f64".to_owned(),
                offset: 8,
            },
        ],
        layout: RecordLayout::Fixed(16),
    };
    let mut file = Container::create(
        path,
        &schema,
        "test",
        "cli-test",
        0,
        DEFAULT_HEADER_AREA,
        &[("stream", "alpha")],
    )
    .expect("create");
    for frame in 0..frames {
        for index in 0..per_frame {
            let mut record = [0u8; 16];
            record[..8].copy_from_slice(&(frame * per_frame + index).to_le_bytes());
            record[8..].copy_from_slice(&(100.5_f64 + index as f64).to_le_bytes());
            file.append_record(&record).expect("append");
        }
        file.close_frame(frame, HOT_LEVEL).expect("close");
    }
    file.seal(frames, HOT_LEVEL).expect("seal");
}

#[test]
fn verify_says_sound_and_exits_zero() {
    let root = scratch("verify");
    let path = root.join("sound.bizstd");
    sample(&path, 3, 50);

    let output = run(&["verify", path.to_str().expect("utf-8 path")]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "verify says nothing on stdout");
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("sound"), "{text}");
    assert!(text.contains("150 record(s)"), "{text}");
}

#[test]
fn verify_exits_one_on_a_file_whose_headers_lie() {
    let root = scratch("verify-broken");
    let path = root.join("lying.bizstd");
    sample(&path, 2, 30);

    // Rewrite the record counter to something the data does not support. Same
    // length, so the header zone keeps its shape.
    let mut bytes = std::fs::read(&path).expect("read");
    let zone_start = 16;
    let position = bytes
        .windows(b"_records:60\n".len())
        .position(|window| window == b"_records:60\n")
        .expect("the counter is in the zone");
    assert!(position > zone_start);
    bytes[position..position + b"_records:60\n".len()].copy_from_slice(b"_records:11\n");
    std::fs::write(&path, &bytes).expect("write");

    let output = run(&["verify", path.to_str().expect("utf-8 path")]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a broken file must not exit zero"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("problem"));
}

#[test]
fn verify_exits_two_on_something_that_is_not_a_container() {
    let root = scratch("verify-garbage");
    let path = root.join("not.bizstd");
    std::fs::write(&path, b"this is not a container").expect("write");

    let output = run(&["verify", path.to_str().expect("utf-8 path")]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "unreadable is not the same as unsound"
    );
}

#[test]
fn fix_repairs_the_headers_and_verify_then_passes() {
    let root = scratch("fix");
    let path = root.join("fix.bizstd");
    sample(&path, 2, 40);

    let mut bytes = std::fs::read(&path).expect("read");
    let position = bytes
        .windows(b"_records:80\n".len())
        .position(|window| window == b"_records:80\n")
        .expect("the counter is in the zone");
    bytes[position..position + b"_records:80\n".len()].copy_from_slice(b"_records:11\n");
    std::fs::write(&path, &bytes).expect("write");

    assert_eq!(
        run(&["verify", path.to_str().expect("utf-8")])
            .status
            .code(),
        Some(1)
    );

    let output = run(&["fix", path.to_str().expect("utf-8")]);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixed"));

    assert_eq!(
        run(&["verify", path.to_str().expect("utf-8")])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn try_json_puts_nothing_but_json_on_stdout() {
    let root = scratch("try-json");
    let path = root.join("json.bizstd");
    sample(&path, 2, 25);

    let output = run(&["try-json", path.to_str().expect("utf-8")]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).expect("utf-8 output");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 50, "one line per record");
    for line in &lines {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "not an object: {line}"
        );
        assert!(
            line.contains("\"time_nanos\""),
            "the schema's field names are used: {line}"
        );
    }
    // A u64 beyond 2^53 goes out as a string, because a JSON number would be
    // rounded by every reader that parses into a double.
    assert!(
        lines.first().expect("a line").contains("\"time_nanos\":0"),
        "small ones stay numbers"
    );
}

#[test]
fn try_json_stops_at_the_limit() {
    let root = scratch("limit");
    let path = root.join("limit.bizstd");
    sample(&path, 3, 100);

    let output = run(&["try-json", path.to_str().expect("utf-8"), "--limit", "7"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 7);
}

#[test]
fn meta_json_is_one_object_with_the_headers_and_the_index() {
    let root = scratch("meta");
    let path = root.join("meta.bizstd");
    sample(&path, 4, 20);

    let output = run(&["meta-json", path.to_str().expect("utf-8")]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).expect("utf-8");
    assert_eq!(text.lines().count(), 1, "one object, one line");
    assert!(text.contains("\"headerArea\":4096"));
    assert!(text.contains("\"_schema\":\"samples@1\""));
    assert!(
        text.contains("\"stream\":\"alpha\""),
        "application headers are included"
    );
    // Four frames, each with an id, an offset, a length and a checksum.
    assert_eq!(text.matches("\"hash\"").count(), 4);
}

#[test]
fn rebuild_moves_between_levels_including_none() {
    let root = scratch("rebuild");
    let path = root.join("levels.bizstd");
    sample(&path, 3, 200);
    let compressed = std::fs::metadata(&path).expect("stat").len();

    let output = run(&["rebuild", path.to_str().expect("utf-8"), "--level", "0"]);
    assert_eq!(output.status.code(), Some(0));
    let plain = std::fs::metadata(&path).expect("stat").len();
    assert!(plain > compressed, "no compression means a larger file");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("without compression"),
        "the consequence is stated, not left to be discovered"
    );
    assert_eq!(
        run(&["verify", path.to_str().expect("utf-8")])
            .status
            .code(),
        Some(0)
    );

    assert_eq!(
        run(&["rebuild", path.to_str().expect("utf-8"), "--level", "19"])
            .status
            .code(),
        Some(0)
    );
    assert!(std::fs::metadata(&path).expect("stat").len() < plain);
    assert_eq!(
        run(&["verify", path.to_str().expect("utf-8")])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn try_csv_writes_a_header_row_and_one_row_per_record() {
    let root = scratch("csv");
    let path = root.join("csv.bizstd");
    sample(&path, 2, 15);

    let output = run(&["try-csv", path.to_str().expect("utf-8")]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).expect("utf-8");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 31, "a header row and thirty records");
    assert_eq!(lines.first().copied(), Some("time_nanos,value"));
    assert_eq!(lines.get(1).copied(), Some("0,100.5"));
    assert!(output.stderr.is_empty(), "nothing but CSV comes out");

    let bare = run(&["try-csv", path.to_str().expect("utf-8"), "--no-header"]);
    assert_eq!(String::from_utf8_lossy(&bare.stdout).lines().count(), 30);

    let limited = run(&["try-csv", path.to_str().expect("utf-8"), "--limit", "4"]);
    assert_eq!(
        String::from_utf8_lossy(&limited.stdout).lines().count(),
        5,
        "the limit counts records, not lines"
    );
}

#[test]
fn inspect_describes_the_file() {
    let root = scratch("inspect");
    let path = root.join("inspect.bizstd");
    sample(&path, 3, 40);

    let output = run(&["inspect", path.to_str().expect("utf-8")]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).expect("utf-8");

    for expected in [
        "version 1",
        "schema  samples@1",
        "fixed, 16 B per record",
        "time_nanos",
        "frames        3",
        "records       120",
        "system headers",
        "application headers",
        "stream",
        "first records",
    ] {
        assert!(
            text.contains(expected),
            "inspect never mentioned {expected:?}:\n{text}"
        );
    }
    // The frame table carries a checksum per frame, which is the thing worth
    // seeing when a file is suspected of being damaged.
    assert_eq!(
        text.matches("  0            0").count(),
        1,
        "the first frame is listed:\n{text}"
    );
}

#[test]
fn a_command_line_that_makes_no_sense_exits_two_and_says_why() {
    assert_eq!(run(&["frobnicate", "x"]).status.code(), Some(2));
    assert_eq!(run(&[]).status.code(), Some(2));
    assert_eq!(run(&["verify"]).status.code(), Some(2));

    let bad_level = run(&["rebuild", "x.bizstd", "--level", "loud"]);
    assert_eq!(bad_level.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad_level.stderr).contains("wants a number"));

    let help = run(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("USAGE"));
}
