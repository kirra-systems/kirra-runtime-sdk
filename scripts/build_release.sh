#!/usr/bin/env bash
# scripts/build_release.sh
#
# Builds Aegis release binaries for all supported targets.
# Run this on your development machine before a GitHub release.
#
# Prerequisites (install once):
#   cargo install cross           # cross-compilation tool
#   # cross uses Docker internally — Docker must be running
#
# Usage:
#   ./scripts/build_release.sh [VERSION]
#   VERSION defaults to the current git tag or "dev"
#
# Output:
#   dist/
#     aegis-v1.0.0-x86_64-linux.tar.gz
#     aegis-v1.0.0-aarch64-linux.tar.gz
#     aegis-v1.0.0-armv7-linux.tar.gz
#     SHA256SUMS

set -euo pipefail

VERSION="${1:-$(git describe --tags --exact-match 2>/dev/null || echo "dev")}"
DIST_DIR="dist"
BINARY="aegis_verifier_service"
CARLA_BINARY="aegis_carla_client"

TARGETS=(
    "x86_64-unknown-linux-musl"
    "aarch64-unknown-linux-musl"
    "armv7-unknown-linux-musleabihf"
)

TARGET_NAMES=(
    "x86_64-linux"
    "aarch64-linux"
    "armv7-linux"
)

echo "Building Aegis ${VERSION}"
echo "Targets: ${TARGETS[*]}"
echo ""

# Clean and create dist directory
rm -rf "${DIST_DIR}"
mkdir -p "${DIST_DIR}"

# Check for cross
if ! command -v cross &>/dev/null; then
    echo "ERROR: 'cross' not found. Install with: cargo install cross"
    echo "       Docker must also be running."
    exit 1
fi

for i in "${!TARGETS[@]}"; do
    TARGET="${TARGETS[$i]}"
    TARGET_NAME="${TARGET_NAMES[$i]}"
    ARCHIVE_NAME="aegis-${VERSION}-${TARGET_NAME}.tar.gz"

    echo "Building ${TARGET_NAME}..."

    # Build main verifier service
    cross build \
        --release \
        --target "${TARGET}" \
        --bin "${BINARY}"

    # Build CARLA client
    cross build \
        --release \
        --target "${TARGET}" \
        --bin "${CARLA_BINARY}"

    # Create staging directory
    STAGING="$(mktemp -d)"
    STAGING_BIN="${STAGING}/aegis"
    mkdir -p "${STAGING_BIN}"

    # Copy binaries
    cp "target/${TARGET}/release/${BINARY}"       "${STAGING_BIN}/"
    cp "target/${TARGET}/release/${CARLA_BINARY}" "${STAGING_BIN}/"

    # Copy supporting files
    cp install.sh                    "${STAGING}/"
    cp INSTALL.md                    "${STAGING}/"
    cp LICENSE 2>/dev/null           "${STAGING}/" || true
    cp README.md 2>/dev/null         "${STAGING}/" || true

    # Copy systemd unit template
    mkdir -p "${STAGING}/systemd"
    cp scripts/aegis-verifier.service "${STAGING}/systemd/"

    # Write version file
    echo "${VERSION}" > "${STAGING}/VERSION"

    # Package
    tar -czf "${DIST_DIR}/${ARCHIVE_NAME}" -C "${STAGING}" .
    rm -rf "${STAGING}"

    echo "  → ${DIST_DIR}/${ARCHIVE_NAME}"
done

# Generate checksums
echo ""
echo "Generating checksums..."
(cd "${DIST_DIR}" && sha256sum ./*.tar.gz > SHA256SUMS)
cat "${DIST_DIR}/SHA256SUMS"

echo ""
echo "Release ${VERSION} built successfully."
echo "Files in ${DIST_DIR}/:"
ls -lh "${DIST_DIR}/"
echo ""
echo "Next steps:"
echo "  1. Test an archive: tar -xzf ${DIST_DIR}/aegis-${VERSION}-x86_64-linux.tar.gz"
echo "  2. Create GitHub release and upload contents of ${DIST_DIR}/"
echo "  3. Or push a git tag to trigger automated release via GitHub Actions"
