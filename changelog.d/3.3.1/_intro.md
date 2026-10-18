Released 2026-07-26.

### Changed

- **The container image is distroless — there is no shell in it any more.**
  `docker run --entrypoint sh` and anything else that assumed a userland will
  stop working; `gcr.io/distroless/cc-debian13:debug` carries busybox if you
  need to get inside one. The image only ever needed to hold the binary, and
  dropping the package manager and coreutils drops that whole surface with it.
  It is not `scratch`, because three things rule that out: the system trust
  store, which reqwest reads through rustls-native-certs; libstdc++ and a
  dynamic loader, because pdfium is embedded and dlopened at runtime; and
  `/tmp`, where pdfium is extracted on first use.

### Added

- **`linux-aarch64` binaries in the release assets.** The arm64 image needs one
  built regardless, so publishing it costs nothing. Linux binaries are also
  built against an older glibc than before, which widens the range of
  distributions they run on.
