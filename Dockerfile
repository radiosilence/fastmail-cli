# fastmail-cli as a container. Default command runs the MCP server over HTTP —
# this is the image the mcp-gateway proxies to as a backend (token per request
# via X-Fastmail-Token).

# Selects which stage supplies the binary. Must be declared before the first
# FROM to be usable in one. `docker build .` compiles from source as it always
# did; CI passes prebuilt to reuse the binary the release matrix already built,
# rather than compiling the crate a second time in here.
ARG BIN_SOURCE=source

# Build deps (clang/cmake) are for the kreuzberg / bundled-pdfium native build.
FROM rust:1-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --locked && cp target/release/fastmail /fastmail

FROM scratch AS bin-source
COPY --from=builder /fastmail /fastmail

FROM scratch AS bin-prebuilt
COPY dist/fastmail /fastmail

# Indirection through a stage is required: --from takes no variable expansion,
# only FROM does. BuildKit then only builds the stage this resolves to, so
# bin-prebuilt never looks for dist/ on a source build and the Rust builder
# never runs on a prebuilt one.
FROM bin-${BIN_SOURCE} AS bin

# Distroless rather than a distro base: the binary is the only thing this image
# needs to contain, so there is no shell, no package manager and no apt layer.
# It is not `scratch` because three things rule that out, all verified against
# this image:
#
#   - /etc/ssl/certs/ca-certificates.crt — reqwest resolves roots through
#     rustls-native-certs, which reads the system trust store rather than
#     carrying its own. Without it every TLS handshake fails.
#   - libstdc++ / libgcc_s / libc — kreuzberg embeds pdfium and dlopens it at
#     runtime, so a dynamic loader and the C++ runtime have to be present.
#     That extract-and-dlopen is also why a fully static musl build is out.
#   - /tmp — where pdfium is extracted on first use (std::env::temp_dir()).
#
# glibc invariant: whatever builds the binary must be no newer than this base.
# Symbol versioning is forward-compatible only, so a binary linked against a
# newer glibc builds fine and dies at exec time. cc-debian13 is trixie, 2.41 —
# comfortably above the ubuntu-24.04 runners CI builds on (2.39) and the
# bookworm source path above (2.36). Bumping either end means re-checking both.
# Note cc-debian12 is 2.36 and would not clear the runners.
FROM gcr.io/distroless/cc-debian13

# Absolute, not a bare name: there is no shell here to fall back on for PATH
# resolution.
COPY --from=bin /fastmail /usr/local/bin/fastmail

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/fastmail"]
CMD ["mcp", "--http", "0.0.0.0:8080"]
