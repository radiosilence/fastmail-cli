# fastmail-cli as a container. Default command runs the MCP server over HTTP —
# this is the image the mcp-gateway proxies to as a backend (token per request
# via X-Fastmail-Token). Build deps (clang/cmake) are for the kreuzberg /
# bundled-pdfium native build.

FROM rust:1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/fastmail /usr/local/bin/fastmail
EXPOSE 8080
ENTRYPOINT ["fastmail"]
CMD ["mcp", "--http", "0.0.0.0:8080"]
