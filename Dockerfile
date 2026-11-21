# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY data ./data
COPY tests ./tests
RUN cargo build --locked --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 hkia \
    && mkdir -p /data /config \
    && chown -R hkia:hkia /data /config

COPY --from=build /src/target/release/hkia /usr/local/bin/hkia

ENV HKIA_DATA_DIR=/data \
    HKIA_CONFIG=/config/config.toml

USER hkia
EXPOSE 8787
ENTRYPOINT ["hkia"]
# Default to the HTTP MCP server for containers and the Cloudflare Worker.
# Override the command for CLI use, e.g. `docker run --rm hkia departures`.
CMD ["mcp", "--transport", "http", "--host", "0.0.0.0", "--port", "8787"]
