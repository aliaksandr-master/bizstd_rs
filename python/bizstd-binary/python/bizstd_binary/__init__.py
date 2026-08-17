"""The compiled half of :mod:`bizstd`.

Everything here comes from the extension module. The names are re-exported so
that ``bizstd_binary.Container`` works and so that this package — rather than a
bare shared object — is what carries :pep:`561` typing information.

Import :mod:`bizstd` instead of this. What lives here is the boundary: values
moved across it and errors mapped onto exceptions. The API worth calling is one
package up.
"""

from ._native import (
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
    __version__,
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
    "RebuildReport",
    "RecordLayout",
    "RepackReport",
    "Schema",
    "ValidateReport",
    "__version__",
    "max_frames_for",
    "peek_headers",
    "rebuild_headers",
    "repack",
    "split_records",
    "validate",
    "xxh64",
]
