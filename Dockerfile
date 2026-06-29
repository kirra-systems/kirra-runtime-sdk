# This image builds the GATEWAY (kirra_verifier_service) only — target-agnostic
# and unchanged. The Parko inference BACKENDS run on a per-target base-image
# matrix (one framework, per-target base), using the same target names as
# scripts/install-parko-backend.sh and the scheduler descriptor strings:
#
#   ort-cpu    → this CPU base is fine (ONNX Runtime CPU)
#   openvino   → an Intel / OpenVINO runtime base
#   tensorrt   → an NVIDIA L4T / JetPack base (TensorRT-enabled ORT, aarch64/Jetson)
#   qnn / ti-tidl / amd-vitis → vendor-SDK bases (gated; slots, not yet real)
#
# Do NOT fork this file per target. The per-target backend layer installs via
# scripts/install-parko-backend.sh (host or container); per-target base images
# are documented in INSTALL.md "Multi-Backend / Multi-Chipset Install". One
# gateway image + one target-parameterized backend flow.

# ── Stage 1: build ───────────────────────────────────────────────────────────
# #686: digest-pinned (multi-arch index digest) so the build is reproducible and
# a re-tagged upstream image can't silently change the build. The `:1-alpine` tag
# is kept for readability; the `@sha256:` digest is authoritative. Bumped by
# Dependabot (docker ecosystem) — see .github/dependabot.yml.
FROM rust:1-alpine@sha256:f87aa870663e2b57ec8c69de82c7eedf7383bee987eef7612c0359635eaadb41 AS builder

RUN apk add --no-cache musl-dev gcc

WORKDIR /build
COPY . .

RUN cargo build --release --bin kirra_verifier_service

# ── Stage 2: runtime ─────────────────────────────────────────────────────────
# #686: digest-pinned (see Stage 1). Bumped by Dependabot (docker ecosystem).
FROM alpine:3@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b

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
