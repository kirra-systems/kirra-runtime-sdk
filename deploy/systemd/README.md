# Kirra on boot — systemd units

Bring the on-box Kirra governor stack up automatically on the Orin (or any
systemd host): the **verifier** (governance plane, `:8090`), the **Occy planner**
sidecar (`:8100`), the **Taj perception** sidecar (`:8101`), and the **Mick
typed-intent** sidecar (`:8102`).

| Unit | What it runs | Port | Secrets |
|---|---|---|---|
| `kirra-verifier.service` | `kirra_verifier_service` (fail-closed governor, `Type=notify` + watchdog) | 8090 | yes (env file) |
| `kirra-planner.service` | `planner_service` (Occy `POST /plan`) | 8100 | no |
| `kirra-taj.service` | `taj_service` (Taj `POST /perception` — the cmd_vel cap) | 8101 | no |
| `kirra-mick.service` | `mick_service` (Mick `POST /intent` — typed text → typed intent; never a command) | 8102 | optional (narrator auditor token) |
| `kirra-mick-chat.service` | `mick_chat_service` (Mick `POST /chat` — conversation only, NO motion authority) | 8103 | no |
| `kirra.target` | groups the stack for one-command lifecycle | — | — |

The sidecar binaries ship from the `kirra-sidecars` crate and are FENCED:
`ci/check_mick_actuation_fence.py` fails the build if any of them gains a
dependency route to actuation (release-token mint, serial consumer, ROS/DDS).

### `/chat` vs `/intent` — the two Mick doors

```
/chat   (:8103)  conversation only — plain text out, no motion authority,
                 no intent state; a motion-shaped model reply is replaced
                 with a routing explanation, never passed through
/intent (:8102)  motion-intent classification only — typed, fail-closed,
                 governed downstream (Occy grounds, KIRRA bounds, the
                 verifying consumer enforces)
```

Casual conversation goes to `/chat`; it structurally cannot produce an
intent (`crates/kirra-sidecars/tests/mick_chat_separation.rs` is the fence).
Chat has **no live robot state** — posture/sensors/diagnostics are known only
if the caller pastes them into the request text; the prompt forbids inventing
nominal status. Terminal use:

```bash
python3 robot/mick_chat.py            # interactive: You: … / Mick: …
# or one shot:
curl -sS -X POST http://127.0.0.1:8103/chat \
  -H 'Content-Type: application/json' \
  -d '{"text":"How are you?"}' | python3 -m json.tool
```

## Install

```bash
# 1. build the binaries (or run scripts/orin_bringup.sh)
cargo build --release --bin kirra_verifier_service
cargo build --release -p kirra-sidecars

# 2. install + enable on boot (idempotent)
sudo deploy/systemd/install.sh
```

`install.sh` creates the `kirra` system user, copies the four binaries to
`/opt/kirra/`, **generates `/etc/kirra/kirra.env` with strong random secrets** if it
doesn't exist (it never overwrites an existing one, and no secret is ever committed),
installs the units + a `PartOf=` drop-in so the target owns the verifier too, and
`enable --now`s `kirra.target`.

Read the generated admin token (for ROS clients / the console):

```bash
sudo sed -n 's/^KIRRA_ADMIN_TOKEN=//p' /etc/kirra/kirra.env
```

## Operate

```bash
systemctl status kirra.target                 # at-a-glance health of the stack
systemctl restart kirra.target                # bounce the whole stack
systemctl stop kirra.target                    # take it down (leaves it enabled on boot)
journalctl -u kirra-verifier -f                # follow the verifier log
```

## How it stays up (fail-closed)

- The verifier is `Type=notify` with a 10 s **watchdog**: a hung-but-alive process
  (e.g. a stalled posture engine → stale cache) misses the ping and systemd restarts
  it — the same direction the in-process gate already fails.
- The verifier **exits at startup** if `KIRRA_ADMIN_TOKEN` is absent/empty (SG-008).
  `StartLimitIntervalSec=0` (in `[Unit]`) disables the start-rate limiter, so it keeps
  retrying and **self-heals** the moment `/etc/kirra/kirra.env` is populated — it never
  lands in a permanent `failed` state because a secret was briefly missing.
- The sidecars `Restart=always`. All are sandboxed (`User=kirra`,
  `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, restricted address families),
  bind loopback by default, and refuse a routable bind unless
  `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` is set (fail-closed startup abort).
- `mick_service` fails closed end-to-end: no Ollama / an unparseable model reply →
  422 and NO latched intent — the doer grounds nothing and nothing moves. Its
  optional narrator (`GET /narration/last`) relays the verifier's #893
  `GET /system/verdicts/last` using `KIRRA_VERIFIER_URL` + `KIRRA_MICK_AUDITOR_TOKEN`
  (an `auditor`-role principal token — NEVER the admin token; half-configuring the
  pair aborts startup).

## Secrets

`/etc/kirra/kirra.env` (mode 600, owned by `kirra`) is the **only** source of
`KIRRA_ADMIN_TOKEN` and `KIRRA_SUPERVISOR_RESET_KEY` — no hardcoded fallback
(INVARIANTS #6/#7). It is generated at install time and never committed; the
committed `kirra.env.example` is a placeholder template. To rotate, delete the file
and re-run `install.sh` (or edit it and `systemctl restart kirra.target`).

## Robot / ROS clients

The verifier binds `127.0.0.1:8090`, so the ROS `cmd_vel` interceptor on the same box
reaches it at `http://localhost:8090`. Pass the token to the launch:

```bash
export KIRRA_ADMIN_TOKEN=$(sudo sed -n 's/^KIRRA_ADMIN_TOKEN=//p' /etc/kirra/kirra.env)
ros2 launch kirra_safety kirra_with_robot.launch.py \
    kirra_token:=$KIRRA_ADMIN_TOKEN use_perception_cap:=true start_sidecars:=false
```

Use `start_sidecars:=false` (and the Gazebo demo's `start_verifier:=false`) when the
stack is already running under systemd — otherwise the launch's own sidecars would
double-bind the ports.

## Uninstall

```bash
sudo deploy/systemd/uninstall.sh            # remove units + /opt/kirra (keeps secrets + DB)
sudo deploy/systemd/uninstall.sh --purge    # also remove /etc/kirra and /var/lib/kirra
```
