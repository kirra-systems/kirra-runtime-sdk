# Kirra Safety Kernel — Installation Guide

Kirra is a deterministic safety enforcement kernel for autonomous systems.
It intercepts motion commands from AI planners and enforces hard physical
limits before they reach actuators — preventing unsafe commands from reaching
robots, vehicles, drones, or industrial equipment regardless of what the AI
instructs.

---

## Requirements

| Requirement | Minimum |
|-------------|---------|
| Operating System | Ubuntu 20.04 LTS / Debian 11 or newer |
| Architecture | x86_64, aarch64 (Jetson/Pi), or armv7 |
| RAM | 256 MB |
| Disk | 1 GB (for database and logs) |
| Network | HTTP port (default 8090) accessible from managed devices |
| Privileges | sudo / root for installation |
| Init system | systemd |

> **NVIDIA Jetson users:** Use the `aarch64` binary. Tested on Jetson Orin and
> Jetson Xavier. The installer detects your architecture automatically.

> **Raspberry Pi users:** Pi 4 and Pi 5 use the `aarch64` binary. Pi 3 and
> earlier use `armv7`. The installer detects this automatically.

---

## Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/kirra-systems/kirra-runtime-sdk/main/install.sh | sudo bash
```

The installer will:
1. Detect your system architecture
2. Download the correct binary
3. Ask three questions (port, admin token, database location)
4. Create a dedicated system user
5. Install and start the service
6. Show you the admin token and a quick-test command

**The whole process takes under 2 minutes.**

---

## What the Installer Asks

### Admin Token
The admin token is a secret password required for all management operations —
registering devices, viewing fleet posture, submitting safety reports. Think of
it like a root password for the Kirra API.

- **Leave blank** to have the installer generate a secure random token
- **Enter your own** if you want a specific value (minimum 16 characters recommended)
- The token is stored in `/etc/kirra/kirra.env`, readable only by root and the
  `kirra` system user
- **Write it down or store it in a password manager** — you'll need it for API calls

### Port
The TCP port Kirra listens on. Default is **8090**.

Change this if:
- Another service already uses 8090
- Your firewall requires a specific port
- You're running multiple Kirra instances on the same machine

### Database Location
Where Kirra stores its SQLite database. Default is `/var/lib/kirra/kirra.db`.

Change this if:
- You want the database on a separate volume
- You're mounting persistent storage at a different path
- Your organization has a standard location for application data

---

## After Installation

### Check the Service is Running

```bash
sudo systemctl status kirra-verifier
```

You should see `active (running)`. If not, check the logs:

```bash
sudo journalctl -u kirra-verifier -n 50
```

### Test the API

```bash
# Health check (no token required)
curl http://localhost:8090/health

# Fleet posture (token required)
curl -H "Authorization: Bearer YOUR_TOKEN" \
     http://localhost:8090/fleet/posture
```

Replace `YOUR_TOKEN` with the token shown at the end of installation,
or find it in `/etc/kirra/kirra.env`.

### View Logs

```bash
# Live log stream
sudo journalctl -u kirra-verifier -f

# Last 100 lines
sudo journalctl -u kirra-verifier -n 100

# Logs from the last hour
sudo journalctl -u kirra-verifier --since "1 hour ago"
```

---

## Service Management

| Action | Command |
|--------|---------|
| Check status | `sudo systemctl status kirra-verifier` |
| Start | `sudo systemctl start kirra-verifier` |
| Stop | `sudo systemctl stop kirra-verifier` |
| Restart | `sudo systemctl restart kirra-verifier` |
| View logs | `sudo journalctl -u kirra-verifier -f` |
| Disable autostart | `sudo systemctl disable kirra-verifier` |

---

## Configuration

Configuration lives in `/etc/kirra/kirra.env`. Edit this file to change
any setting, then restart the service.

```bash
sudo nano /etc/kirra/kirra.env
sudo systemctl restart kirra-verifier
```

### Configuration Reference

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `KIRRA_ADMIN_TOKEN` | **Yes** | — | Bearer token for admin API calls |
| `KIRRA_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address and port |
| `KIRRA_DB_PATH` | No | `/var/lib/kirra/kirra.db` | SQLite database path |
| `KIRRA_VERIFIER_MODE` | No | `active` | `active` or `passive_standby` |
| `KIRRA_TRUSTED_INGRESS_MODE` | No | `false` | Require client ID headers |
| `KIRRA_INSTANCE_ID` | No | hostname | Unique ID for HA deployments |
| `KIRRA_HEARTBEAT_INTERVAL` | No | `2000` | HA heartbeat interval (ms) |
| `KIRRA_PROMOTION_TIMEOUT` | No | `10000` | HA promotion timeout (ms) |
| `KIRRA_SUPERVISOR_RESET_KEY` | No | — | Supervisor reset operations |

### Rotating the Admin Token

```bash
# Generate a new token
NEW_TOKEN=$(openssl rand -hex 32)

# Update the config file
sudo sed -i "s/^KIRRA_ADMIN_TOKEN=.*/KIRRA_ADMIN_TOKEN=${NEW_TOKEN}/" \
    /etc/kirra/kirra.env

# Restart to apply
sudo systemctl restart kirra-verifier

echo "New token: ${NEW_TOKEN}"
```

Update all clients and integrations with the new token before restarting.

---

## Upgrading

```bash
curl -fsSL https://raw.githubusercontent.com/kirra-systems/kirra-runtime-sdk/main/install.sh \
    | sudo bash -s -- --force
```

The `--force` flag reinstalls over the existing installation. Your configuration
(`/etc/kirra/kirra.env`) and database (`/var/lib/kirra/kirra.db`) are preserved.

---

## Uninstalling

```bash
curl -fsSL https://raw.githubusercontent.com/kirra-systems/kirra-runtime-sdk/main/install.sh \
    | sudo bash -s -- --uninstall
```

This removes the binary and service but **preserves** your configuration and
database. To remove everything:

```bash
sudo rm -rf /etc/kirra /var/lib/kirra /var/log/kirra
sudo userdel -r kirra 2>/dev/null || true
```

---

## High Availability (Two-Instance Setup)

For production deployments where Kirra must remain running even if one
machine fails, deploy two instances: one `active` (primary) and one
`passive_standby` (standby). Both must share the same SQLite database
(via NFS, shared block storage, or database replication).

**Primary instance** (`/etc/kirra/kirra.env`):
```
KIRRA_VERIFIER_MODE=active
KIRRA_INSTANCE_ID=kirra-primary
```

**Standby instance** (`/etc/kirra/kirra.env`):
```
KIRRA_VERIFIER_MODE=passive_standby
KIRRA_INSTANCE_ID=kirra-standby
KIRRA_DB_PATH=/mnt/shared/kirra.db
```

The standby monitors the primary's heartbeat. If the primary is silent for
10 seconds (configurable via `KIRRA_PROMOTION_TIMEOUT`), the standby
automatically promotes itself to active and begins enforcing posture.

---

## Firewall Configuration

If you have a firewall (ufw, iptables, firewalld), allow the Kirra port:

```bash
# ufw
sudo ufw allow 8090/tcp comment "Kirra Safety Kernel"

# firewalld
sudo firewall-cmd --permanent --add-port=8090/tcp
sudo firewall-cmd --reload
```

Restrict access to trusted networks in production:
```bash
# Only allow from your device management network (example: 10.0.1.0/24)
sudo ufw allow from 10.0.1.0/24 to any port 8090
```

---

## Non-Interactive Installation (CI/CD and Automation)

Set environment variables before running the installer to skip all prompts:

```bash
export KIRRA_ADMIN_TOKEN="your-secret-token-here"
export KIRRA_PORT="8090"
export KIRRA_DB_PATH="/var/lib/kirra/kirra.db"
export KIRRA_VERIFIER_MODE="active"

curl -fsSL https://raw.githubusercontent.com/kirra-systems/kirra-runtime-sdk/main/install.sh \
    | sudo -E bash -s -- --non-interactive
```

The `-E` flag passes your environment variables through sudo.

---

## Troubleshooting

### Service Won't Start

```bash
sudo journalctl -u kirra-verifier -n 100
```

Common causes:
- **Port already in use**: Change `KIRRA_VERIFIER_ADDR` in `/etc/kirra/kirra.env`
- **Database directory not writable**: `sudo chown kirra:kirra /var/lib/kirra`
- **Missing admin token**: Ensure `KIRRA_ADMIN_TOKEN` is set in `/etc/kirra/kirra.env`

### API Returns 401 Unauthorized

Your admin token is wrong or missing. Check:
```bash
# View the configured token (requires root)
sudo grep KIRRA_ADMIN_TOKEN /etc/kirra/kirra.env
```

Use it in requests:
```bash
curl -H "Authorization: Bearer $(sudo grep KIRRA_ADMIN_TOKEN /etc/kirra/kirra.env | cut -d= -f2)" \
     http://localhost:8090/fleet/posture
```

### API Returns 503 Service Unavailable

The admin token environment variable is empty or missing. Restart the service
after ensuring `KIRRA_ADMIN_TOKEN` is set in `/etc/kirra/kirra.env`.

### Database Errors

```bash
# Check disk space
df -h /var/lib/kirra

# Check file ownership
ls -la /var/lib/kirra/

# Fix ownership if wrong
sudo chown -R kirra:kirra /var/lib/kirra
```

### Architecture Mismatch

If you see `Exec format error`, the binary architecture doesn't match your
system. The installer detects architecture automatically — if it downloaded
the wrong binary, open an issue at https://github.com/kirra-systems/kirra-runtime-sdk/issues
with the output of `uname -m`.

---

## Security Notes

- The admin token is equivalent to root access to the Kirra API. Treat it
  like a password.
- `/etc/kirra/kirra.env` is readable by root and the `kirra` group only
  (mode 640). Do not change this permission.
- The `kirra` system user has no login shell and no home directory outside
  `/var/lib/kirra`. It cannot be used to log in.
- All administrative API calls are logged to the tamper-evident audit chain
  stored in the database.
- For production deployments, place Kirra behind a TLS-terminating reverse
  proxy (nginx, Caddy) and restrict direct port access.

---

## Multi-Backend / Multi-Chipset Install (full stack: Kirra + Occy + Parko)

The install is **run-and-go ready**: the install PATH is authored for **every**
target now, so the only thing that can be missing at install time is **external**
— the hardware and (for vendor targets) the operator-supplied licensed SDK
artifact. Never "we still have to build the install."

`install.sh` installs the silicon-agnostic **gateway** (`kirra_verifier_service`)
— unchanged. The companion **target-aware layer** composes the full stack —
**Kirra** (governor/gateway) + **Occy** (trajectory planner, silicon-agnostic
ROS2/Autoware) + **Parko** (the per-silicon inference backend) — and validates
it fail-closed:

```bash
sudo bash scripts/install-parko-backend.sh --target <TARGET> [--sdk-path <ARTIFACT>]
# explore without installing anything (no hardware/root/network):
bash scripts/install-parko-backend.sh --readiness     # the readiness model
bash scripts/install-parko-backend.sh --list
bash scripts/install-parko-backend.sh --target ort-cpu --dry-run
```

The per-silicon variation lives **entirely in Parko** (the backend matrix);
Kirra and Occy are authored once and composed with the selected Parko backend.
Per-target flow: `[Kirra] + [Occy]` (silicon-agnostic) `+` select Parko target →
acquire runtime/SDK → build Parko (right feature) → apply posture →
**fail-closed validate the backend loads** → common safety gates across the
composed stack.

### Two readiness dimensions (don't conflate)

1. **Install-path readiness** — the procedure per target. **READY NOW for all six
   targets** (this script).
2. **Backend-code readiness** — `done` (ort-cpu, openvino), `scaffold` (tensorrt,
   inference Jetson-gated), `stub` (qnn/ti-tidl/amd-vitis — PARK-027/028/030, a
   separate code effort).

"Ready when hardware + license arrive" = install-path ready (now, all) + backend
code ready (done for some) + the external hardware/SDK. **A vendor target's PATH
is ready; its backend is the remaining code gate — different things.** Run
`--readiness` for the live table.

| Target | Install-path | Backend-code | Remaining external gate |
|--------|--------------|--------------|--------------------------|
| `ort-cpu` | **READY** | done | none — ready now (CPU, anywhere) |
| `openvino` | **READY** | done | Intel silicon (dev box ok) — ready now there |
| `tensorrt` | **READY** | scaffold | NVIDIA Jetson hardware (no license) |
| `qnn` | **READY** | stub (PARK-027) | Qualcomm HW + QNN SDK + backend code |
| `ti-tidl` | **READY** | stub (PARK-028) | TI HW + TIDL SDK + backend code |
| `amd-vitis` | **READY** | stub (PARK-030) | AMD HW + Vitis AI + backend code |

### Authored per-target install path

Every target has a real, ready-to-run procedure — not a "requires-SDK" refusal:

- **ort-cpu / openvino** — runtime is freely pullable; path runs and validates
  now (CPU anywhere; Intel on the dev box).
- **tensorrt** — NVIDIA's TRT-enabled ORT from JetPack/L4T on the Jetson; path +
  fail-closed load path present, on-device run Jetson-gated.
- **qnn / ti-tidl / amd-vitis** — the operator **supplies the licensed SDK
  artifact** via `--sdk-path <ARTIFACT>` (never auto-fetched — vendor-gated). The
  path runs acquire → build → posture end to end; its **FINAL backend-load
  validation defers** until the backend code is implemented (PARK-027/028/030).
  That boundary is marked explicitly: missing `--sdk-path` says "supply the
  artifact" (external gate), a stub backend says "remaining CODE gate" — neither
  is a missing install.

### Design decisions (recommendations)

- **Selection — explicit, not auto.** `--target` explicitly (recommended).
  `--auto-detect` only *suggests* and needs `--confirm` — never auto-proceeds
  (it can mask misconfig / pick wrong silicon — unsafe for a safety runtime).
- **Fail-closed per backend.** If the selected backend's runtime/EP isn't
  present, the install **refuses** — never silently substitutes another backend
  (no quiet CPU fallback on a GPU target). Generalizes `parko-tensorrt`'s
  `.error_on_failure()` to every target (wire the probe via `PARKO_BACKEND_PROBE`).
- **Full-stack composition.** Kirra + Occy are silicon-agnostic and authored
  once; the per-target axis is Parko only. Default composes all three;
  `--parko-only` / `--no-occy` scope it down.
- **Operator-supplied SDK.** Vendor licensed artifacts are passed as
  `--sdk-path <ARTIFACT>` (a path the operator provides), never an impossible
  auto-fetch.
- **Container vs host.** Target-parameterized installer driving both, with
  per-target **base images** (CPU / Intel / L4T-Jetson / vendor) for the
  reproducible/pilot path. The gateway `Dockerfile`/`docker-compose.yml` are
  **not forked**. The ort-cpu (CPU) per-target image is
  `deploy/docker/Dockerfile.parko-ort-cpu` (ROS 2 Jazzy + ONNX Runtime 1.23.2).

### Common safety gates (chipset-independent, NON-skippable)

Run for **every** target across the composed stack as refuse-to-proceed steps
(there is no `--skip-safety-gates`): fail-closed backend-load validation, the
**chokepoint check** (exactly one publisher on the motor topic — the Kirra
gateway is the sole writer), envelope/posture config presence, **e-stop**
verification, and a **wheels-up-first smoke** (an over-limit Occy plan is
clamped/denied by Kirra with the vehicle on stands).

### Posture config per target

| Target | Posture |
|--------|---------|
| `ort-cpu` | single-thread + `GraphOptimizationLevel::Disable` (bitwise-reproducible) |
| `openvino` | `ACCURACY` + `INFERENCE_PRECISION_HINT=f32` + `LATENCY` (mirrors ORT-CPU) |
| `tensorrt` | `fp16=false`, `int8=false`, engine-cache on; **TF32 unenforced** (Jetson-gated); not bitwise-reproducible → decision-agreement posture |
| `qnn` / `ti-tidl` / `amd-vitis` | full precision; vendor posture defined with the backend (PARK-027/028/030) |

### Testable now vs externally-gated

- **Testable now (no special hardware):** the framework —
  `scripts/test-install-parko-backend.sh` (pure shell, CI job
  `parko-install-framework`, 16 assertions) covers dispatch, full-stack
  composition, the readiness model, the operator-SDK path, fail-closed refusals,
  and non-skippable gates. Plus the full **Kirra + Occy + Parko(CPU)** and
  **Parko(Intel)** stack end to end.
- **Externally-gated:** on-silicon validation per target — **Jetson/TensorRT** on
  hardware now (path + fail-closed present; on-device run gated, see the
  `parko-tensorrt` PARK-021 list); **Qualcomm/TI/AMD** when backend code +
  hardware + SDK align.

> Note: `tensorrt` references the `parko-tensorrt` crate (PARK-021 scaffold) and
> the vendor rows reference `parko-qnn`/`parko-tidl`/`parko-vitis`
> (PARK-027/028/030) — those land via their own branches. The installer
> dispatches by **target name** and is independent of those crates being merged.

---

## Getting Help

- **Documentation**: https://github.com/kirra-systems/kirra-runtime-sdk
- **Issues**: https://github.com/kirra-systems/kirra-runtime-sdk/issues
- **Logs**: `sudo journalctl -u kirra-verifier -f`
