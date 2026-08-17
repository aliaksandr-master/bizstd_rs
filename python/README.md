# bizstd for Python

Not built yet. This file marks the place and states the shape so the layout is
readable before the code arrives.

Two packages, released together on the same major and minor as everything else
in this repository:

| Package | What it is |
|---|---|
| `bizstd-binary` | the compiled extension — one wheel per platform, nothing else in it |
| `bizstd` | pure Python: the API people actually call, depending on `bizstd-binary` |

The split is the one `pydantic` and `pydantic-core` use, and it exists for the
same reason. A compiled wheel has to be built for every platform and Python
version combination; a pure-Python package does not. Keeping the API in the
pure package means it can be read, patched and released without a build matrix,
and means a platform without a prebuilt wheel fails with "no binary for this
platform" rather than dragging in a compiler.

The extension is built with PyO3 and maturin against `abi3`, so one wheel per
platform covers every supported Python rather than one per version.

Planned targets: Linux x86-64 and aarch64 (glibc and musl), macOS x86-64 and
arm64, Windows x86-64.
