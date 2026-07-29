# Evidence binding — what is bound, and what is not

**Issue:** #1214 · **Applies to:** the R2 perception → planning → actuation path

---

## 1. Why this document exists

The evidence-binding chain is easy to over-credit. It has an impressive number
of digests in it, and a reader who sees `evidence_digest` threaded from Taj to
the motor consumer can reasonably conclude that the perception behind a command
has been *verified* somewhere along the way. It has not. Every hop validates
**shape** and carries the value forward; no component recomputes a digest it
received.

That is not a defect on its own — it is a design consequence of where the
signing authority sits — but it means the guarantee is narrower than it looks,
and a narrow guarantee mistaken for a broad one is worse than no guarantee.

## 1a. Threat boundary — read this before citing the binding as a control

> A compromised or dishonest caller inside the accepted HTTP trust boundary
> **can choose the evidence identity being signed.**
>
> It cannot exceed the independently checked command envelope or actuator
> scope, but **the signature does not prove that the identity was derived from
> genuine sensor data.**

What the binding does provide, within that boundary:

- **Attribution** — a command names the evidence frame it claims to rest on, and
  that claim is in the signed bytes and the audit record.
- **Replay resistance** — a captured frame cannot be re-presented (nonce burn),
  reordered (sequence watermark), or paired with older evidence (supersession).
- **Integrity in transit** — no hop can alter the command or its claimed
  evidence without invalidating the signature.

What it does not provide:

- **Authentication of perception.** Nothing proves the digest corresponds to a
  scan Taj actually produced.
- **Freshness of the evidence itself.** The frame identity is checked for
  ordering, not for age against the sensor.

Terminology in this repository should follow that split. "Bound", "attributed"
and "non-replayable" are accurate; "verified sensor evidence", "authenticated
perception" and "proves the digest corresponds to the frame" are not, and must
not be used unless a component actually recomputes or attests the digest.

This document records the boundary, so the next audit reads it instead of
re-deriving it. #1214's own issue text is the evidence that re-derivation is
expensive: it estimated this area as "partial foundation; major work remains"
when five of its seven items already shipped.

## 2. The chain

```
Taj  ──frame_id──▶  Occy  ──frame_id + proposal_digest──▶  interceptor
      (stamps)            (echoes, folds into digest)      (carries verbatim)
                                                                  │
                                                                  ▼
motor consumer  ◀──signed 272-byte frame──  verifier  ◀──release_binding
   (verifies signature,                     (signs whatever
    refuses stale/replayed)                  it is handed)
```

The chain is unbroken end to end on the ROS path — `occy_doer.py` forwards
Taj's `frame_id` into the plan request, `build_release_binding` copies the plan
response's identity into the proposal envelope, and `cmd_vel_interceptor`
attaches it to the actuator request without re-deriving it. No human retypes a
value.

## 3. What IS bound

| Property | Mechanism | Enforced where |
|---|---|---|
| The command bytes are the governor's | Ed25519 over exactly the presented 176 bytes | `RosBoundCommandGate::release` step 2 |
| The consumer runs the same platform profile | `profile_digest` equality against a pinned value | step 4 |
| The command is not stale | `issued_at_ms` / `expires_at_ms` against the local clock | step 5 |
| A token cannot grant itself an unbounded life | `expires_at_ms - issued_at_ms <= maximum_lifetime_ms` | step 5 |
| Commands cannot be reordered | strictly-advancing `sequence` | step 6 |
| A captured frame cannot be replayed | strictly-advancing `nonce` (a burn, constant memory) | step 7 |
| A refusal cannot burn a legitimate identity | watermarks advance only after every check passes | step 8 → advance |
| Evidence cannot go backwards | `(tracker_generation, scan_sequence)` supersession | step 8 |
| A zeroed frame cannot pass as evidence | generation `0` reserved; first live generation is `1` | Taj + planner seam |
| A proposal must name its evidence | `MISSING_PERCEPTION_FRAME_ID` | planner seam |
| A stepped wall clock cannot silently extend a token | monotonic cross-check + hold | `ClockStepGuard` |

## 4. What is NOT bound

### 4.1 No component verifies that the evidence digest is genuine

The verifier signs the `release_binding` it receives in the HTTP body. It has no
channel to Taj, never recomputes `evidence_digest`, and cannot. Anything able to
`POST /actuator/motion/command` with well-formed hex gets it signed.

**Why this is not the hole it appears to be:** that endpoint is
`SCOPE_ACTUATOR_COMMAND`-gated, and the resulting command still passes the full
kinematic envelope, posture gate and decel-to-stop checks. The binding's job is
to make evidence *attributable and non-replayable*, not to authenticate the
perception stack to the verifier. A caller that fabricates an evidence identity
does not thereby obtain a wider envelope — it obtains a signed command that
lies about its provenance in the audit record.

**What would close it:** Taj signing its own frame identity, with the verifier
holding Taj's key. Not filed; the value depends on the threat model for a
process already inside the actuator scope.

### 4.2 The planner does not use evidence identity, it carries it

`crates/kirra-planner` contains no reference to `PerceptionFrameId` at all. The
identity is validated for shape at the sidecar seam, folded into
`proposal_digest`, and echoed. The planning algorithm never sees it. A proposal
is therefore bound to *an* evidence frame, not proven to have been *computed
from* it.

### 4.3 The enforced speed cap is not in the evidence digest

`compute_perception_evidence_digest` runs before the liveness verdict and before
cap stabilization, so it covers perception's raw `speed_cap_mps`, not the
enforced `stabilized_speed_cap_mps` (#1212).

This is deliberate and it is not a gap: the digest identifies the **evidence**,
and a verdict about that evidence is a different kind of fact. What actually
needs binding is the commanded velocity — and that *is* signed, in the payload.
The cap bounds the velocity; the velocity is what reaches the wheels.

The same reasoning excludes the #1211 liveness fields and the #1210 rejection
tally. Folding a verdict into the digest would make the identity of a scan
depend on the history of unrelated scans.

### 4.4 Track identity is bound only through the digest

The signed payload carries no track id. `evidence_digest` covers every object's
id and state, so a frame naming different objects has a different identity — but
there is no way to express "this command concerns track 7" and have the gate
check it.

`(tracker_generation, object_id)` is nonetheless a stable pair: ids never
recycle within a tracker instance, and every tracker replacement routes through
`reset()`, which advances the generation. So a recycled id always arrives under
a new generation, which the supersession rule refuses.

### 4.5 Clock synchronization remains an integrator obligation

`ClockStepGuard` detects a wall-clock **step**. It does not detect **drift** —
two clocks slowly diverging while both advance smoothly produce no divergence
signal, because the guard compares elapsed intervals, not absolute offsets.

That residue is AOU-TIMESYNC-001's, and it stays the integrator's: only a
disciplined time distribution closes it. The guard converts one silent failure
into a loud one; it does not discharge the assumption.

### 4.7 Restart is a new trust epoch — and pre-restart tokens survive it

The consumer's release gate holds three watermarks — last released `sequence`,
last released `nonce`, and the newest released `(tracker_generation,
scan_sequence)` — and the clock-step guard holds its hold deadline. **None of
these are persisted.**

The model to hold, stated once and without qualification:

```
process restart
→ all in-memory replay and freshness state is lost
→ trust is re-established from scratch
→ no individual guard claims continuity across restart
```

That is the honest description, and it is the reason the clock-step hold is not
persisted on its own: doing so would make restart *look* like continuity in the
one respect that was persisted, while sequence, nonce and frame watermarks
silently reset. A partial guarantee here is worse than none, because it invites
exactly the wrong mental model.

Most of what re-establishment requires already holds. Fresh evidence is observed
after restart, a token is minted against it, startup generation and frame
identity are nonzero and accepted by the full chain (#1214), and the gate
enforces its watermarks from the first post-restart release.

**One requirement holds only at the interim tier (#1230 Part B).** The gate now
carries the consumer's boot instant (`boot_wall_ms`, captured once at process
start), and any token MINTED before it is refused with the distinct
`TokenPredatesBoot` (FFI code 111) — regardless of remaining signed lifetime.
The post-restart replay window therefore no longer scales with the configurable
`KIRRA_FRESHNESS_WINDOW_MS`: with a correctly recorded boot instant it is zero,
and the historical coupling (raise the lifetime, widen the replay window by the
same amount) is severed. The boundary is strict — a token minted at exactly the
boot instant is the current epoch — and re-establishment stays deadlock-free:
the first post-boot mint is admitted on the first cycle.

**The honest remainder, which is why #1230 stays open.** The comparison is
time-anchored on the shared host clock (sound on the ADR-0033 single-host
topology, where verifier and consumer share one clock domain). A backward wall
step DURING the restart can place the recorded boot instant before the mint it
should fence, restoring the pre-Part-B behaviour — pinned as an
expected-but-undesired test exactly as the original gap was. The structural fix
(Part A) binds a per-boot epoch into the SIGNED payload, which no clock step
can forge: wire revision V3 per ADR-0033, a new consumer-published epoch
channel attached by the interceptor, refusal `TokenEpochMismatch`. Until it
lands, `restart_trust_epoch.rs` carries both the flipped assertions and the
remaining-gap pin.

**Operational consequences.**

- Restarting the motor consumer while the system clock is being corrected, or
  immediately after a suspected replay, discards exactly the state that was
  defending against it. Treat a consumer restart with the care of an initial
  bring-up.
- `KIRRA_FRESHNESS_WINDOW_MS` is not only a freshness bound. It is also the
  post-restart replay window, and raising it widens that window by the same
  amount.

### 4.6 The bench minter binds nothing real

`kirra_ros_release_mint` defaults `--scan-seq` and `--tracker-gen` to `1` and
the digests to fixed `be11…` / `d0e5…` constants. A `frame-v2` frame with only
`--profile-digest` mints a fully-signed payload with entirely synthetic
evidence. It is marked DEV/DEMO ONLY and is how the negative tests reach each
refusal arm. It must never be on a robot's path.

## 5. Cold-start behaviour worth knowing

A restarted Taj sidecar resets `tracker_generation` to `1`, while a long-lived
motor consumer holds a supersession watermark from the previous run. The
consumer will refuse the restarted sidecar's frames as superseded until it is
itself restarted.

This is **fail-closed and correct** — the consumer cannot distinguish a restart
from a replay of old evidence — but it means *restart Taj alone* is not a valid
recovery action. Restart the consumer with it.

## 6. Cited by

- `crates/kirra-release-token/src/ros_bound_command.rs` — the gate.
- `crates/kirra-sidecars/src/taj.rs` — `PerceptionFrameId`, the digests.
- `crates/kirra-sidecars/src/planner.rs` — the seam validation.
- `robot/clock_step_guard.py` — the wall-clock step detector.
- `docs/safety/ASSUMPTIONS_OF_USE.md` — AOU-TIMESYNC-001.
