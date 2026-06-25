#!/usr/bin/env bash
#
# Install the Kirra governor stack (verifier + Occy planner + Taj perception) as
# systemd services that come up on boot. Single-box (the Orin): everything local.
#
#   sudo deploy/systemd/install.sh
#
# What it does (idempotent):
#   1. creates the `kirra` system user,
#   2. copies the three release binaries to /opt/kirra/,
#   3. generates /etc/kirra/kirra.env with strong RANDOM secrets if absent
#      (never overwrites an existing one; no secret is ever committed),
#   4. installs the unit files + a PartOf= drop-in so `kirra.target` owns the
#      verifier too,
#   5. enables + starts kirra.target (so the stack boots automatically).
#
# Build the binaries first (or run scripts/orin_bringup.sh):
#   cargo build --release --bin kirra_verifier_service
#   cargo build --release -p kirra-mick --example planner_service --example taj_service
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "error: run as root (sudo deploy/systemd/install.sh)"; exit 1; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REL="${KIRRA_RELEASE_DIR:-$REPO/target/release}"
UNITS="$REPO/deploy/systemd"
OPT=/opt/kirra
ENVDIR=/etc/kirra
ENVFILE="$ENVDIR/kirra.env"

echo "== 1. kirra system user =="
if id kirra &>/dev/null; then
  echo "  user 'kirra' exists"
else
  useradd --system --no-create-home --shell /usr/sbin/nologin kirra
  echo "  created system user 'kirra'"
fi

echo "== 2. binaries -> $OPT =="
declare -A SRC=(
  [kirra_verifier_service]="$REL/kirra_verifier_service"
  [planner_service]="$REL/examples/planner_service"
  [taj_service]="$REL/examples/taj_service"
)
for name in "${!SRC[@]}"; do
  [[ -x "${SRC[$name]}" ]] || {
    echo "error: missing ${SRC[$name]} — build it first:"
    echo "  cargo build --release --bin kirra_verifier_service"
    echo "  cargo build --release -p kirra-mick --example planner_service --example taj_service"
    exit 1
  }
done
install -d -m 0755 "$OPT"
for name in "${!SRC[@]}"; do
  install -m 0755 "${SRC[$name]}" "$OPT/$name"
  echo "  installed $OPT/$name"
done

echo "== 3. secrets -> $ENVFILE =="
install -d -m 0750 -o kirra -g kirra "$ENVDIR"
if [[ -f "$ENVFILE" ]]; then
  echo "  $ENVFILE exists — leaving it untouched (your secrets are preserved)"
else
  gen() { LC_ALL=C tr -dc 'a-f0-9' < /dev/urandom | head -c "$1"; }
  admin="$(gen 64)"          # admin bearer token (64 hex chars)
  reset="$(gen 48)"          # supervisor reset key (48 bytes, <= 64)
  umask 077
  cat > "$ENVFILE" <<EOF
# /etc/kirra/kirra.env — generated $(date -u +%FT%TZ) by deploy/systemd/install.sh.
# SECRETS — keep private (mode 600). Regenerate by deleting this file and re-running.
KIRRA_ADMIN_TOKEN=$admin
KIRRA_SUPERVISOR_RESET_KEY=$reset
EOF
  chown kirra:kirra "$ENVFILE"; chmod 600 "$ENVFILE"
  echo "  generated $ENVFILE with random secrets"
  echo "  (read the admin token for clients: sudo sed -n 's/^KIRRA_ADMIN_TOKEN=//p' $ENVFILE)"
fi

echo "== 4. unit files + verifier PartOf drop-in =="
install -m 0644 "$UNITS/kirra-verifier.service" /etc/systemd/system/kirra-verifier.service
install -m 0644 "$UNITS/kirra-planner.service"  /etc/systemd/system/kirra-planner.service
install -m 0644 "$UNITS/kirra-taj.service"      /etc/systemd/system/kirra-taj.service
install -m 0644 "$UNITS/kirra.target"           /etc/systemd/system/kirra.target
# Make kirra.target own the verifier too, without editing the committed unit.
dropin=/etc/systemd/system/kirra-verifier.service.d
install -d -m 0755 "$dropin"
cat > "$dropin/10-kirra-target.conf" <<'EOF'
[Unit]
PartOf=kirra.target
EOF
echo "  installed 4 units + PartOf drop-in"

echo "== 5. enable + start =="
systemctl daemon-reload
systemctl enable kirra.target kirra-verifier.service kirra-planner.service kirra-taj.service >/dev/null
systemctl restart kirra.target
echo
echo "done — the Kirra stack is enabled on boot."
echo "  status:  systemctl status kirra.target kirra-verifier kirra-planner kirra-taj"
echo "  logs:    journalctl -u kirra-verifier -f"
echo "  stack:   verifier :8090  ·  planner :8100  ·  taj :8101"
