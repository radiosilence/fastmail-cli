# fastmail-cli as a container. Default command runs the MCP server over HTTP —
# this is the image the mcp-gateway proxies to as a backend (token per request
# via X-Fastmail-Token).
#
# The image holds the release binaries CI built; nothing is compiled here. The
# build context is a directory holding `<arch>/fastmail` for each Docker
# architecture (amd64, arm64), which CI stages from the `linux-build`
# tarballs. To build one locally:
#
#   cargo build --release --locked
#   mkdir -p dist/amd64 && cp target/release/fastmail dist/amd64/
#   docker build --platform linux/amd64 -f Dockerfile dist

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
# newer glibc builds fine and dies at exec time. cc-debian13 is trixie, 2.41,
# above the ubuntu-24.04 runners CI builds on (2.39). Bumping either end means
# re-checking both. cc-debian12 is 2.36 and would not clear the runners.
FROM gcr.io/distroless/cc-debian13

ARG TARGETARCH

# Absolute, not a bare name: there is no shell here to fall back on for PATH
# resolution.
COPY ${TARGETARCH}/fastmail /usr/local/bin/fastmail

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/fastmail"]
CMD ["mcp", "--http", "0.0.0.0:8080"]
