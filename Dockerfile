FROM rust:1.93-slim-bullseye AS builder

WORKDIR /usr/src/dockerproxy

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --target x86_64-unknown-linux-musl
RUN mkdir -p /image/cache /image/data/cache

FROM debian:bullseye-slim AS certs

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

FROM scratch

WORKDIR /

COPY --from=builder /usr/src/dockerproxy/target/x86_64-unknown-linux-musl/release/dockerproxy /dockerproxy
COPY --from=builder --chown=65532:65532 /image/cache /cache
COPY --from=builder --chown=65532:65532 /image/data /data
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

USER 65532:65532
VOLUME ["/cache", "/data"]
EXPOSE 8080

ENTRYPOINT ["/dockerproxy"]
CMD ["--config-file", "/data/options.json", "--cache-dir", "/data/cache"]
