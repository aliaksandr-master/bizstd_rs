# bizstd for Node.js

Not built yet. This file marks the place and states the shape so the layout is
readable before the code arrives.

One package, `bizstd`, released on the same major and minor as everything else
in this repository.

Built with napi-rs, which produces a real native addon rather than a runtime
FFI binding. That matters for two reasons: there is no foreign-function library
to install alongside it, and the per-platform binaries ship as their own
optional packages (`bizstd-darwin-arm64` and so on) that npm resolves
automatically. A machine with no prebuilt binary fails at install with a clear
message instead of at the first call.

Planned targets: Linux x86-64 and aarch64 (glibc and musl), macOS x86-64 and
arm64, Windows x86-64.
