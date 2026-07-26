# fastmail-cli as a container. Default command runs the MCP server over HTTP —
# this is the image the mcp-gateway proxies to as a backend (token per request
# via X-Fastmail-Token).
#
# This expects `fastmail` to already be in the build context, built for the
# right architecture against Debian bookworm's glibc. CI does that in
# build-linux; to build the image by hand:
#
#   docker run --rm -v "$PWD:/app" -w /app rust:1-bookworm \
#     sh -c 'apt-get update && apt-get install -y --no-install-recommends clang cmake pkg-config && cargo build --release --locked'
#   cp target/release/fastmail . && docker build -t fastmail-cli .
#
# Compiling in here instead would be self-contained, but it duplicates a build
# CI has already done — it was the two slowest jobs in the pipeline.
#
# The glibc in the builder must be no newer than the one here: symbol
# versioning is forward-compatible only, so a binary built on a newer base
# fails at exec time on this one. Both are bookworm — keep them in step.
FROM debian:bookworm-slim

# Not optional: reqwest resolves roots through rustls-native-certs, which reads
# the system trust store rather than carrying its own. Without this every TLS
# handshake fails.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# pdfium is embedded in the binary and extracted to a temp dir on first use, so
# there is no library to install alongside it.
COPY fastmail /usr/local/bin/fastmail

EXPOSE 8080
ENTRYPOINT ["fastmail"]
CMD ["mcp", "--http", "0.0.0.0:8080"]
