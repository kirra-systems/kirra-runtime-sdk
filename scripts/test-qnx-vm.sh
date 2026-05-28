#!/bin/bash
# Test Kirra binary on QNX x86_64 VM
# REQUIRES: QNX SDP 8.0 installed at /opt/qnx800/sdp2
# Install from: https://www.qnx.com/developers/sdp80/
# License required — apply at https://www.qnx.com/developers/
# Tracked: PARK-024

set -e

if [ ! -d "/opt/qnx800/sdp2" ]; then
    echo "ERROR: QNX SDP 8.0 not found at /opt/qnx800/sdp2"
    echo "Install QNX SDP 8.0 before running this script."
    echo "License required — apply at https://www.qnx.com/developers/"
    exit 1
fi

source /opt/qnx800/sdp2/qnxsdp-env.sh

BINARY=target/x86_64-pc-nto-qnx800/debug/kirra_verifier_service

if [ ! -f "$BINARY" ]; then
    echo "ERROR: QNX binary not found."
    echo "Run: cargo build --target x86_64-pc-nto-qnx800 --bin kirra_verifier_service"
    exit 1
fi

echo "QNX binary size: $(ls -lh "$BINARY" | awk '{print $5}')"
echo "QNX binary arch: $(file "$BINARY")"
echo "QNX binary linked: $(qnx-readelf -d "$BINARY" | grep NEEDED | head -5)"
echo ""
echo "Binary confirmed: QNX x86_64 target builds successfully"
echo "Next step: boot in QEMU with QNX image from SDP"
