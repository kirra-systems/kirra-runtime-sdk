# Jetson root filesystem check (`e2fsck`) — shutdown-hook procedure

**Applies to:** ROSMASTER R2 / Jetson Orin NX on L4T (`yahboom`, `/dev/nvme0n1p1`
ext4 root) · **Status:** procedure validated, first run 2026-08-04 ·
**Access required:** SSH only, plus the ability to physically reach the device

---

## 1. What this document is for

The root filesystem on this platform **is never checked at boot**, and it cannot
be made to check at boot. This document explains why, and gives the one
procedure that does work: a `systemd-shutdown` hook that runs `e2fsck` after
every filesystem has been remounted read-only, immediately before the reboot.

Read §2 before trying anything else. Three obvious approaches fail here, and two
of them fail in ways that look like they worked.

## 2. Why boot-time fsck cannot work on this platform

`systemd-fsck-root.service` — the unit that would normally check the root
filesystem at boot — carries `ConditionPathIsReadWrite=!/`. It runs only when
root is **not** already read-write, so it can check before the remount.

On this platform that condition fails on every boot:

```
$ systemctl show systemd-fsck-root.service -p ActiveState -p ConditionResult
ActiveState=inactive
ConditionResult=no
```

The chain is:

1. The kernel command line is honoured — putting `ro` in
   `/boot/extlinux/extlinux.conf` **does** reach the kernel; `/proc/cmdline` and
   the `Kernel command line:` dmesg line both confirm it.
2. NVIDIA's L4T initrd then mounts the rootfs **read-write** on its own and
   switch-roots.
3. By the time systemd evaluates the condition, `/` is already read-write.
   The unit is skipped. No check ever runs.

The consequence, observed on this device: `Last checked` read
`Thu Jun 26 04:52:21 2025` across **53 mounts** and 918 GB of lifetime writes,
while every boot logged
`EXT4-fs (nvme0n1p1): warning: mounting fs with errors, running e2fsck is recommended`.

`/etc/fstab` has the correct pass field (`/dev/root / ext4 defaults 0 1`). It has
been irrelevant the whole time.

### 2.1 Approaches that do not work — do not retry these

| Approach | Why it fails |
|---|---|
| `fsck.repair=yes` on the kernel cmdline | The service never starts, so it has nothing to configure. Changes nothing. |
| `fsck.mode=force` on the kernel cmdline | Same. Both were tried together and the filesystem state was unchanged after the reboot. |
| `ro` on the kernel cmdline | Reaches the kernel, but the initrd remounts read-write before systemd. Necessary-looking and insufficient. |
| `break=premount` to get an initramfs shell | This is **not** an initramfs-tools initrd — 286 files, no `scripts/`, and **no `e2fsck` inside it**. Rebuilding an L4T initrd to add one risks an unbootable device needing a re-flash. |
| `tune2fs` to clear the error flag | Hides the flag without checking anything. Never do this. |
| `e2fsck` from a normal SSH session | The filesystem is mounted **read-write**. `e2fsck` warns `***WILL*** cause ***SEVERE*** filesystem damage` and prompts. `-y` does **not** auto-answer this prompt. Answer `n`. |

> **On that last row.** `e2fsck` skips the warning entirely when the filesystem
> is the *root* filesystem mounted **read-only** — which is exactly the state
> the procedure below creates, and exactly what `systemd-fsck-root` would have
> relied on. A prompt appearing means root is read-write and you are in the
> wrong place.

### 2.2 `init=/bin/bash` — works, but needs a console

Booting with `init=/bin/bash` gives a root shell with no services running, where
root can be remounted read-only and checked by hand. It is a legitimate route
**only if you have a serial console on `ttyTCU0` or HDMI plus a keyboard**.

With SSH-only access it takes the machine away from you: no systemd means no
networking and no `sshd`. The shell attaches to whichever `console=` appears
**last** on the command line — currently `console=tty0` (HDMI), so a
serial-only operator gets a black screen unless `console=ttyTCU0,115200` is
appended after `init=/bin/bash`.

The shutdown-hook procedure below reaches the same read-only root from an
ordinary `sudo reboot`, which is why it is the default recommendation.

## 3. The mechanism

`systemd-shutdown` is the binary PID 1 execs into for the final teardown. It
runs every executable in `/usr/lib/systemd/system-shutdown/` **after all
filesystems have been unmounted or remounted read-only**, immediately before the
reboot. Verified on this device:

```
--- root mount line ---
/dev/nvme0n1p1 / ext4 ro,relatime 0 0
```

That is the quiesced, read-only root `e2fsck` needs. The system reboots
immediately afterwards, so no stale cached metadata is written back over the
repair.

It is not a service. Nothing you have disabled for headless operation affects
it, and nothing needs enabling.

### 3.1 Where the log goes

Root is read-only at hook time, so the log cannot be written there. The hook
writes to the FAT ESP on `/dev/nvme0n1p10`, mounting it itself if
`/boot/efi` has already been unmounted (it usually has). The log is then
readable at `/boot/efi/kirra-fsck.log` after the reboot.

### 3.2 Safety properties

| Property | How |
|---|---|
| **Read-only interlock** | The hook greps `/proc/mounts` for `/ ext4 ro` and *refuses* to run `e2fsck` otherwise, logging `REFUSED`. Running against a read-write root is structurally excluded, not merely assumed. |
| **One shot** | Armed by a marker file on the ESP, which the hook deletes **before** running. An interrupted run cannot re-arm itself. |
| **Never silent** | If no writable log target can be reached, the hook exits without running anything. A repair that cannot be recorded is not performed. |
| **Bounded** | `timeout 3600`. This bounds a livelock; it will not free a process stuck in uninterruptible I/O. |
| **Disarmed by default** | Without the marker the hook exits immediately. Leaving it installed is inert. |

## 4. The hook

Install once, as root:

```bash
sudo tee /usr/lib/systemd/system-shutdown/kirra-fsck.shutdown > /dev/null <<'HOOK'
#!/bin/sh
# One-shot offline e2fsck of the root filesystem at shutdown.
# Arms only when the marker exists on the ESP; clears it before running.
DEV=/dev/nvme0n1p1
ESPDEV=/dev/nvme0n1p10
LOGDIR=""
MOUNTED=0
if grep -q ' /boot/efi vfat rw' /proc/mounts 2>/dev/null; then
    LOGDIR=/boot/efi
else
    mkdir -p /run/kirra-esp 2>/dev/null
    if /usr/bin/mount -t vfat "$ESPDEV" /run/kirra-esp 2>/dev/null; then
        LOGDIR=/run/kirra-esp
        MOUNTED=1
    fi
fi
[ -n "$LOGDIR" ] || exit 0
MARKER="$LOGDIR/kirra-fsck-request"
LOG="$LOGDIR/kirra-fsck.log"
if [ ! -e "$MARKER" ]; then
    [ "$MOUNTED" = "1" ] && /usr/bin/umount /run/kirra-esp 2>/dev/null
    exit 0
fi
/usr/bin/rm -f "$MARKER"
/usr/bin/sync
{
    echo "===== kirra shutdown hook — e2fsck ====="
    echo "when : $(/usr/bin/date -Is 2>/dev/null)"
    echo "arg  : $1"
    echo "root : $(grep ' / ' /proc/mounts)"
} >> "$LOG" 2>&1
if ! grep -q ' / ext4 ro[, ]' /proc/mounts; then
    echo "REFUSED: root not read-only — no check run" >> "$LOG" 2>&1
    /usr/bin/sync
    [ "$MOUNTED" = "1" ] && /usr/bin/umount /run/kirra-esp 2>/dev/null
    exit 0
fi
/usr/bin/timeout 3600 /usr/sbin/e2fsck -f -y "$DEV" >> "$LOG" 2>&1
echo "e2fsck exit=$?" >> "$LOG" 2>&1
echo "===== end =====" >> "$LOG" 2>&1
/usr/bin/sync
[ "$MOUNTED" = "1" ] && /usr/bin/umount /run/kirra-esp 2>/dev/null
exit 0
HOOK

sudo chmod 0755 /usr/lib/systemd/system-shutdown/kirra-fsck.shutdown
sudo sh -n /usr/lib/systemd/system-shutdown/kirra-fsck.shutdown && echo "syntax OK"
sudo wc -l /usr/lib/systemd/system-shutdown/kirra-fsck.shutdown   # expect 43
```

> **Verify the line count and `syntax OK` before trusting it.** A terminal that
> wraps a pasted heredoc can split a line, and `sh -n` will *not* catch the
> resulting bug: `if cmd` / newline / `2>/dev/null; then` is valid shell whose
> exit status comes from the redirect-only command, which is always true. A
> failed `mount` would then be treated as success.

## 5. Procedure

### 5.1 Before arming

- **Back up anything that matters, off-box.** A backup on `nvme0n1p1` is on the
  filesystem being repaired. For the verifier audit ledger, stop the service
  first and use `sqlite3 … ".backup"` rather than `cp`, then `scp` it away.
- **Have HDMI and a keyboard within reach of the robot.** The realistic outcomes
  are "clean" or "corrected"; the tail case where a repair leaves the device
  unbootable is why physical access matters.

### 5.2 Arm and run

```bash
sudo touch /boot/efi/kirra-fsck-request
ls -l /boot/efi/
sudo reboot
```

The reboot takes noticeably longer than usual, with **no output on any console**
— the check runs after everything is torn down. Do not power-cycle it.

### 5.3 Verify

```bash
sudo cat /boot/efi/kirra-fsck.log
sudo dumpe2fs -h /dev/nvme0n1p1 2>/dev/null | grep -iE "state|error|last checked|mount count"
sudo ls -la /lost+found
sudo dmesg | grep -i "ext4.*nvme0n1p1"
ls -l /boot/efi/                      # marker should be gone
```

Expect `Filesystem state: clean`, today's date on `Last checked`, `Mount count: 1`
(the check resets it), no `FS Error count` fields, an empty `/lost+found`, and no
`mounting fs with errors` line in dmesg.

### 5.4 Interpreting the exit code

`e2fsck` exit codes are a bit field:

| Exit | Meaning | Verdict |
|---|---|---|
| `0` | No errors | Success — already clean |
| `1` | Errors corrected | Success |
| `2` | Errors corrected, reboot required | Success; the reboot is the one you came up from |
| `3` | Both of the above (`1｜2`) | Success |
| `4` | Errors left **uncorrected** | **Read the log before doing anything else.** Do not re-arm blindly |
| `8` / `16` / `128` | Operational, usage, or library error | The check did not run properly; read the log |

Also read the *passes*. Damage confined to Pass 1 and Pass 5 (bitmaps, free
counts, deleted inodes) is allocation accounting — nothing lost. Output from
Pass 2, 3 or 4 (directory structure, connectivity, reference counts), or
anything appearing in `/lost+found`, means actual structural damage and deserves
a closer look.

### 5.5 Afterwards

Leave the hook installed. Without the marker it is inert, and it is the only
mechanism that can check this filesystem — §2 is not going to change. Re-run
with:

```bash
sudo touch /boot/efi/kirra-fsck-request && sudo reboot
```

To remove it entirely: `sudo rm /usr/lib/systemd/system-shutdown/kirra-fsck.shutdown`.

## 6. Record — first run, 2026-08-04

| | |
|---|---|
| Before | `clean with errors`, last checked `Thu Jun 26 04:52:21 2025`, 53 mounts |
| After | `clean`, checked `Tue Aug 4 20:43:07 2026`, mount count reset to 1 |
| Exit | `3` (errors corrected + reboot required) |
| `/lost+found` | empty |

Everything repaired was allocation accounting:

- 17 deleted inodes with zero `dtime`
- block and inode bitmap differences, **all** in the "marked in use but actually
  free" direction
- free block/inode counts wrong across ~90 groups
- 2 extent trees narrowed (an optimisation, not a defect)

Passes 2, 3 and 4 produced no output at all. No multiply-claimed blocks, no
orphans, no directory damage.

**Free blocks: 10 449 148 → 10 701 568.** That is 252 420 blocks of 4 KiB —
about 0.96 GiB — that the filesystem believed were occupied and were not, plus
128 inodes. The figure is one filesystem at one instant; it measures the size of
the accounting error at the moment of the check, not a rate, and it supports no
claim about how quickly such errors accumulate.

Leaked allocations with intact directory structure is what **lost metadata
writes** look like. This device has a documented NVMe lost-completion defect
(`nvme0: I/O N QID M timeout, completion polled`), identified as the stall
mechanism in ADR-0041 **D-15**. The two are consistent, which is suggestive
rather than conclusive: no common cause was demonstrated, and the repair fixes
the filesystem, not the device.

The measurement consequence is recorded in ADR-0041 **D-18** —
[`docs/adr/0041-world-model-persistence-architecture.md`](../adr/0041-world-model-persistence-architecture.md).

## 7. See also

- [`JETSON_WM2_PERSISTENCE_DRILL.md`](JETSON_WM2_PERSISTENCE_DRILL.md) — the
  measurement drill that surfaced the platform state
- ADR-0041 **D-15** (NVMe lost-completion mechanism), **D-18** (platform-state
  discontinuity)
