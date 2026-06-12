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

> **Scope note (what's wired here vs. next):** this path wires the *delivery* and
> the *stop tie-in* — a grant releases the loop, and an immobilized loop forces a
> stop. **Feeding the loop from live collision detection** (so the node *enters*
> the immobilized state on a real impact, not only in tests) is the named
> follow-up; until it lands, the node-owned loop is exercised by the delivery
> path and the test harness.
