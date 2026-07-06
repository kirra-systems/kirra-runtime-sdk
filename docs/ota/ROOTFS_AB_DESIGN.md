# Rootfs-level A/B design (WS-4 / Track 3)

How the node-side installer generalizes from trialing the **governor app** to
trialing the **whole node image** (bootloader + kernel + rootfs), and how that
composes with secure/measured boot on a Jetson Orin.

> The pure state machine is unchanged. Only the [`BootController`] behind it changes.
> App-level and rootfs-level are the SAME [`Installer`] driving two controllers.

---

## 1. Two schemes, one contract

| | **App-level** (`FileBootController`) | **Rootfs-level** (`NvbootctrlBootController`) |
|---|---|---|
| Unit of update | one governor binary | the whole slot (bootloader+kernel+rootfs) |
| "Trial boot" | `systemctl restart` (seconds) | a **reboot** |
| One-shot / fallback | our JSON boot record + `plan_run` | the bootloader's per-slot **retry counter** |
| Durable trial state | `/var/lib/kirra/boot-record.json` | `nvbootctrl` slot metadata |
| Blast radius of a bad update | governor process | the node boots the old slot |
| Best for | fast governor iteration | kernel/driver/OS changes, tamper-evident images |

Both satisfy the same safety contract: **an unproven slot never becomes permanently
active until health is confirmed; a failed trial reverts automatically.** Because the
contract is identical, both plug into the same [`Installer`] state machine (`stage →
begin_trial → report_health → {commit | rollback}`) through the same four-method
[`BootController`] trait. The rootfs controller lives in
`crates/kirra-ota-installer/src/nvbootctrl.rs`.

## 2. `BootController` → `nvbootctrl`

| trait method | `nvbootctrl` command | effect |
|---|---|---|
| `active_slot` | `get-current-slot` | which slot we booted (`0⇒A`, `1⇒B`) |
| `set_try_boot(s)` | `set-active-boot-slot <s>` | arm next boot into `s`; the retry counter auto-falls-back if `s` is never marked successful |
| `commit(_)` | `mark-boot-successful` | make the CURRENT (trial) slot permanent |
| `rollback(s)` | `set-active-boot-slot <s>` | point the next boot back at the previous good slot |

Slot parsing is **fail-closed**: an `nvbootctrl` token the controller can't parse
(neither `0/1` nor `A/B`) is an error, never a default — guessing the active slot
could commit or roll back the wrong image. The runner is behind the
[`NvbootctrlRunner`] seam so the mapping + command sequence unit-test without a
device (see the module's tests: full commit + rollback cycles over a mock).

## 3. The one-shot is the bootloader's retry counter

`set-active-boot-slot B` arms slot B AND resets its retry counter (default 3). Each
boot attempt decrements it. If the trial reaches `mark-boot-successful` first →
committed. If the counter hits 0 first (boot loop, panic before the health probe,
power loss mid-trial) → the bootloader marks B unbootable and falls back to A on the
next boot. This is strictly stronger than the app-level single-shot: transient boot
failures get retries, and a slot that won't boot *at all* is still recovered.

## 4. The reboot-spanning two-phase driver

A rootfs trial spans a reboot, so an in-memory [`Installer`] can't be held across it —
the durable trial state is the bootloader's slot metadata. A real driver runs in two
phases, reconstructing state from `nvbootctrl` each time:

```
Phase 1 (pre-reboot, e.g. from `pull`):
  flash the new image to the INACTIVE slot partition
  verify its SHA-256 == the campaign's signed digest      # fail-closed: mismatch never arms
  nvbootctrl set-active-boot-slot <inactive>              # arm the trial (retry counter set)
  reboot

  ── bootloader boots the trial slot; on repeated boot failure it auto-falls-back ──

Phase 2 (post-boot, e.g. a systemd oneshot after multi-user.target):
  am I in a trial?  = get-current-slot != last-good AND current slot not marked successful
  health-probe the new image for N seconds
    healthy   → nvbootctrl mark-boot-successful           # commit
    unhealthy → nvbootctrl set-active-boot-slot <previous>; reboot   # rollback
```

The verifier's `/fleet/campaigns/assignment/{node_id}` (#829) supplies the assigned
signed digest for Phase 1 exactly as in the app-level `pull`; the health gate reuses
the `HealthGate` streak logic from `probe`. That two-phase driver (a `kirra-ota-ctl`
sibling that flashes a partition and shells `nvbootctrl` / `reboot`) is the on-device
follow-up — it is device- and partition-layout-specific and cannot be exercised in
CI. This module ships the reusable controller it will drive.

## 5. Secure + measured boot (Orin UEFI Secure Boot + dm-verity)

A/B answers *availability* (a bad update can't brick the node). Secure/measured boot
answers *integrity* (only an authentic image runs, and its identity is attestable):

- **UEFI Secure Boot** — the Orin verifies the signed boot chain (bootloader → kernel)
  against enrolled keys, so an unsigned/tampered slot won't boot at all. A/B still
  applies: a slot that fails signature verification is a failed trial and falls back.
- **dm-verity rootfs** — each slot's rootfs is a read-only dm-verity volume with a
  Merkle root hash; any block tampering fails the read. The campaign's **signed
  artifact digest should be (or commit to) the verity root hash**, so the same
  `verify_staged_artifact` fail-closed check that gates staging also pins the exact
  image the node will run. Because the rootfs is immutable, per-node state lives on a
  separate writable partition (not on `ReadWritePaths` inside the image).
- **Measured boot** — the boot chain extends PCRs with each stage's measurement. This
  ties into the verifier's attestation path (#73): the follow-up is to include the
  `expected_pcr16_digest_hex` measured-boot quote in `verify_attestation`, so a node
  that booted a rolled-back / unexpected slot is detectable at the trust layer, not
  just locally. (PCR16 quote verification is already a tracked follow-up on the
  verifier side.)

Together: **A/B + retry-counter fallback keep the node alive; Secure Boot + dm-verity
keep it authentic; the campaign's signed digest is the single identity that pins the
staged image; measured boot makes the running slot attestable to the fleet.**

## 6. Status

- **Live (this crate, unit-tested):** `NvbootctrlBootController` + the
  `NvbootctrlRunner` seam; the `Installer` drives it through full commit + rollback
  cycles over a mock (`nvbootctrl.rs` tests). Fail-closed slot parsing.
- **On-device follow-up:** the two-phase partition-flashing driver (§4); enrolling
  Secure Boot keys + building dm-verity slot images; the PCR16 measured-boot quote in
  `verify_attestation` (§5). Drill: `docs/ota/ORIN_ROOTFS_AB_DRILL.md`.

[`Installer`]: ../../crates/kirra-ota-installer/src/lib.rs
[`BootController`]: ../../crates/kirra-ota-installer/src/lib.rs
[`NvbootctrlRunner`]: ../../crates/kirra-ota-installer/src/nvbootctrl.rs
