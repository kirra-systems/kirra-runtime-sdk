#!/usr/bin/env bash
# make_gov_seed.sh — write the 32-byte governor signing seed for the verifier's
# file: signing-key source, so the verifier's MINTED releases verify against the
# R2 consumer's pinned KIRRA_GOVERNOR_VK_HEX.
#
# 🔴 WHY THIS EXISTS (load-bearing): the verifier's built-in `dev-fixed` source
# signs with seed 0x07×32, but the R2 consumer (run_consumer_r2.sh) pins the
# pubkey of 0x2a×32. Mismatched → every release is SignatureInvalid at the
# consumer → permanent safe stop + latched key_mismatch_alarm. To make them
# agree you MUST run the verifier with a file: source holding the SAME 2a seed:
#     ./robot/install/make_gov_seed.sh                 # writes /etc/kirra/gov_2a.seed (0600)
#     export KIRRA_GOVERNOR_SIGNING_KEY_SOURCE=file:/etc/kirra/gov_2a.seed
#
# 🔴 DEV/DEMO ONLY. The 2a seed is the well-known bench key — NEVER on a
# production/golden unit (that path is a real provisioned governor key).
set -euo pipefail

OUT="${1:-/etc/kirra/gov_2a.seed}"
# 64 hex chars = 32 bytes. Default = the bench 2a seed the consumer pins; override
# with KIRRA_GOV_SEED_HEX to pair with a differently-enrolled consumer.
SEED_HEX="${KIRRA_GOV_SEED_HEX:-$(printf '2a%.0s' $(seq 1 32))}"
if [ "${#SEED_HEX}" -ne 64 ]; then
  echo "FATAL: seed must be 64 hex chars (32 bytes), got ${#SEED_HEX}" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUT")"
# hex → raw 32 bytes (python3 is always present on this unit; avoids an xxd dep).
python3 -c "import sys; sys.stdout.buffer.write(bytes.fromhex('$SEED_HEX'))" > "$OUT"
chmod 600 "$OUT"

echo "wrote 32-byte governor seed → $OUT (mode 600, ${#SEED_HEX}/2 = $(( ${#SEED_HEX} / 2 )) bytes)"
echo "point the verifier at it:"
echo "    export KIRRA_GOVERNOR_SIGNING_KEY_SOURCE=file:$OUT"
echo "the verifier's pubkey will then equal the consumer's KIRRA_GOVERNOR_VK_HEX pin."
