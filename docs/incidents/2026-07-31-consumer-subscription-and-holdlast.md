# 2026-07-31 — Consumer serviced timers but not subscriptions, then held last command

**Platform** Rosmaster R2 / Jetson Orin NX, Ubuntu 22.04, ROS 2 Humble, Fast DDS
**Severity** One safety-relevant behaviour (SS-002 violated in the field)
**Status** Both faults resolved and verified on hardware

---

## Summary

A verifying motor consumer executed its liveness timer but never its
`/kirra/release` subscription, while DDS discovery showed a matched pair and
other subscribers on the same host received the same messages. Fixing that
exposed a second fault: once releases stopped arriving, the last motor command
stayed latched and the wheels kept turning.

Neither was a defect in the checker, the verify core, or the doer-checker
architecture. **All four contributing faults were the deployed robot disagreeing
with the repository**, and in every case the system looked healthy: units
active, sentinel OK, `dlopen` fine, `.so` newer than the last commit.

---

## Fault 1 — subscription callbacks never dispatched

### Root cause

`install_kirra.sh` derived the robot user two different ways:

| Line | Purpose | Derivation | Under `sudo` |
|------|---------|------------|--------------|
| 97 | udev rule `OWNER` | `"${SUDO_USER:-${USER}}"` | robot user |
| 196 | systemd `User=` | `$(id -un)` | **root** |

The installer is documented to run under `sudo`, so `kirra-consumer.service`
shipped `User=root` while `/dev/myserial` stayed owned by the robot user.

Fast DDS carries discovery over UDP multicast but same-host **data** over
`/dev/shm` segments owned by their creating participant, mode 0644. A writer
must write into the reader's segment and lock its semaphore. With the consumer
as root and the publisher as the robot user, the publisher held `r--` on
`fastrtps_port*` and `sem.fastrtps_port*_mutex` and could not deliver.

Discovery still matched, so `ros2 topic info --verbose` showed a healthy
reliable/volatile pair. Timers are local, so the liveness clock kept ticking.
The result is a node that services timers, never its subscription, and looks
correct from every graph-level query.

### Evidence

Same port, ownership flipping with the fix:

```
14:10  -rw-r--r--  1 root   root    sem.fastrtps_port14427_mutex
14:17  -rw-r--r--  1 jetson jetson  sem.fastrtps_port14427_mutex
```

The serial sentinel had been reporting it at every boot for hours:

```
14:07:59  WARNING: serial exclusivity DEGRADED … /dev/myserial is owned by
          uid 1000, not the consumer's uid 0
14:14:58  serial exclusivity: OK (owner+mode 0600, no other holder)
```

RX count went 0 → 55.

### Why it took so long to find

`KIRRA_ALLOW_SHARED_SERIAL=1` was set in `robot.env`. The sentinel **detected
the uid mismatch at every single boot** and was told to downgrade its refusal to
a warning. Without that flag the consumer would have refused to start with a
message naming uid 0 versus uid 1000, and the root cause would have been on
screen immediately.

A bring-up escape hatch outlived its bring-up and suppressed the one guard
positioned to catch this.

---

## Fault 2 — last command held after releases stopped

### Root cause

`/etc/kirra/robot.env` pinned `KIRRA_CONSUMER_LIB` to
`/opt/kirra/libkirra_consumer_ffi.so`, the layout that predates the `lib/`
split. `install_kirra.sh` writes `/opt/kirra/lib/libkirra_consumer_ffi.so`.

```
caef2bb5…  /opt/kirra/libkirra_consumer_ffi.so       ← loaded
29f30719…  /opt/kirra/lib/libkirra_consumer_ffi.so   ← installed
```

Both files existed, both `dlopen`'d cleanly, and nothing compared them. The
consumer ran an old verify core whose V2 starve path never armed. Every rebuild
performed during the investigation landed in a file the consumer does not open,
so each fix appeared to work and changed nothing.

`env.template` already carried the corrected path — a freshly rendered robot
would have been fine. This bit an existing robot whose env file was never
re-rendered.

### Evidence

Before — ~100 ticks, no write, wheels turning:

```
14:38:25 ACTUATE linear=0.150   ← last release
14:38:26 TICK n=40  write=0
14:38:33 TICK n=110 write=0     ← eight seconds later, still zero
```

After pointing `KIRRA_CONSUMER_LIB` at the installed library:

```
14:59:34 ACTUATE linear=0.150  R2 WRITE motor=(10,0,0,10) reason=ok
14:59:35 ACTUATE linear=0.100  R2 WRITE motor=(7,0,0,7)   reason=ok
14:59:35 ACTUATE linear=0.050  R2 WRITE motor=(0,0,0,0)   reason=stopped
14:59:35 ACTUATE linear=0.000  R2 WRITE motor=(0,0,0,0)   reason=stopped
14:59:35 TICK n=60  write=0     ← hold
14:59:44 TICK n=150 write=0     ← still holding
```

0.05 m/s per 100 ms period is `KIRRA_STOP_DECEL_MPS2=0.5` exactly. Stop, then
hold, with no self-authored re-acceleration.

### Safety significance

`safe_stop` covers the consumer *exiting* — SIGTERM, exception, normal exit. It
does not cover a release drought while the consumer stays up. During the fault
window a publisher crash, a network drop, or a killed process left the platform
driving at its last commanded velocity indefinitely. That is hold-last, which
SS-002 forbids.

---

## What was ruled out, and how

The second fault took the longest because the obvious explanations were wrong.
Each was killed by evidence, not argument:

| Hypothesis | Killed by |
|---|---|
| Timer not firing | `TICK n=30`→`n=130` at 1 Hz across ten seconds of silence |
| journald dropping lines | No `Suppressed` markers, no active `RateLimit`, ~900 lines per 30 s against a default burst of 10000 |
| Retained-reference / GC | The variant that *worked* used bare `create_subscription`; the failing one retained both handles |
| Timer frequency | 1.0 s worked, 0.1 s failed, timer removed entirely also failed |
| Malformed executor / callback groups | Deployed file differed from source only by additive debug prints; one `rclpy.spin`, one `finally:` |
| Stale `.so` (by mtime) | Built after the last commit to either consumer crate — mtime proves *when*, not *from what* |
| Non-positive stop decel | `KIRRA_STOP_DECEL_MPS2=0.5` |
| Architecture-specific core bug | Five starve sequences pass natively on arm64 from the exact commit |

The answer came from comparing what was *configured* against what was
*installed* — a check that did not exist.

---

## Contributing pattern

Four deployed-artifact divergences, each hiding the next:

1. Deployed consumer `.py` hand-patched away from source during debugging
2. Unit `User=root` disagreeing with `env.template`'s robot user → fault 1
3. `KIRRA_ALLOW_SHARED_SERIAL=1` masking the sentinel that reported #2 hourly
4. `KIRRA_CONSUMER_LIB` disagreeing with the installer destination → fault 2

None was a code defect. The repository was correct throughout; the robot was
not, and nothing compared them.

---

## Fixes

**Source**

- `install_kirra.sh` derives the robot user once, honouring `SUDO_USER`
- `verify_deployment.py` derives the cdylib path from one `INSTALLED_CONSUMER_LIB`,
  and reports a loaded-vs-installed mismatch as its own row — loading the wrong
  core is not a load failure, and the `dlopen` check passes either way
- `KIRRA_CONSUMER_RX_DEBUG` gates the release-path trace (RX / ACTUATE / R2 WRITE)
  and a throttled tick heartbeat, so bring-up no longer hand-patches the file
  that drives the wheels

**Tests**

- The installer's robot-user derivations must agree and honour `SUDO_USER`
- The unit template must never hardcode `User=root`
- Installer destination, `EXPECTED_ARTIFACTS`, and `env.template` must name the
  same cdylib path
- Five V2 starve sequences: single release; sustained 40-frame train; that train
  interleaved with ticks; ticks before the first release; real boot epoch at
  epoch scale. The V2 starve path had **no** coverage before this while its V1
  twin has had a ramp test since it shipped
- `robot/diagnostics/` — a controlled ROS callback-dispatch matrix and a
  UID-boundary reproduction, so this class is diagnosed outside production code

**Robot**

- `KIRRA_CONSUMER_LIB` → `/opt/kirra/lib/libkirra_consumer_ffi.so`; stale copy
  moved aside
- `kirra-consumer.service` `User=` → the robot user
- `KIRRA_ALLOW_SHARED_SERIAL` removed; the sentinel now passes unaided

---

## Recommended follow-ups

1. **Refuse to run as uid 0.** Running as root breaks DDS delivery silently
   while every health signal stays green. A startup guard would have turned
   fault 1 into a one-line boot error. Behaviour change — decide deliberately.
2. **Compare deployed `.py` against source in `verify_deployment.py`.** Three of
   the four divergences now have a guard; the hand-patched consumer does not.
3. **Re-render `robot.env` from `env.template` on upgrade**, or report drift.
   Both faults reached production through an env file that was correct when
   written and never revisited.
4. **Retire bring-up escape hatches explicitly.** `KIRRA_ALLOW_SHARED_SERIAL`
   silently converted a fail-closed guard into a log line for an unknown period.
