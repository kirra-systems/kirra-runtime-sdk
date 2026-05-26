# ── Stage 1: build ───────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev gcc

WORKDIR /build
COPY . .

RUN cargo build --release --bin kirra_verifier_service

# ── Stage 2: runtime ─────────────────────────────────────────────────────────
FROM alpine:3

RUN apk add --no-cache curl && \
    addgroup -S -g 1000 kirra && \
    adduser  -S -u 1000 -G kirra -h /var/lib/kirra -s /sbin/nologin kirra && \
    mkdir -p /var/lib/kirra && \
    chown kirra:kirra /var/lib/kirra

COPY --from=builder /build/target/release/kirra_verifier_service /usr/local/bin/kirra_verifier_service

USER kirra
WORKDIR /var/lib/kirra

ENV KIRRA_VERIFIER_ADDR=0.0.0.0:8090
ENV KIRRA_DB_PATH=/var/lib/kirra/kirra.db

VOLUME ["/var/lib/kirra"]
EXPOSE 8090

HEALTHCHECK --interval=10s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsSL http://localhost:8090/health || exit 1

ENTRYPOINT ["/usr/local/bin/kirra_verifier_service"]
