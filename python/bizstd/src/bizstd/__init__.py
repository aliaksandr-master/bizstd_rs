"""bizstd — an append-only container for records that arrive continuously.

A file is a fixed binary preamble, a text header zone edited in place, and a
data section of independent zstd frames followed by an uncompressed tail.
Records are appended to the tail as they arrive; when a period ends the tail is
compressed into a frame. Both steps are crash-safe and every frame carries a
checksum.

The compiled work happens in :mod:`bizstd_binary`. What lives here is the part
worth reading: iteration, context managers, and names that say what they do.

    >>> import bizstd
    >>> schema = bizstd.Schema(
    ...     "samples@1",
    ...     [bizstd.FieldSpec("time_nanos", "u64", 0), bizstd.FieldSpec("value", "f64", 8)],
    ...     bizstd.RecordLayout.fixed(16),
    ... )
    >>> with bizstd.create("samples.bizstd", schema, source="sensor", writer="demo") as file:
    ...     file.append(record_bytes)
    ...     file.close_frame(0)

One writer per file, and enforcing that is the caller's job: nothing here takes
a lock, and two writers on one path corrupt it silently.
"""

from __future__ import annotations

import os
from collections.abc import Iterator, Mapping, Sequence
from contextlib import contextmanager
from typing import Final

from bizstd_binary import (
    COLD_LEVEL,
    DEFAULT_HEADER_AREA,
    EXTENSION,
    HOT_LEVEL,
    MAX_HEADER_AREA,
    VERSION,
    BizstdCompressionError,
    BizstdError,
    BizstdMalformedError,
    BizstdUsageError,
    BizstdZoneFullError,
    Container,
    FieldSpec,
    Frame,
    Preamble,
    RebuildReport,
    RecordLayout,
    RepackReport,
    Schema,
    ValidateReport,
    max_frames_for,
    peek_headers,
    rebuild_headers,
    repack,
    split_records,
    validate,
    xxh64,
)

__all__ = [
    "COLD_LEVEL",
    "DEFAULT_HEADER_AREA",
    "EXTENSION",
    "HOT_LEVEL",
    "MAX_HEADER_AREA",
    "VERSION",
    "BizstdCompressionError",
    "BizstdError",
    "BizstdMalformedError",
    "BizstdUsageError",
    "BizstdZoneFullError",
    "Container",
    "FieldSpec",
    "Frame",
    "Preamble",
    "Reader",
    "RebuildReport",
    "RecordLayout",
    "RepackReport",
    "Schema",
    "ValidateReport",
    "Writer",
    "__version__",
    "create",
    "max_frames_for",
    "open_append",
    "open_read",
    "peek_headers",
    "rebuild_headers",
    "repack",
    "split_records",
    "validate",
    "xxh64",
]

__version__: Final = "2.1.0"

StrPath = str | os.PathLike[str]


class Reader:
    """A container opened for reading.

    Wraps the compiled handle to add the thing a reader actually wants: the
    records, in order, without having to remember that frames are addressed by
    position and that the tail is separate.
    """

    __slots__ = ("_inner",)

    def __init__(self, inner: Container) -> None:
        """Wrap a compiled handle. Use :func:`open_read` rather than this."""
        self._inner = inner

    @property
    def headers(self) -> Mapping[str, str]:
        """Every header, system and application alike."""
        return self._inner.headers()

    @property
    def schema(self) -> Schema:
        """The schema the file declares."""
        return self._inner.schema()

    @property
    def frames(self) -> Sequence[Frame]:
        """The closed frames, in file order."""
        return self._inner.frames()

    @property
    def record_count(self) -> int:
        """Records the headers claim, closed frames only."""
        return self._inner.records()

    def frame(self, index: int) -> bytes:
        """The decompressed bytes of the frame at this position.

        By position rather than by id: ids are the writer's and writers repeat
        them — partition by hour and a midnight spill closes under hour 0 after
        hour 23.
        """
        return self._inner.read_frame_at(index)

    def frame_by_id(self, frame_id: int) -> bytes:
        """The decompressed bytes of the first frame carrying this id."""
        return self._inner.read_frame(frame_id)

    def tail(self) -> bytes:
        """The uncompressed tail, whole records only."""
        return self._inner.read_tail()

    def records(self, *, include_tail: bool = True) -> Iterator[bytes]:
        """Every record in the file, frame by frame and then the tail.

        Yields one record at a time, but decompresses one whole frame at a
        time: that is how the format is laid out, and pretending otherwise
        would mean decompressing a frame per record.
        """
        layout = self._inner.schema().layout
        for index in range(len(self._inner.frames())):
            records, leftover = split_records(self._inner.read_frame_at(index), layout)
            if leftover:
                raise BizstdMalformedError(f"frame at position {index}: {leftover} torn byte(s)")
            yield from records
        if include_tail:
            records, _leftover = split_records(self._inner.read_tail(), layout)
            yield from records

    def close(self) -> None:
        """Release the handle. Reading holds no buffers, so this is cheap."""

    def __enter__(self) -> Reader:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def __iter__(self) -> Iterator[bytes]:
        return self.records()

    def __repr__(self) -> str:
        return f"Reader(frames={len(self._inner.frames())}, records={self._inner.records()})"


class Writer:
    """A container opened for appending.

    Buffers records and writes them in batches, which is what makes appending
    cheap. Leaving the ``with`` block flushes; losing the object without either
    flushing or sealing is the one way to lose records, and it is the reason
    this is a context manager.
    """

    __slots__ = ("_inner",)

    def __init__(self, inner: Container) -> None:
        """Wrap a compiled handle. Use :func:`create` or :func:`open_append`."""
        self._inner = inner

    @property
    def headers(self) -> Mapping[str, str]:
        """Every header, system and application alike."""
        return self._inner.headers()

    @property
    def frames(self) -> Sequence[Frame]:
        """The frames closed so far."""
        return self._inner.frames()

    @property
    def record_count(self) -> int:
        """Records appended, the unflushed buffer included."""
        return self._inner.records()

    def append(self, body: bytes) -> None:
        """Appends one record.

        A fixed layout takes exactly the schema's size; a prefixed layout takes
        the body and the container writes the length itself.
        """
        self._inner.append_record(body)

    def extend(self, bodies: Sequence[bytes]) -> None:
        """Appends many records."""
        for body in bodies:
            self._inner.append_record(body)

    def flush(self) -> None:
        """Writes whatever is buffered, without closing a frame."""
        self._inner.flush_data()

    def close_frame(self, frame_id: int, level: int = HOT_LEVEL) -> None:
        """Compresses the tail into one frame, crash-safe.

        Raises :class:`BizstdZoneFullError` when the header zone can no longer hold
        another entry — the way out is :func:`repack` with a larger zone, not a
        retry.
        """
        self._inner.close_frame(frame_id, level)

    def seal(self, frame_id: int, level: int = HOT_LEVEL) -> None:
        """Closes the tail and marks the file finished."""
        self._inner.seal(frame_id, level)

    def set_header(self, key: str, value: str) -> None:
        """Sets an application header. Keys may not start with ``_``."""
        self._inner.set_user_header(key, value)

    def __enter__(self) -> Writer:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.flush()

    def __repr__(self) -> str:
        return f"Writer(records={self._inner.records()}, frames={len(self._inner.frames())})"


def create(
    path: StrPath,
    schema: Schema,
    *,
    source: str,
    writer: str,
    created_at_millis: int = 0,
    header_area: int = DEFAULT_HEADER_AREA,
    headers: Mapping[str, str] | None = None,
) -> Writer:
    """Creates a file and returns it open for appending.

    ``header_area`` decides how many frames the file can ever hold — see
    :func:`max_frames_for`. It cannot be changed later without
    :func:`repack`, so a long-lived file is worth sizing up front.
    """
    inner = Container.create(
        os.fspath(path),
        schema,
        source,
        writer,
        created_at_millis,
        header_area,
        list((headers or {}).items()),
    )
    return Writer(inner)


def open_read(path: StrPath) -> Reader:
    """Opens a file read-only. Nothing is recovered and nothing is written."""
    return Reader(Container.open_read(os.fspath(path)))


def open_append(path: StrPath) -> Writer:
    """Opens a file for appending, recovering it first.

    A pending seal journal is replayed and a torn record is cut off the tail
    before the handle is returned.
    """
    return Writer(Container.open_append(os.fspath(path)))


@contextmanager
def reading(path: StrPath) -> Iterator[Reader]:
    """:func:`open_read` as a context manager."""
    reader = open_read(path)
    try:
        yield reader
    finally:
        reader.close()
