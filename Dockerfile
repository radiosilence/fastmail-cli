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
# only FROM does.
FROM bin-${BIN_SOURCE} AS bin

FROM debian:bookworm-slim

# Not optional: reqwest resolves roots through rustls-native-certs, which reads
# the system trust store rather than carrying its own. Without this every TLS
# handshake fails, so a scratch or distroless base is not an option here.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# BuildKit only builds the stage this resolves to: bin-prebuilt never looks for
# dist/ on a source build, and the Rust builder never runs on a prebuilt one.
#
# glibc invariant — whatever builds the binary must be no newer than this base.
# Symbol versioning is forward-compatible only, so a binary linked against a
# newer glibc builds fine and dies at exec time. bookworm is 2.36; CI builds on
# ubuntu-22.04 (2.35) for that reason, and the source path above uses bookworm
# itself. Bumping either end means re-checking both.
#
# pdfium is embedded in the binary and extracted to a temp dir on first use, so
# there is no library to install beside it. That extract-and-dlopen is also why
# a fully static musl build isn't available here.
COPY --from=bin /fastmail /usr/local/bin/fastmail

EXPOSE 8080
ENTRYPOINT ["fastmail"]
CMD ["mcp", "--http", "0.0.0.0:8080"]
