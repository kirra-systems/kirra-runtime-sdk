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

kirra-ota-ctl stage "$NEW" "$DIGEST"
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
kirra-ota-ctl commit
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
kirra-ota-ctl stage "$BAD" "$DIGEST"

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

## Follow-ups (after this drill passes)

- A small **health-probe agent** that runs the gate automatically (probe the new
  governor for N seconds post-restart, then `commit` or leave it to roll back),
  and the thin HTTP agent that pulls the assigned digest from the verifier's
  `/fleet/campaigns/assignment/{node_id}` (#829) to drive `stage`.
- The **rootfs-level `nvbootctrl` `BootController`** (the "generalize to node
  software" step) + secure/measured boot (Orin UEFI Secure Boot + dm-verity).
