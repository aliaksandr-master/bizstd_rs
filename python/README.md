# bizstd for Python

Two packages, released together on the same major and minor as everything else
in this repository:

| Package | What it is |
|---|---|
| [`bizstd-binary`](bizstd-binary/) | the compiled extension — one abi3 wheel per platform, nothing else in it |
| [`bizstd`](bizstd/) | pure Python: the API people call, depending on `bizstd-binary` |

The split is the one `pydantic` and `pydantic-core` use, for the same reason. A
compiled wheel needs a build matrix; a pure-Python package does not. Keeping the
API in the pure package means it can be read, patched and released without one,
and means a platform with no prebuilt wheel fails with "no binary for this
platform" rather than hunting for a compiler.

The extension is built with PyO3 and maturin against `abi3`, so one wheel per
platform covers every supported Python rather than one per version.

## Typing

Typing is part of the package, not decoration on it:

- `py.typed` in both packages, so a consumer sees types at all;
- hand-written stubs for the extension — PyO3 emits none — and a test that
  compares them against the module that actually got built, because a stub
  which has drifted is worse than no stub: the checker agrees, confidently,
  with code that will fail;
- `mypy --strict` over both packages *and* the tests, as part of `make dev`.

Verified from the outside as well: installing the two wheels into a clean
environment and running `mypy --strict` over a consumer script finds no issues,
rejects `xxh64("not bytes")` with an `arg-type` error, and resolves
`max_frames_for(4096)` to `int`.

## Working on it

```bash
make dev          # build the extension, mypy --strict, ruff, pytest
make build        # compile and install both packages into .venv
make typecheck
make test
```

Everything runs inside a `.venv` created on demand. A language whose checks
depend on what happens to be installed globally is a language whose checks mean
nothing.

## Supported

Python 3.9 and up. Planned wheel targets: Linux x86-64 and aarch64 (glibc and
musl), macOS x86-64 and arm64, Windows x86-64.
