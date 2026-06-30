# QNX SDP 8.0 x86_64 VM — Relaunch Runbook (#274)

Reproducible recipe for spinning up the QNX 8.0 `mkqnximage`/QEMU VM and running
the on-target FDIT matrix + `wcet_measure`. Recovered verbatim from the bring-up
session.

**This run's one change vs. last time: KVM instead of TCG.** Last time the dev
laptop had VT-x disabled (no `/dev/kvm`), so QEMU fell back to TCG software
emulation and the WCET numbers were jitter-dominated (`max ~2.2 ms`). With Intel
virtualization now enabled, QEMU can use KVM (near-native), making the
acceptance-#4 attempt (`max < 100 µs`) reachable as a **Phase-I-indicative**
result.

> All commands run on the **dev laptop**, not in CI/sandbox. Paths reflect that
> machine (`/opt/qnx800/sdp2`, `~/qnxvm`, `/home/justin/...`).

## Environment

| Thing | Value |
|---|---|
| SDP install | `/opt/qnx800/sdp2` |
| SDP env script | `source /opt/qnx800/sdp2/qnxsdp-env.sh` |
| `mkqnximage` | `/opt/qnx800/sdp2/host/common/bin/mkqnximage` |
| VM workdir | `~/qnxvm` |
| VM IP / host | `172.31.1.10` / `qnxvm` (bridge `br0`) |
| Target tuple | `x86_64-pc-nto-qnx800` |
| Toolchain | judge: `cargo -Zbuild-std=core` (core-only, no QNX std); C++: `q++` QCC 12.2.0 |

The `~/qnxvm/local/snippets/*.custom` overlay files **persist** across rebuilds —
if `~/qnxvm` was not wiped, the bake-in + auto-run blocks are still present and
you can skip straight to the quick path.

## Quick path (overlay still present from last time)

```bash
# 1. SDP env
source /opt/qnx800/sdp2/qnxsdp-env.sh

# 2. Re-cross-build the binaries the overlay references (regenerates build-qnx/)
cd ~/kirra-runtime-sdk
tools/qnx-rtm-harness/run_qnx_fdit.sh

# 3. Confirm KVM is now live (the point of the reboot)
ls -l /dev/kvm && grep -Eoc '(vmx)' /proc/cpuinfo

# 4. Rebuild the image + boot headless
cd ~/qnxvm
/opt/qnx800/sdp2/host/common/bin/mkqnximage --build --run=-h

# 5. Read the auto-run results off the serial console
sleep 30
sed -n '/KIRRA-274 FDIT START/,/KIRRA-274 DONE/p' ~/qnxvm/output/qemu.out
```

## Make sure step 4 actually uses KVM

Last time the generated QEMU line was `--cpu max` with **no** `-enable-kvm` (TCG,
because `/dev/kvm` was absent). Check whether `--run` picked up KVM this time:

```bash
grep -m1 'QEMU Command' ~/qnxvm/output/qemu.out   # look for -enable-kvm / accel=kvm
```

If it's **still not** KVM, build the image without running, then launch QEMU
directly with acceleration (last time's exact generated command + `-enable-kvm
-cpu host`, duplicate `-nographic` dropped):

```bash
cd ~/qnxvm
/opt/qnx800/sdp2/host/common/bin/mkqnximage --build      # rebuild IFS only, don't run
qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 1G \
  -drive file=output/disk-qemu.vmdk,if=ide,id=drv0 \
  -netdev bridge,br=br0,id=net0 -device virtio-net-pci,netdev=net0,mac=52:54:00:e3:a6:d3 \
  -pidfile output/qemu.pid -nographic -kernel output/ifs.bin \
  -serial file:output/qemu.out \
  -object rng-random,filename=/dev/urandom,id=rng0 -device virtio-rng-pci,rng=rng0 &
sleep 30
sed -n '/KIRRA-274 FDIT START/,/KIRRA-274 DONE/p' ~/qnxvm/output/qemu.out
```

The `br0` bridge + `qemu-bridge-helper` were already configured last time (network
came up), so this should just work.

## If `~/qnxvm` is fresh — recreate it + the bake-in (full recipe)

```bash
# create the VM (bridge net, root login, ssh key, headless)
mkdir -p ~/qnxvm && cd ~/qnxvm
/opt/qnx800/sdp2/host/common/bin/mkqnximage \
  --type=qemu --ip=172.31.1.10 --hostname=qnxvm \
  --users=root/root --ssh-ident="$HOME/.ssh/id_ed25519.pub" \
  --build --run=-h

# bake the 3 binaries into the boot IFS (they land in /proc/boot -> on PATH)
cat >> ~/qnxvm/local/snippets/ifs_files.custom <<'EOF'
[perms=0755 uid=0 gid=0] rtm_harness = /home/justin/kirra-runtime-sdk/tools/qnx-rtm-harness/build-qnx/rtm_harness
[perms=0755 uid=0 gid=0] wcet_measure = /home/justin/kirra-runtime-sdk/tools/qnx-rtm-harness/build-qnx/wcet_measure
[perms=0755 uid=0 gid=0] kirra_demo = /home/justin/kirra-runtime-sdk/tools/qnx-rtm-harness/build-qnx/kirra_demo
EOF

# auto-run them at boot; output goes to the serial console (output/qemu.out)
cat >> ~/qnxvm/local/snippets/post_start.custom <<'EOF'
echo "===== KIRRA-274 FDIT START ====="
rtm_harness
echo "KIRRA-274 FDIT_EXIT=$?"
echo "===== KIRRA-274 WCET START ====="
wcet_measure
echo "KIRRA-274 WCET_EXIT=$?"
echo "===== KIRRA-274 DONE ====="
EOF
```

> **Auto-run + the cert gate (`wcet_measure`, after PR #734).** The bake-in above
> runs `wcet_measure` with no env, so it self-declares `other` /
> `INDICATIVE-NOT-WCET` — correct for a VM. To tag KVM provenance in the banner,
> change the `post_start.custom` line to `KIRRA_WCET_PLATFORM=kvm wcet_measure`.
> **Never** set `KIRRA_WCET_CERTIFIED=1` on the VM — that flag is reserved for the
> Phase-II certified hardware and is the only thing that promotes a row to
> `QNX-TARGET-MEASURED`.

Then run steps 4–5 (or the direct-KVM launch) above.

## Useful adjuncts

```bash
/opt/qnx800/sdp2/host/common/bin/mkqnximage --getip    # prints 172.31.1.10
/opt/qnx800/sdp2/host/common/bin/mkqnximage --stop     # stop the VM
tail -n 80 ~/qnxvm/output/qemu.out                     # watch boot / console
```

## After the run — honesty labelling

- **Gate = verdict correctness.** `GATE: PASS` (all 9 rows) is the load-bearing
  result; emulation/virtualization does not affect it.
- **WCET under KVM is `INDICATIVE-KVM`, not cert-grade.** A KVM VM is near-native
  but is still a VM on an x86_64 laptop, not the deployment ISA. Record the row as
  `x86_64-pc-nto-qnx800(KVM) / INDICATIVE-KVM` — do **not** promote it to
  `QNX-TARGET-MEASURED`.
  - *Token vs. label.* `INDICATIVE-KVM` is the **human label** for the recorded
    results file / `QNX_MAPPING.md` prose. The machine **CSV token** emitted by
    `wcet_measure` is the canonical `INDICATIVE-NOT-WCET` (with `env=other`,
    `sched=host-default`, and `platform=kvm` in the human banner) — see
    `QNX_MAPPING.md` §7. They are consistent: both say "not WCET evidence"; the
    `(KVM)` provenance lives in the banner + this label, not in the canonical
    `wcet_status` column (which stays a `kirra_timing::MeasurementEnv` token so the
    host-vs-target join holds).
- **Cert-grade WCET is Phase-II:** NVIDIA DRIVE AGX (Orin/Thor) + QNX OS for
  Safety + Ferrocene-qualified Rust under SCHED_FIFO. That number, not a VM
  figure, backs the FTTI claim.
- Save the `KIRRA-274 …` block to
  `tools/qnx-rtm-harness/results/qnx800-x86_64-vm-kvm.txt` (beside the TCG one);
  if `max < 100 µs`, update `QNX_MAPPING.md §6.1` and `WCET_QNX_BRINGUP.md` to
  record the KVM-indicative acceptance-#4 attempt.

## See also

- `tools/qnx-rtm-harness/README.md` — harness overview, the FDIT matrix
- `tools/qnx-rtm-harness/run_qnx_fdit.sh` — the cross-build driver
- `tools/qnx-rtm-harness/QNX_MAPPING.md` §6 / §6.1 — on-target run + recorded result
- `docs/adr/KIRRA_QNX_RUNBOOK.md` — the general cross-build → deploy → run runbook
- `docs/safety/WCET_QNX_BRINGUP.md` — full recipe + Phase-I acceptance criteria
- `tools/qnx-rtm-harness/results/qnx800-x86_64-vm-tcg.txt` — prior TCG run evidence
