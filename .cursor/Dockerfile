FROM rust:1.96-bookworm

ENV DEBIAN_FRONTEND=noninteractive

# Cursor Cloud Agents use this image for repository work. Rust 1.96 provides an
# edition-2024-aware Cargo, which is required by the locked getrandom v0.4.2
# manifest. Keep native build tools here so `cargo test --locked` works without
# per-agent rustup/toolchain setup.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    build-essential \
    pkg-config \
    clang \
    libclang-dev \
    cmake \
    git \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:${PATH}

WORKDIR /workspace
