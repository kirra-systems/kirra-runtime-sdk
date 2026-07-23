# Operator Console — Demo Runbook

> ## ⚠️ DEMO DATA — NOT EVIDENCE
> Everything this runbook seeds is a **demonstration**, not a safety artifact.
> Node ids are **`KIRRA-DEMO-*`** so they're unmistakable in any screenshot. The
> seeded audit chain is genuinely signed (so the console's verification flags are
> real), but **nothing here enters any safety case** — it is a desk demo of the
> real machinery, with synthetic inputs.

Five minutes, copy-paste, **zero hardware**. You'll seed a demo fleet, serve the
console, walk a real SG6 escalation → operator clearance → Phase-B delivery, and
reset.

---

## 1. One-time: generate an audit signing key

The seeded chain is **signed** so its verification is real (not decoration). Make
a base64 32-byte Ed25519 seed (the form `KIRRA_LOG_SIGNING_KEY` expects):

```sh
export KIRRA_LOG_SIGNING_KEY="$(openssl rand -base64 32)"
export KIRRA_DB_PATH="kirra_demo.sqlite"
export KIRRA_ADMIN_TOKEN="demo-admin"
export KIRRA_SUPERVISOR_RESET_KEY="demo-supervisor-key"
```

Keep this shell open (the seeder, the service, and the delivery example all need
the **same** `KIRRA_LOG_SIGNING_KEY` so the chain verifies end-to-end).

## 2. Seed the demo store

```sh
cargo run --bin kirra_console_demo_seed
```

This refuses to run against a non-empty store (it only initializes fresh ones) and
requires `KIRRA_LOG_SIGNING_KEY`. It writes 6 `KIRRA-DEMO-*` nodes and the
`KIRRA-DEMO-03` SG6 sequence (`RSS_VIOLATION → TRAJECTORY_MRC_FALLBACK →
ImpactDetected → ImpactEscalationRaised`) as **real, signed** chain events, then
prints the exact next command.

## 3. Serve the console

```sh
cargo run --bin kirra_verifier_service
```

Open **http://127.0.0.1:8090/console**.

## 4. Walk the demo

1. **See the fleet** — 6 tiles. `KIRRA-DEMO-03` is `Untrusted` (post-collision
   latch); `KIRRA-DEMO-05` carries the `flood_condition_active` note.
2. **Open the escalation docket** — `KIRRA-DEMO-03`'s `ImpactDetected` (vanished-
   object trigger) + `ImpactEscalationRaised`, signed, with no later clear → open.
3. **Record a clearance grant** — in the grant card enter node `KIRRA-DEMO-03`, an
   operator id, and the supervisor key `demo-supervisor-key` (the value of
   `KIRRA_SUPERVISOR_RESET_KEY`). Submit → the card shows **`GRANT RECORDED —
   DELIVERY PENDING`** and a signed `OperatorClearanceGrantIssued` row appears in
   the chain feed. **The vehicle is NOT released yet** — the grant is recorded and
   signed; delivery is the node's job.
4. **Deliver it** — in a second terminal (same env vars):

   ```sh
   cargo run -p parko-kirra --example deliver_clearance -- \
     --db "$KIRRA_DB_PATH" --node KIRRA-DEMO-03
   ```

   This is the **Phase-B loop, live** — it stands in for the parko-ros2 node tick;
   `poll_and_deliver` is the exact call the node will make. It takes the pending
   grant (one-shot), re-validates it at the `ClearanceLoop`, and prints
   **`DELIVERED · CLEARED`**.
5. **Refresh the console** — the grant card flips to **`DELIVERED · CLEARED`** and
   a signed `ClearanceDelivered` row is in the feed.

## 5. Reset

```sh
rm -f "$KIRRA_DB_PATH" "$KIRRA_DB_PATH"-wal "$KIRRA_DB_PATH"-shm
```

A fresh `KIRRA_DB_PATH` re-seeds cleanly (step 2).

---

## Operator onboarding (#314 Phase 1 — operator-proven identity)

The console's primary clearance path is **operator-signed**: a registered operator
proves possession of their Ed25519 private key by signing a server challenge, so a
grant becomes **non-repudiable chain evidence** (the signed ledger records *which
key* authorized the release), not a self-reported string. The attestation pattern,
applied to humans.

(`$KIRRA_ADMIN_TOKEN` is exported in step 1; `$BASE` is the running service.)

```sh
export BASE="http://127.0.0.1:8090"
```

### 1. Generate an operator keypair (the private key NEVER leaves the operator)

```sh
openssl genpkey -algorithm ed25519 -out operator_priv.pem      # PRIVATE — keep secret
openssl pkey -in operator_priv.pem -pubout -out operator_pub.pem  # PUBLIC — to register
```

### 2. Register the operator's PUBLIC key (admin-gated — a SEPARATE power from the supervisor key)

```sh
jq -n --arg op alice --arg pem "$(cat operator_pub.pem)" \
   '{operator_id:$op, ed25519_pubkey_pem:$pem}' \
 | curl -s -X POST "$BASE/console/operators" \
     -H "authorization: Bearer $KIRRA_ADMIN_TOKEN" \
     -H "content-type: application/json" --data-binary @-
# → {"operator_id":"alice","operator_key_fingerprint":"...","status":"registered"}
```

Revoke with `POST $BASE/console/operators/alice/revoke` (also admin-gated). A revoked
operator can never clear a grant.

### 3. Sign a grant — in the browser (primary)

In the grant card, enter the node id + operator id, **paste the operator private key
PEM** (held in memory for the session only — never `localStorage`, never sent; the
page signs locally via WebCrypto Ed25519), and submit. The page fetches a one-time
challenge, signs the canonical payload, and posts the signature. The grant card shows
the **auth method** + the **key fingerprint**.

### 3-alt. Sign a grant — CLI (when WebCrypto Ed25519 is unavailable)

The console feature-detects WebCrypto Ed25519; if the browser lacks it the in-page
form is replaced with a pointer here (it does **not** silently fall back to
break-glass). Sign from the CLI instead — this Python snippet mirrors the server's
canonical payload exactly:

```python
import base64, struct, json, urllib.request
from cryptography.hazmat.primitives.serialization import load_pem_private_key

BASE, op, node = "http://127.0.0.1:8090", "alice", "robot-01"
priv = load_pem_private_key(open("operator_priv.pem","rb").read(), None)
nonce = json.load(urllib.request.urlopen(
    f"{BASE}/console/clearance-challenge?operator_id={op}&node_id={node}"))["nonce"]
lp = lambda s: struct.pack("<Q", len(s.encode())) + s.encode()        # u64-LE length prefix
payload = b"KIRRA-OPERATOR-GRANT-v1" + lp(op) + lp(node) + lp(nonce)   # == operator_grant_signing_payload
sig = base64.b64encode(priv.sign(payload)).decode()
req = urllib.request.Request(f"{BASE}/console/clearance-grants", method="POST",
    data=json.dumps({"node_id":node,"operator_id":op,"nonce":nonce,"signature_b64":sig}).encode(),
    headers={"content-type":"application/json"})
print(urllib.request.urlopen(req).read().decode())
```

### 4. Request an emergency stop — operator-signed (ADR-0013)

The console's second authenticated affordance is the **"Request emergency stop"** card
— the clearance loop **inverted**. It is a signed **REQUEST** to the governor, never a
console→actuator command: the operator signs, the fail-closed governor judges it and
commands the MRC (controlled decel-to-stop · HOLD → sticky `LockedOut`) under **its own**
authority. Recovery is a deliberate supervisor reset, not automatic. It is **supplementary
to the hardwired physical E-stop, never the primary safety mechanism**.

In the browser: enter node id + operator id, paste the operator PEM (memory only), tick
the acknowledgement, and submit — the page signs the **STOP-distinct** payload
(`KIRRA-OPERATOR-ESTOP-v1`) and posts to `/console/estop-requests`. It is **operator-signed
ONLY — there is no break-glass** (ADR-0013 #2: the stop must be non-repudiable, provably a
specific operator's; a shared key cannot give that). When WebCrypto Ed25519 is unavailable,
sign from the CLI — identical to the grant snippet above with two changes: the domain tag
and the endpoint:

```python
# ... same challenge fetch + `lp` length-prefix helper as the grant snippet above ...
payload = b"KIRRA-OPERATOR-ESTOP-v1" + lp(op) + lp(node) + lp(nonce)  # == operator_stop_signing_payload
sig = base64.b64encode(priv.sign(payload)).decode()
req = urllib.request.Request(f"{BASE}/console/estop-requests", method="POST",
    data=json.dumps({"node_id":node,"operator_id":op,"nonce":nonce,"signature_b64":sig}).encode(),
    headers={"content-type":"application/json"})
print(urllib.request.urlopen(req).read().decode())   # -> {"status":"stop_commanded","governor_action":"MRC_COMMANDED",...}
```

### Break-glass policy (the named, audited supervisor fallback)

The shared supervisor key (`KIRRA_SUPERVISOR_RESET_KEY`, #255) **remains** as an
explicitly-named **break-glass** path — recorded distinctly as
`auth_method="supervisor-break-glass"` and logged loudly — **because a fleet locked
out of recovery by a lost operator key is its own safety failure**. It is the
console's *secondary* affordance (a collapsed "Break-glass" panel), never the
primary. **Deployments that don't want it disable it by unsetting
`KIRRA_SUPERVISOR_RESET_KEY`** (the route then fail-closes 503 for break-glass while
operator-signed grants keep working). Use it only when an operator key is genuinely
unavailable; every use is auditable and visibly distinct from operator-signed grants.

---

## What you're looking at (one pane at a time)

For a first-time viewer — each pane is a window onto a **real** mechanism, not a
mock.

- **Fleet posture (tiles).** The per-node trust state straight from the verifier's
  durable store (`load_nodes`) — `Trusted` / `Untrusted(reason)`. The
  `flood_condition_active` and post-collision notes are the actual trust-state
  reasons the **posture engine** records, not labels painted on for the demo.

- **Escalation docket.** Open SG6 escalations **derived from the signed audit
  chain** — `ImpactDetected` / `ImpactEscalationRaised` events with no later
  `ImpactCleared`. These are the production event types (parko-kirra's impact
  sink); the docket is the same fail-closed view the operator sees in the field.

- **Clearance grant form.** The console's **one** authenticated affordance. The
  supervisor key is verified by `constant_time_compare` against
  `KIRRA_SUPERVISOR_RESET_KEY` (the #255 mechanism) — a bad key is a real 401.
  Well-formedness (non-empty operator, **registered** node) is checked server-side,
  and the timestamp is the **server's** clock (no client-supplied time → no
  future-dating). The grant is recorded **and cryptographically signed**; it does
  not release anything.

- **Audit chain feed.** Every row is **Ed25519-signed and hash-linked**; the
  `valid` flag is real signature verification under the configured key, and the
  masthead `chain: verified` is the hash-chain integrity check (`chain_intact`).
  The clearance lifecycle you just drove — `OperatorClearanceGrantIssued` →
  `ClearanceDelivered` — is in this same tamper-evident ledger.

- **Delivery (the example / the node tick).** The grant is taken **exactly once**
  (an atomic `UPDATE … RETURNING` — double-pickup is impossible) and re-validated
  at the `ClearanceLoop` at **delivery** time: a grant that sat too long is
  rejected at the loop even though the verifier accepted it at record time (the
  **two-checkpoint** design). A rejected/stale grant is consumed, never retried —
  the operator re-issues.

> The console is **QM** and **posture-exempt** (reachable during LockedOut — the
> posture it exists to recover from). The governor judges; the console does not.

---

## On the vehicle (the deployed delivery path)

The desk demo above runs the delivery by hand (`cargo run --example
deliver_clearance`). **On a real vehicle the node delivers grants on its own
tick** — no manual step. The `parko-ros2` node, when configured, calls
`poll_and_deliver` every tick: a console-recorded grant is picked up and clears
the post-collision loop on the node's next cycle, and the node holds the vehicle
**stopped** (regardless of posture) while that loop is immobilized.

### ⚠️ ONE store, one vehicle

The node and the vehicle's verifier **share a single SQLite store** — one
vehicle, one file. Clearance delivery is store-level pickup over that shared
store (no network hop). Point every component at the **same** path:

```sh
# THE one co-located store + the one signing key (shared by service + node):
export KIRRA_DB_PATH="/var/lib/kirra/this_vehicle.sqlite"
export KIRRA_LOG_SIGNING_KEY="$(cat /etc/kirra/log_signing_key.b64)"

# Service-only — the operator-auth secret. The NODE never sees this.
export KIRRA_SUPERVISOR_RESET_KEY="$(cat /etc/kirra/supervisor_key)"   # service shell ONLY

# Node-only — THIS vehicle's node id. REQUIRED to enable delivery; no default
# (a wrong-node grant pickup must be impossible).
export KIRRA_NODE_ID="KIRRA-DEMO-03"

# Node-only — SG6 impact-detection sources (#309). OPTIONAL; each unset topic is
# REDUCED detection coverage, logged loudly at startup (never a fabricated latch):
export PARKO_IMU_TOPIC="/imu/data"          # sensor_msgs/Imu  → decel-spike trigger
export PARKO_CONTACT_TOPIC="/bumper/contact" # std_msgs/Bool    → contact trigger
# Deployment-tunable decel threshold (m/s²); VALIDATION-PENDING, default 30.0:
export PARKO_IMPACT_SPIKE_THRESHOLD_MPS2="30.0"
```

The node reads `KIRRA_DB_PATH` first, falling back to the divergence sink's
`PARKO_DIVERGENCE_AUDIT_DB`; if both are set and **differ**, it warns and uses
`KIRRA_DB_PATH`. In the co-located deployment they must name the **same** file.

If the store + signing key + `KIRRA_NODE_ID` are not all present, the node logs
a loud warning and runs with **clearance delivery disabled** (the dev lane) —
console grants will not be delivered. If the store/key are present but
`KIRRA_NODE_ID` is missing, the node **refuses to start** (it will not guess a
node id).

> **The node never holds the supervisor key.** Operator authentication is the
> **service's** job (the `KIRRA_SUPERVISOR_RESET_KEY` grant form, #255). The node
> only delivers what the verifier already recorded and signed — it authenticates
> nothing itself.

### Day-one flow

1. **Launch the stack** with the env above — the verifier service *and* the
   `parko-ros2` node against the one store.
2. **The console shows real posture** (`/console`) — the live fleet, not seed
   data, this time.
3. **Record a grant** for the immobilized vehicle in the grant form (operator id
   + supervisor key), exactly as in the desk demo.
4. **The node's own tick delivers it** — `poll_and_deliver` picks up the grant,
   re-validates it at the `ClearanceLoop`, and on success the post-collision hold
   lifts and motion resumes. A `ClearanceDelivered` row appears in the same
   signed chain. No manual `deliver_clearance` run.

### What latches the loop for real (#309)

The node now **arms itself** from live evidence each tick — it no longer needs an
external driver to enter the post-collision hold:

- **Decel (IMU).** When `PARKO_IMU_TOPIC` is set, the node reads the IMU
  linear-acceleration magnitude per tick; a value above
  `PARKO_IMPACT_SPIKE_THRESHOLD_MPS2` (default 30 m/s², well above the ~9.81 m/s²
  gravity baseline) is a collision-grade impact → the loop latches and the
  vehicle stops.
- **Contact.** When `PARKO_CONTACT_TOPIC` is set, a `true` reading is a
  definitive impact → latch.
- **Vanished-object — NOT yet wired.** This trigger needs an `AgentScene` per
  tick, and no scene source flows through the node's tick yet. It is the **named
  remainder of #309**; until it lands, a person-under-vehicle *disappearance* does
  not latch from the tick (decel + contact still do).

A **missing** IMU or contact topic is **reduced detection coverage**, stated
loudly at startup — never a fabricated latch. Once latched (by any armed
trigger), recovery is exactly the delivery flow above: record a grant → the
node's own tick delivers it → the hold lifts.

> **Held line (delivery before detection):** within a tick the node delivers a
> pending grant *before* it runs detection. A grant consumed on a tick is matched
> against the loop state at pickup; it can never clear an impact that latches on
> that *same* tick. If both happen at once, the grant clears the old escalation,
> detection re-latches on the new evidence, the vehicle stays stopped, and the
> operator re-issues against the new escalation.
