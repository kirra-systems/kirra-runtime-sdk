# ADR-0033: Actuation authority on the ROS/R2 deployment topology — verify-before-release at the motor boundary

| Field | Value |
|---|---|
| Status | **Proposed** — for owner sign-off; merge ratifies. |
| Date | 2026-07-09 |
| Deciders | Project / safety-case owner |
| Safety goals | **PO-2** (DFA independence — `docs/safety/OCCY_DFA.md` §3); SG3 (per-command kinematic envelope, ADR-0013); SS-002 (Degraded = decel-to-stop-and-hold, `docs/safety/SAFE_STATE_SPECIFICATION.md`) |
| Cross-refs | ADR-0031 (release-token on the actuation path — **this ADR extends that decision from the SHM path to the ROS deployment boundary**); ADR-0014 (R2 + Orin integration design, esp. the LLM→`exec()`→actuator anti-pattern clause); ADR-0013 (authenticated actuation gate / hardwired E-stop); ADR-0032 (QNX partition track); `crates/kirra-release-token` (canonical token); `crates/kirra-inline-governor/src/lib.rs:163-185` (`ActuatorStation::release` — the refusal semantics this ADR deploys); `docs/safety/GOVERNOR_KEY_PROVISIONING.md` (key sourcing) |
| Tracking | The safety finding filed alongside this ADR (see "Consequences — tracking") |

---

## Context — the finding

The safety case asserts checker authority in two places:

- `docs/safety/OCCY_DFA.md:30-31` — **PO-2 (Independence):** *"the QM planner cannot
  defeat or corrupt the ASIL-D Governor, and no common cause disables both."* Without
  PO-2, ISO 26262-9 voids the ASIL decomposition.
- `docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md:189-191` — the integration
  design exists so *"the build can't drift into the LLM→`exec()`→actuator anti-pattern."*

PO-2 is **enforced on the SHM/inline path** — the EP-01 FDIT fault matrix
(`crates/kirra-inline-governor/src/lib.rs`, `fdit_matrix`) and the WP-21b iceoryx2
carrier tests prove an unsigned / tampered / replayed command never releases.

PO-2 is **unenforced on the `ros2_ws` deployment topology**, in two independent ways:

1. **The bus is open.** The deployed graph
   (`ros2_ws/src/kirra_safety/launch/kirra_with_robot.launch.py:152-212` — five nodes,
   no security configuration) carries no sros2, no `permissions.xml`, no DDS security.
   Any process with bus access can `create_publisher('/cmd_vel_safe')` and command the
   motors. The `/cmd_vel_raw → cmd_vel_interceptor → /cmd_vel_safe` topology
   (`cmd_vel_interceptor.py:51-52`) is a launch-file remap — a convention, not a
   mechanism.
2. **The serial device below the bus has no owner.** ADR-0014:83-84 places the motor /
   servo driver in System 1, but nothing in-repo owns or restricts the Rosmaster
   expansion board's serial device. A process that can open the port can drive the
   motors with no ROS at all.

The mechanism that closes exactly this class of hole already exists and is
ADR-ratified for the actuation path — it is simply not deployed on this topology.

## Threat model (explicit)

**In scope:** a defective or compromised **QM component on the same host** — a buggy
planner, a hallucinating LLM/intent node, a mis-remapped launch file, a compromised
non-checker process — attempting to command motion without the checker's verdict.

**Out of scope (owned elsewhere, unchanged by this ADR):** a compromised
checker/governor (it holds the signing key), a compromised root/kernel, and physical
access. Those layers are owned by the hardwired E-stop (ADR-0013) and the QNX
partition track (ADR-0032). This ADR does not replace either.

## Decision

**(i) Cryptographic verify-before-release at the motor boundary, first.
(ii) sros2 / DDS-Security as a complementary transport perimeter, second.**

Why (i) precedes (ii):

- It reuses **ratified, measured machinery**: ADR-0031 already decided the release
  token rides the actuation path (outside the verdict WCET); `kirra-release-token` is
  the canonical construction; `ActuatorStation::release`
  (`crates/kirra-inline-governor/src/lib.rs:163-185`) defines the refusal semantics
  (no token / bad signature / non-advancing sequence / undecodable payload → refuse,
  refusals never advance the release watermark).
- It closes the **serial-port hole**, which sros2 structurally cannot see (DDS
  security governs the bus, not the device node).
- It carries **no dependency on rmw-security support** on the target distro — an
  unknown until tried on the robot.

(ii) remains worth doing after (i): it hardens the perimeter against *accidental*
publishers and unauthenticated flooding. It authenticates the **node**; (i)
authenticates the **command bytes against the checker's verdict**. They compose; they
do not substitute for each other.

## Feasibility — measured figures

ADR-0031's Phase-I measurements (**QNX 8.0 x86_64 KVM VM — INDICATIVE, not
R2-measured; no figure in this ADR was taken on the R2/Orin**):

| step | p50 | p99.9 |
|---|---|---|
| `issue_release_token` (Ed25519 sign, governor side) | 31.6 µs | 63 µs |
| `verify_release` (Ed25519 verify, actuator side) | 59.6 µs | 88 µs |

The R2's `/cmd_vel` control rate is 10–20 Hz (50–100 ms periods). Even taking the
p99.9 verify figure at face value, verification is ≤ 0.2 % of the control period.
Per ADR-0031's accounting the crypto rides the actuation path, never inside the
verdict WCET (`GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100`, `src/wcet_gate.rs:92`).
An on-target (Orin, Cortex-A78AE) measurement should be recorded when the consumer
lands, but the margin is three orders of magnitude — the decision does not hinge
on it.

## Design

Two layers, both required:

1. **The verifier mints the token.** The existing HTTP actuator path already computes
   the enforced command — middleware chain `require_secure_transport →
   require_actuator_scope → enforce_actuator_safety_envelope` threading an
   `EnforcementOutcome` (`src/bin/kirra_verifier_service.rs:1564-1584`;
   `src/gateway/policy_layer.rs`), with the epoch fence re-asserted in the handler
   (`src/bin/kirra_verifier_service/actuator.rs:54-92`). Extend the **200 response
   only** to carry an Ed25519 release token over exactly the enforced bytes plus a
   strictly-advancing sequence, using the same length-prefixed, domain-separated
   construction as the SHM path (`kirra-release-token/src/lib.rs:157-172`) under a
   **new domain** (below). The interceptor (`cmd_vel_interceptor.py:196`) already
   POSTs every command to this endpoint; it forwards the token alongside the enforced
   command.
2. **A verifying motor-consumer is the sole serial-port owner.** A dedicated consumer
   node subscribes the enforced topic, runs the `ActuatorStation` refusal semantics
   (verify → strictly-advancing sequence → decode), and only then writes the Rosmaster
   serial protocol. The serial device is owned by a dedicated user/group, mode 0600;
   only the consumer runs as that user. The fence is structural at **two layers**:
   bus (unsigned/invalid → refused) and OS (no other process can reach the port).
   A boot-time sentinel (the `src/startup_sentinel.rs` pattern) asserts the port's
   owner/mode and refuses startup on a world-writable device.

### Settled decision 1 — consumer language

**Decision: (c) now, (a) as the end-state. (b) is rejected.**

- **(c) Rust verify core via the existing FFI boundary (`src/ffi.rs`) into the Python
  consumer node** is the immediate path. The token verification, sequence watermark,
  and refusal taxonomy stay in **one Rust implementation** — the same code the SHM
  path runs — exposed as a small C-ABI surface consumed by the Python node that
  already owns the ROS + vendor-library glue. ADR-0006 Clause 3 already designates
  FFI as the C/C++/foreign integration boundary; this is that clause applied.
- **(a) a full Rust consumer speaking the Rosmaster serial protocol** is the end-state
  (drops Python from the enforcement path entirely). It requires characterizing the
  Yahboom expansion-board serial protocol **on the robot — unknown, needs hardware**.
  Do not block (c) on it.
- **(b) Python + PyNaCl is rejected**: it reimplements `ReleaseRefusal` semantics
  (`crates/kirra-inline-governor/src/lib.rs:110-124`) in a second language, creating
  two sources of truth for a safety-critical refusal rule. Divergence between them
  would be silent and unauditable. The FDIT matrix and Kani/loom evidence attach to
  the Rust implementation only.

### Settled decision 2 — domain separation

The existing token binds the frozen 176-byte `GovernorContractView` under
`DIGEST_DOMAIN = b"KIRRA-GOVERNOR-CONTRACT-DIGEST-V1"` and
`RELEASE_DOMAIN = b"KIRRA-GOVERNOR-RELEASE-V1"`
(`crates/kirra-release-token/src/lib.rs:126-130`).

**Decision: the ROS-path token gets its own payload type and its own domain pair —
`KIRRA-ROS-TWIST-DIGEST-V1` / `KIRRA-ROS-RELEASE-V1` — over a canonical fixed-width
encoding of `{sequence: u64, issued_at_ms: u64, linear_mps: f64, angular_rad_s: f64}`
(little-endian, length-prefixed, same hashing discipline as `contract_digest`).**

- A signature under one domain can never verify under the other — **no cross-path
  replay**: a captured SHM release token is not a valid ROS release and vice versa.
- The frozen-contract freeze assertions are untouched: the new payload is a separate
  type in `kirra-release-token`; nothing is added to `GovernorContractView` and the
  talisman/blob pins are unaffected.
- `issued_at_ms` is **inside the signed bytes** — required by decision 3. (The
  current `ReleaseToken` is `{digest, signature}` with no timestamp,
  `kirra-release-token/src/lib.rs:136-140`; the ROS payload carries the stamp in the
  digested image, so the 96-byte token wire shape can stay as-is.)

### Settled decision 3 — restart semantics

`ActuatorStation`'s strictly-advancing watermark is in-memory; a consumer restart
resets it.

**Decision: resync-from-zero plus a token freshness window. No durable watermark.**

Rule: on start, the consumer's watermark is empty; it accepts the first token whose
`issued_at_ms` is within the freshness window (proposed: 2 control periods, ≤ 200 ms
at 10 Hz — tuned at implementation, recorded in the config registry) and adopts its
sequence as the new baseline; thereafter strictly-advancing as today. Every token,
always, must be fresh — staleness is refusal even mid-run.

Justification vs a durable watermark:

- A durable watermark puts a **write on the actuation hot path** and a corruptible
  file on a robot SD card — a new failure mode (corrupt watermark file → consumer
  can't start → availability loss) in exchange for closing a replay window the
  freshness bound already closes: an attacker replaying a pre-restart token must do
  so within the window, and the verifier's sequences are monotonic within a boot, so
  the exposure is ≤ one stale-but-fresh command — which is still envelope-bounded
  bytes the checker signed moments earlier.
- The SHM path made the same shape of choice: refusals never poison the watermark;
  liveness and validity are separate concerns.

## Safe state on the motor side

**No valid token within the deadline window (≈ 3 missed control periods) → active
commanded stop, then output silence — decel-to-zero via the last valid envelope.
Never hold-last-command** (hold-last is the Cruise-drag failure mode SS-002 exists to
prevent, `docs/safety/SAFE_STATE_SPECIFICATION.md`). This keeps the interceptor's
existing fail-closed pattern (`cmd_vel_interceptor.py:203-279` — timeout/error/non-200
→ commanded stop) and moves it to the final authority boundary.

**A refusal must not reset the liveness window.** Refusals are not liveness — a flood
of invalid tokens must starve into the safe stop exactly as silence does. This
mirrors `ReleaseRefusal` never advancing the release watermark
(`crates/kirra-inline-governor/src/lib.rs:110-124`).

## Invariant flags

- **The deny path never mints a token.** The 400/deny body of
  `/actuator/motion/command` must remain token-free; only the 200 (enforced) arm
  carries one. Pinned by a regression case in the Tier-1 guard (below).
- `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` remains fail-closed (unset → refuse) and
  **`KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV` remains refused-by-default on the robot**
  (`docs/safety/GOVERNOR_KEY_PROVISIONING.md`). The verifying key's distribution to
  the consumer follows the same provisioning document; rotation re-provisions the
  consumer before the verifier signs under the new key.
- No existing fail-closed invariant is loosened by anything in this ADR.

## Regression guard (the evidence artifact)

- **Tier 1 (CI, no ROS, blocking):** cross-process test — consumer with a **mock
  serial seam** (the `NvbootctrlRunner` command-seam precedent,
  `crates/kirra-ota-installer/src/nvbootctrl.rs`); inject (a) valid signed command,
  (b) unsigned, (c) replayed, (d) token-over-different-bytes, (e) a deny response
  carrying a token (must be treated as protocol violation). Assert exactly one
  serial write occurred and it was (a)'s enforced bytes.
- **Tier 2 (launch-level, Gazebo/Orin, per-release):** `launch_testing` case — full
  `kirra_with_robot` graph plus a rogue node flooding `/cmd_vel_safe` with unsigned
  Twists; assert the robot does not move and the consumer's refusal counter
  increments.
- **Tier 3 (OS, boot):** startup-sentinel check on the serial device's owner/mode.

## Consequences

**Enforces:** command authenticity and freshness end-to-end on the deployed topology
— a rogue publisher's bytes carry no valid token and are refused at the last hop
regardless of topic remaps; no non-consumer process reaches the serial device.

**Does not enforce:** DoS/flooding (a rogue can delay valid commands — converted to a
safe stop by the liveness window, not prevented); a compromised verifier (holds the
key); consumer-process compromise. Residual owned by ADR-0013 (hardwired E-stop) and
ADR-0032 (QNX partition).

**Ordering constraint:** on the real R2 **the verifying consumer IS the motor
bringup**. Do not stand up a vendor `/cmd_vel`-subscribing driver first and retrofit
the fence — that ships the hole this ADR exists to close.

**Tracking:** the gap itself is filed as a safety finding (issue), independent of any
demo. Until this ADR's mechanism is on the path, the honest claim is: *the checker is
the authority for every command that routes through the interceptor; nothing prevents
a component from bypassing it on the bus or the serial port.* `OCCY_DFA.md` PO-2
carries a scoping note to that effect; `ASSUMPTIONS_OF_USE.md` carries the
serial-port-exclusivity assumption (`AOU-ACTUATION-SERIAL-001`) until the Tier-3
sentinel enforces it.
