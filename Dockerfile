FROM rust:1.93-alpine AS builder

WORKDIR /usr/src/dockerproxy

RUN apk add --no-cache build-base

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release \
    && cp target/release/dockerproxy /dockerproxy
RUN mkdir -p /image/cache /image/data/cache

FROM --platform=$BUILDPLATFORM alpine AS certs

RUN apk add --no-cache ca-certificates

FROM scratch

WORKDIR /

COPY --from=builder /dockerproxy /dockerproxy
COPY --from=builder /image/cache /cache
COPY --from=builder /image/data /data
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

VOLUME ["/cache", "/data"]
EXPOSE 8080

ENTRYPOINT ["/dockerproxy"]
CMD ["--config-file", "/data/options.json", "--cache-dir", "/data/cache"]
