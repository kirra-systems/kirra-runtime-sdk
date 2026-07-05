# Orin app-level A/B governor drill (WS-4 / Track 3)

The on-device validation of the dual-slot installer: prove, on a real Jetson
Orin, that a health-gated governor update **commits** when the new slot is
healthy and **auto-rolls-back** when it isn't. This is the app-level scheme (two
governor install dirs + a systemd unit + a one-shot boot record) — no bootloader
/ `nvbootctrl` / OS reboot involved; a "trial boot" is a `systemctl restart`.

The mechanism (`kirra-ota-ctl` + `FileBootController`) is unit-tested and was
validated end-to-end in CI-equivalent runs; this drill exercises it against the
**real governor binary under systemd** on target hardware — the piece that can
only be confirmed on the device.

> Everything here uses paths from `/etc/kirra/ota.env`
> (`crates/kirra-ota-installer/deploy/kirra-ota.env.example`). Adjust to your box.
>
> **`sudo` note:** the mutating commands (`stage` / `commit` / `rollback`) write
> the root-owned slot dirs (`/opt/kirra/...`) and boot record (`/var/lib/kirra/...`),
> so run them with `sudo`. `run` already runs as root under systemd; `status` is
> read-only and needs no privilege. A `stage` that can't write its slot fails
> **closed** — the boot record is left un-armed, so a permission error can never
> leave a half-staged trial.

---

## 0. Build + install (once)

Build `kirra-ota-ctl` for the Orin (aarch64). On the Orin itself:

```sh
cargo build --release -p kirra-ota-installer --bin kirra-ota-ctl
sudo install -m0755 target/release/kirra-ota-ctl /usr/local/bin/
```

Lay out the slots + config + unit:

```sh
sudo mkdir -p /opt/kirra/slots/a /opt/kirra/slots/b /var/lib/kirra /etc/kirra
# Put the CURRENT governor binary in slot A (the starting active slot):
sudo cp /path/to/your/kirra-governor /opt/kirra/slots/a/kirra-governor

sudo cp crates/kirra-ota-installer/deploy/kirra-ota.env.example /etc/kirra/ota.env
sudo cp crates/kirra-ota-installer/deploy/kirra-governor.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now kirra-governor
```

Sanity check:

```sh
kirra-ota-ctl status
# active=a try_boot=None trying=None -> run would launch slot a (/opt/kirra/slots/a/kirra-governor)
systemctl status kirra-governor    # should be running slot A's governor
```

---

## 1. HEALTHY update → commit

Stage a NEW governor build into the inactive slot (verified against its SHA-256),
trial-boot it, confirm health, commit.

```sh
NEW=/path/to/new/kirra-governor
DIGEST=$(sha256sum "$NEW" | cut -d' ' -f1)

sudo kirra-ota-ctl stage "$NEW" "$DIGEST"
# staged into slot b (/opt/kirra/slots/b/kirra-governor); `systemctl restart` to trial-boot it
kirra-ota-ctl status
# active=a try_boot=Some("b") trying=None -> run would launch slot b ...

sudo systemctl restart kirra-governor    # trial-boots slot B (one-shot consumed)
kirra-ota-ctl status
# active=a try_boot=None trying=Some("b")   <- in trial: still backed by A

# --- health gate: confirm the NEW governor is actually healthy ---
# e.g. it answers its health endpoint / the process is stable for N seconds:
curl -fsS http://127.0.0.1:8090/health && echo OK

# Healthy → commit. B becomes the permanent active.
sudo kirra-ota-ctl commit
# committed: active slot is now b
kirra-ota-ctl status
# active=b try_boot=None trying=None -> run would launch slot b ...
```

**Expected:** the governor keeps running the new slot-B build; a later restart
still runs B.

---

## 2. UNHEALTHY update → automatic rollback

Stage a deliberately-broken governor (e.g. one that exits non-zero on start) and
confirm the node reverts to the known-good slot **without operator action**.

```sh
# reset to a known A-active baseline for the drill
printf '{"active":"a","try_boot":null,"trying":null}' | sudo tee /var/lib/kirra/boot-record.json

BAD=/path/to/broken/kirra-governor      # crashes / exits immediately
DIGEST=$(sha256sum "$BAD" | cut -d' ' -f1)
sudo kirra-ota-ctl stage "$BAD" "$DIGEST"

sudo systemctl restart kirra-governor    # trial-boots slot B → it crashes
# systemd (Restart=always) restarts the unit; `run` now sees the consumed
# one-shot and reverts to slot A automatically — do NOT run commit.
sleep 5
kirra-ota-ctl status
# active=a try_boot=None trying=None -> run would launch slot a ...
systemctl status kirra-governor          # running slot A's good governor again
journalctl -u kirra-governor -n 30       # shows the trial crash + the A restart
```

**Expected:** after the bad trial slot crashes, the node is back on slot A with no
manual rollback — the one-shot `try_boot` guarantees a failed trial can't stick.

---

## What to send back

For each path, paste: the `kirra-ota-ctl status` lines, the `systemctl status`
one-liner, and the relevant `journalctl` excerpt. If anything diverges (paths,
systemd `Type=exec` vs your systemd version, the health probe), tell me and I'll
adjust the unit / CLI.

## 3. Automatic health gate (`probe`) — hands-off commit/rollback

Drills 1–2 gated health by hand (`curl … && commit`). The `probe` subcommand
automates the decision: after a trial boot it samples a health command and either
commits (a `--successes` streak of healthy samples within `--window-secs`) or rolls
back and restarts onto the good slot. It is a **no-op unless the node is in a trial**
(`trying` set), so it is safe to run on every start.

### 3a. Healthy trial → probe auto-commits

```sh
# baseline: active=b (from Drill 1). Stage the healthy v2 again into slot a.
cat >/tmp/gov-h <<'EOF'
#!/bin/sh
echo "governor (v-probe healthy) pid $$"; exec sleep infinity
EOF
chmod 0755 /tmp/gov-h
sudo kirra-ota-ctl stage /tmp/gov-h "$(sha256sum /tmp/gov-h | cut -d' ' -f1)"
sudo systemctl restart kirra-governor            # trial-boot slot a
kirra-ota-ctl status                             # trying=Some("a")

# The health check here is just "the unit is running" — swap in a real endpoint probe.
sudo kirra-ota-ctl probe --cmd 'systemctl is-active --quiet kirra-governor' \
  --window-secs 12 --interval-secs 2 --successes 3
# ... sample healthy=true streak=1/3 / 2/3 / healthy (3/3) -> committed: active slot is now a
kirra-ota-ctl status                             # active=a try_boot=None trying=None
```

### 3b. Unhealthy trial → probe auto-rolls-back

```sh
# Stage a governor that STAYS UP but is UNHEALTHY (running, but the probe fails).
cat >/tmp/gov-sick <<'EOF'
#!/bin/sh
echo "governor (v-probe SICK: up but unhealthy) pid $$"; exec sleep infinity
EOF
chmod 0755 /tmp/gov-sick
sudo kirra-ota-ctl stage /tmp/gov-sick "$(sha256sum /tmp/gov-sick | cut -d' ' -f1)"
sudo systemctl restart kirra-governor            # trial-boot the sick slot
kirra-ota-ctl status                             # trying=Some(<inactive>)

# A probe that always FAILS (exit 1) simulates "process up, but not actually healthy" —
# the case the crash-based rollback in Drill 2 can NOT catch. probe rolls back + restarts.
sudo kirra-ota-ctl probe --cmd 'false' --window-secs 8 --interval-secs 2 --successes 3
echo "probe exit=$?"                             # non-zero (unhealthy)
sleep 3
kirra-ota-ctl status                             # back to the prior active; trying=None
systemctl status kirra-governor --no-pager -l | head -6
```

**Expected:** 3a commits with no operator decision; 3b rolls back an *up-but-unhealthy*
trial (which Drill 2's crash-rollback would have missed) and restarts onto the good
slot. `probe` needs `sudo` (writes the boot record; restarts the unit on rollback).

To make this fully automatic on every trial boot, install the example
`deploy/kirra-ota-probe.service` oneshot (edit its `--cmd`) and wire it after the
governor unit — see that file's header.

## Hardware result (GATE C evidence)

Both paths were run end-to-end on a **Jetson (Yahboom X3, aarch64) under systemd
`Type=exec`**, 2026-07-06, with stand-in governor scripts (a healthy `exec sleep
infinity`, a broken `exit 1`):

- **Healthy → commit:** `stage` (SHA-256 verified) → `systemctl restart` consumed
  the one-shot `try_boot`, boot record went to `{"active":"a","try_boot":null,
  "trying":"b"}` (in trial, still fenced by A), the new slot ran (`Main PID` was the
  slot binary, not `kirra-ota-ctl` — confirming the `exec` handoff), `commit` →
  `{"active":"b",...}`. ✓
- **Unhealthy → automatic rollback:** staged a governor that exits 1 into the
  inactive slot, `systemctl restart` trial-booted it; the journal shows the broken
  trial `status=1/FAILURE`, `Restart=always` relaunched, and `run` reverted to the
  good active slot with **no operator action** — boot record settled back to
  `{"active":"b","try_boot":null,"trying":null}`. ✓

Two things the hardware surfaced, both fixed in the shipped artifacts:
- `stage`/`commit`/`rollback` need `sudo` (root-owned slot + record dirs) — noted above.
- `StartLimitIntervalSec` / `StartLimitBurst` must live in `[Unit]`, not `[Service]`
  (systemd v230+ ignored them under `[Service]` with an "Unknown key name" warning,
  silently disabling the crash-loop backstop) — moved in `deploy/kirra-governor.service`.

## Follow-ups (after this drill passes)

- A small **health-probe agent** that runs the gate automatically (probe the new
  governor for N seconds post-restart, then `commit` or leave it to roll back),
  and the thin HTTP agent that pulls the assigned digest from the verifier's
  `/fleet/campaigns/assignment/{node_id}` (#829) to drive `stage`.
- The **rootfs-level `nvbootctrl` `BootController`** (the "generalize to node
  software" step) + secure/measured boot (Orin UEFI Secure Boot + dm-verity).
