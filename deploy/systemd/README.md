# Kirra on boot — systemd units

Bring the on-box Kirra governor stack up automatically on the Orin (or any
systemd host): the **verifier** (governance plane, `:8090`), the **Occy planner**
sidecar (`:8100`), and the **Taj perception** sidecar (`:8101`).

| Unit | What it runs | Port | Secrets |
|---|---|---|---|
| `kirra-verifier.service` | `kirra_verifier_service` (fail-closed governor, `Type=notify` + watchdog) | 8090 | yes (env file) |
| `kirra-planner.service` | `planner_service` (Occy `POST /plan`) | 8100 | no |
| `kirra-taj.service` | `taj_service` (Taj `POST /perception` — the cmd_vel cap) | 8101 | no |
| `kirra.target` | groups all three for one-command lifecycle | — | — |

## Install

```bash
# 1. build the binaries (or run scripts/orin_bringup.sh)
cargo build --release --bin kirra_verifier_service
cargo build --release -p kirra-mick --example planner_service --example taj_service

# 2. install + enable on boot (idempotent)
sudo deploy/systemd/install.sh
```

`install.sh` creates the `kirra` system user, copies the three binaries to
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
systemctl status kirra.target                 # at-a-glance health of all three
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
- The sidecars `Restart=always`. All three are sandboxed (`User=kirra`,
  `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, restricted address families).

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
