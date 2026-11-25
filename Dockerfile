FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

RUN cargo build --release -p ccc-cli

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 -s /usr/sbin/nologin nuntius

WORKDIR /app
COPY --from=builder /app/target/release/ccc-cli /usr/local/bin/ccc-cli
# Preserve /app/ccc-cms/static to match CARGO_MANIFEST_DIR baked at compile time.
COPY --from=builder /app/ccc-cms/static /app/ccc-cms/static
COPY ccc.docker.toml /app/ccc.docker.toml

RUN mkdir -p /data /app/web-apps && chown -R nuntius:nuntius /data /app

USER nuntius
EXPOSE 3000 8000

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3000/health || exit 1

ENTRYPOINT ["/usr/local/bin/ccc-cli"]
CMD ["-c", "/app/ccc.docker.toml", "serve"]
