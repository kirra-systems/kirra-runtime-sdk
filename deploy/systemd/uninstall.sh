#!/usr/bin/env bash
#
# Remove the Kirra systemd services. Leaves /etc/kirra (your secrets) and
# /var/lib/kirra (the verifier DB) in place by default — pass --purge to delete them.
#
#   sudo deploy/systemd/uninstall.sh [--purge]
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "error: run as root (sudo)"; exit 1; }

PURGE=0
[[ "${1:-}" == "--purge" ]] && PURGE=1

systemctl disable --now kirra.target kirra-verifier.service kirra-planner.service kirra-taj.service 2>/dev/null || true
rm -f /etc/systemd/system/kirra-verifier.service \
      /etc/systemd/system/kirra-planner.service \
      /etc/systemd/system/kirra-taj.service \
      /etc/systemd/system/kirra.target
rm -rf /etc/systemd/system/kirra-verifier.service.d
rm -rf /opt/kirra
systemctl daemon-reload
echo "removed units + /opt/kirra binaries"

if [[ "$PURGE" == "1" ]]; then
  rm -rf /etc/kirra /var/lib/kirra
  echo "purged /etc/kirra (secrets) and /var/lib/kirra (DB)"
else
  echo "kept /etc/kirra (secrets) and /var/lib/kirra (DB) — pass --purge to remove them"
fi
