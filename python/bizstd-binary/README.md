# bizstd-binary

The compiled half of [`bizstd`](https://pypi.org/project/bizstd/). It holds the
extension module and nothing else — no convenience, no API worth calling
directly.

Install `bizstd`; it depends on this and gives you something to use.

The split exists so that the part which needs a build matrix and the part which
does not can be released independently, and so that a platform with no prebuilt
wheel fails with "no binary for this platform" instead of trying to find a
compiler.

MIT. Source: https://github.com/aliaksandr-master/bizstd_rs
