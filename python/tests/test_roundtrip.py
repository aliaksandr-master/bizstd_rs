"""What the package promises, checked against a real file on disk."""

from __future__ import annotations

import struct
from pathlib import Path

import pytest

import bizstd


def sample_schema() -> bizstd.Schema:
    return bizstd.Schema(
        "samples@1",
        [
            bizstd.FieldSpec("time_nanos", "u64", 0),
            bizstd.FieldSpec("value", "f64", 8),
        ],
        bizstd.RecordLayout.fixed(16),
    )


def record(time_nanos: int, value: float) -> bytes:
    return struct.pack("<Qd", time_nanos, value)


def test_a_full_life_roundtrip(tmp_path: Path) -> None:
    path = tmp_path / "day.bizstd"
    with bizstd.create(path, sample_schema(), source="test", writer="pytest") as file:
        for index in range(100):
            file.append(record(index, float(index)))
        file.close_frame(0)
        for index in range(100, 150):
            file.append(record(index, float(index)))
        file.seal(1)

    with bizstd.open_read(path) as reader:
        assert reader.headers["_sealed"] == "true"
        assert reader.headers["_records"] == "150"
        assert len(reader.frames) == 2
        assert reader.record_count == 150
        assert [frame.hash != 0 for frame in reader.frames] == [True, True]

        records = list(reader.records())
        assert len(records) == 150
        first_time, first_value = struct.unpack("<Qd", records[0])
        assert (first_time, first_value) == (0, 0.0)


def test_iteration_covers_the_unclosed_tail(tmp_path: Path) -> None:
    path = tmp_path / "tail.bizstd"
    with bizstd.create(path, sample_schema(), source="test", writer="pytest") as file:
        for index in range(10):
            file.append(record(index, 1.0))
        file.close_frame(0)
        for index in range(10, 25):
            file.append(record(index, 2.0))
        # No second close: the tail stays raw, which is the normal state of a
        # file being written to.

    with bizstd.open_read(path) as reader:
        assert len(list(reader)) == 25
        assert len(list(reader.records(include_tail=False))) == 10


def test_application_headers_survive(tmp_path: Path) -> None:
    path = tmp_path / "headers.bizstd"
    with bizstd.create(
        path,
        sample_schema(),
        source="test",
        writer="pytest",
        headers={"stream": "alpha", "region": "north"},
    ) as file:
        file.append(record(1, 1.0))
        file.seal(0)

    with bizstd.open_read(path) as reader:
        assert reader.headers["stream"] == "alpha"
        assert reader.headers["region"] == "north"


def test_a_record_of_the_wrong_size_is_a_usage_error(tmp_path: Path) -> None:
    path = tmp_path / "wrong.bizstd"
    with (
        bizstd.create(path, sample_schema(), source="test", writer="pytest") as file,
        pytest.raises(bizstd.BizstdUsageError),
    ):
        file.append(b"too short")


def test_a_missing_file_is_an_oserror(tmp_path: Path) -> None:
    with pytest.raises(OSError):
        bizstd.open_read(tmp_path / "absent.bizstd")


def test_a_truncated_file_is_malformed(tmp_path: Path) -> None:
    path = tmp_path / "torn.bizstd"
    with bizstd.create(path, sample_schema(), source="test", writer="pytest") as file:
        file.append(record(1, 1.0))
        file.seal(0)
    path.write_bytes(path.read_bytes()[:12])
    with pytest.raises((bizstd.BizstdMalformedError, OSError)):
        bizstd.open_read(path)


def test_maintenance_agrees_with_the_data(tmp_path: Path) -> None:
    path = tmp_path / "maintenance.bizstd"
    with bizstd.create(path, sample_schema(), source="test", writer="pytest") as file:
        for index in range(200):
            file.append(record(index, float(index)))
        file.seal(0)

    report = bizstd.validate(path)
    assert report.problems == []
    assert report.frames == 1
    assert report.records == 200

    rebuild = bizstd.rebuild_headers(path, fix=False)
    assert rebuild.differences == []

    before = path.stat().st_size
    repacked = bizstd.repack(path, bizstd.COLD_LEVEL)
    assert repacked.frames == 1
    assert repacked.bytes_before == before
    assert bizstd.validate(path).problems == []


def test_prefixed_records_come_back_without_their_prefix(tmp_path: Path) -> None:
    path = tmp_path / "prefixed.bizstd"
    schema = bizstd.Schema(
        "book@1",
        [bizstd.FieldSpec("n_levels", "u16", 0)],
        bizstd.RecordLayout.prefixed(),
    )
    with bizstd.create(path, schema, source="test", writer="pytest") as file:
        file.append(b"\x01\x02\x03")
        file.append(b"\x04\x05\x06\x07\x08")
        file.seal(0)

    with bizstd.open_read(path) as reader:
        assert list(reader) == [b"\x01\x02\x03", b"\x04\x05\x06\x07\x08"]


def test_peek_reads_headers_without_the_data(tmp_path: Path) -> None:
    path = tmp_path / "peek.bizstd"
    with bizstd.create(path, sample_schema(), source="test", writer="pytest") as file:
        file.append(record(1, 1.0))
        file.seal(0)

    preamble, headers = bizstd.peek_headers(path)
    assert preamble.version == bizstd.VERSION
    assert preamble.header_area == bizstd.DEFAULT_HEADER_AREA
    assert headers["_schema"] == "samples@1"


def test_the_header_zone_fills_and_says_so(tmp_path: Path) -> None:
    path = tmp_path / "full.bizstd"
    closed = 0
    with (
        bizstd.create(path, sample_schema(), source="test", writer="pytest") as file,
        pytest.raises(bizstd.BizstdZoneFullError),
    ):
        for closed in range(10_000):
            file.append(record(closed, 1.0))
            file.close_frame(closed)
    assert closed >= bizstd.max_frames_for(bizstd.DEFAULT_HEADER_AREA)

    # And repacking with a larger zone is the way out, not a retry.
    bizstd.repack(path, bizstd.HOT_LEVEL, header_area=64 * 1024)
    with bizstd.open_append(path) as file:
        file.append(record(1, 1.0))
        file.close_frame(999)


def test_xxh64_matches_the_specification_vectors() -> None:
    assert bizstd.xxh64(b"") == 0xEF46DB3751D8E999
    assert bizstd.xxh64(b"abc") == 0x44BC2CF5AD770999
