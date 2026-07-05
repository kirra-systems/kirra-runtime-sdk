# Orin rootfs A/B drill (WS-4 / Track 3)

Validate the **rootfs-level** A/B path — the whole-slot scheme backed by the Jetson
bootloader via `nvbootctrl`, driven by `NvbootctrlBootController`
(`crates/kirra-ota-installer/src/nvbootctrl.rs`). See `docs/ota/ROOTFS_AB_DESIGN.md`
for the design.

> ⚠️ **This touches the bootloader's active slot and involves REBOOTS.** Unlike the
> app-level drill (a `systemctl restart`), a mistake here can boot the node into a
> slot without a working image. Do the **inspection** steps freely; only do the
> **live slot switch** (§2) if you have a recovery path (a known-good other slot, or
> host-side `flash`/recovery-mode access). On a robot you use day-to-day, read §0–§1,
> confirm the command mapping, and defer §2/§3 until you're set up to recover.

---

## 0. Confirm `nvbootctrl` exists and the mapping matches

The controller maps `A⇒0`, `B⇒1` and uses these exact subcommands. Verify your
`nvbootctrl` speaks them (versions differ slightly):

```sh
which nvbootctrl && nvbootctrl --help 2>&1 | head -30
nvbootctrl get-number-slots        # expect 2 (A/B capable)
nvbootctrl get-current-slot        # 0 or 1  → our Slot::A / Slot::B
nvbootctrl dump-slots-info         # per-slot retry_count + boot_successful
```

`dump-slots-info` is the durable trial state the two-phase driver reads (design §4):
a `boot_successful: 0` on the current slot means "in an uncommitted trial".

**If `get-current-slot` prints something other than `0`/`1`/`A`/`B`,** tell me the
exact output — the controller fail-closes on tokens it can't parse (it will NOT guess
a slot), and I'll widen `parse_slot` to your version's format.

## 1. Dry-run the command sequence (no reboot)

Our controller issues, for a healthy A→B update:

```text
get-current-slot                 # → 0  (running A)
set-active-boot-slot 1           # arm trial slot B  (begin_trial)
# ... reboot into B, health-probe ...
mark-boot-successful             # commit B          (report_health true)
```

and for an unhealthy trial: `set-active-boot-slot 0` (rollback to A) instead of
`mark-boot-successful`. You can confirm the *arming* half non-destructively by reading
state before and after a `set-active-boot-slot` **without rebooting**, then undoing it:

```sh
nvbootctrl get-current-slot                 # note current, e.g. 0
OTHER=1; [ "$(nvbootctrl get-current-slot)" = "1" ] && OTHER=0
sudo nvbootctrl set-active-boot-slot "$OTHER"   # arm the other slot for NEXT boot
nvbootctrl dump-slots-info                   # the other slot is now the active/next
# UNDO before any reboot — re-arm the slot you're actually running:
sudo nvbootctrl set-active-boot-slot "$(nvbootctrl get-current-slot)"
nvbootctrl dump-slots-info                   # back to the running slot
```

This proves `set_try_boot`/`rollback` map correctly **without ever rebooting into an
unproven slot.** For most validation this is enough — the state machine itself is
unit-tested against these exact commands.

## 2. (Optional, needs recovery path) Live trial + auto-fallback

Only with a known-good other slot or host recovery access. This demonstrates the
bootloader's native one-shot fallback: arm the other slot, reboot into it WITHOUT
marking it successful, and watch the retry counter carry you back.

```sh
CUR=$(nvbootctrl get-current-slot); OTHER=$([ "$CUR" = 0 ] && echo 1 || echo 0)
sudo nvbootctrl set-active-boot-slot "$OTHER"
nvbootctrl dump-slots-info                   # OTHER active, retry_count reset, successful=0
sudo reboot
# ... after it comes up ...
nvbootctrl get-current-slot                  # if OTHER: the trial booted
# DO NOT mark-boot-successful. Reboot again (and again) to exhaust the retry counter:
sudo reboot
# Eventually the bootloader marks OTHER unbootable and falls back:
nvbootctrl get-current-slot                  # back to CUR (auto-fallback)
nvbootctrl dump-slots-info                   # OTHER: successful=0 / unbootable
```

**Expected:** an uncommitted trial slot is abandoned automatically after its retries —
the availability guarantee, on real hardware. To commit instead, run
`sudo nvbootctrl mark-boot-successful` while booted in the trial slot.

## 3. (Deferred) Full image update

Flashing a NEW rootfs image into the inactive slot partition and staging it against
the campaign's signed digest is the two-phase driver (design §4) — device- and
partition-layout-specific, and it needs Secure Boot key enrollment + dm-verity slot
images (design §5). Not part of this drill; it's the on-device follow-up once §0–§2
confirm the slot machinery on your board.

---

## What to send back

For §0–§1 (safe): paste `get-number-slots`, `get-current-slot`, `dump-slots-info`, and
the before/after of the §1 non-rebooting arm/undo. That confirms our controller's
command mapping against your `nvbootctrl` version. If any output format differs from
`0/1`, include it verbatim and I'll adjust `parse_slot`.
