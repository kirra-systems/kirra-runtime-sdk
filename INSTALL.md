# Aegis Safety Kernel — Installation Guide

Aegis is a deterministic safety enforcement kernel for autonomous systems.
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
curl -fsSL https://raw.githubusercontent.com/justinlooney/singnet/master/install.sh | sudo bash
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
it like a root password for the Aegis API.

- **Leave blank** to have the installer generate a secure random token
- **Enter your own** if you want a specific value (minimum 16 characters recommended)
- The token is stored in `/etc/aegis/aegis.env`, readable only by root and the
  `aegis` system user
- **Write it down or store it in a password manager** — you'll need it for API calls

### Port
The TCP port Aegis listens on. Default is **8090**.

Change this if:
- Another service already uses 8090
- Your firewall requires a specific port
- You're running multiple Aegis instances on the same machine

### Database Location
Where Aegis stores its SQLite database. Default is `/var/lib/aegis/aegis.db`.

Change this if:
- You want the database on a separate volume
- You're mounting persistent storage at a different path
- Your organization has a standard location for application data

---

## After Installation

### Check the Service is Running

```bash
sudo systemctl status aegis-verifier
```

You should see `active (running)`. If not, check the logs:

```bash
sudo journalctl -u aegis-verifier -n 50
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
or find it in `/etc/aegis/aegis.env`.

### View Logs

```bash
# Live log stream
sudo journalctl -u aegis-verifier -f

# Last 100 lines
sudo journalctl -u aegis-verifier -n 100

# Logs from the last hour
sudo journalctl -u aegis-verifier --since "1 hour ago"
```

---

## Service Management

| Action | Command |
|--------|---------|
| Check status | `sudo systemctl status aegis-verifier` |
| Start | `sudo systemctl start aegis-verifier` |
| Stop | `sudo systemctl stop aegis-verifier` |
| Restart | `sudo systemctl restart aegis-verifier` |
| View logs | `sudo journalctl -u aegis-verifier -f` |
| Disable autostart | `sudo systemctl disable aegis-verifier` |

---

## Configuration

Configuration lives in `/etc/aegis/aegis.env`. Edit this file to change
any setting, then restart the service.

```bash
sudo nano /etc/aegis/aegis.env
sudo systemctl restart aegis-verifier
```

### Configuration Reference

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `AEGIS_ADMIN_TOKEN` | **Yes** | — | Bearer token for admin API calls |
| `AEGIS_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address and port |
| `AEGIS_DB_PATH` | No | `/var/lib/aegis/aegis.db` | SQLite database path |
| `AEGIS_VERIFIER_MODE` | No | `active` | `active` or `passive_standby` |
| `AEGIS_TRUSTED_INGRESS_MODE` | No | `false` | Require client ID headers |
| `AEGIS_INSTANCE_ID` | No | hostname | Unique ID for HA deployments |
| `AEGIS_HEARTBEAT_INTERVAL` | No | `2000` | HA heartbeat interval (ms) |
| `AEGIS_PROMOTION_TIMEOUT` | No | `10000` | HA promotion timeout (ms) |
| `AEGIS_SUPERVISOR_RESET_KEY` | No | — | Supervisor reset operations |

### Rotating the Admin Token

```bash
# Generate a new token
NEW_TOKEN=$(openssl rand -hex 32)

# Update the config file
sudo sed -i "s/^AEGIS_ADMIN_TOKEN=.*/AEGIS_ADMIN_TOKEN=${NEW_TOKEN}/" \
    /etc/aegis/aegis.env

# Restart to apply
sudo systemctl restart aegis-verifier

echo "New token: ${NEW_TOKEN}"
```

Update all clients and integrations with the new token before restarting.

---

## Upgrading

```bash
curl -fsSL https://raw.githubusercontent.com/justinlooney/singnet/master/install.sh \
    | sudo bash -s -- --force
```

The `--force` flag reinstalls over the existing installation. Your configuration
(`/etc/aegis/aegis.env`) and database (`/var/lib/aegis/aegis.db`) are preserved.

---

## Uninstalling

```bash
curl -fsSL https://raw.githubusercontent.com/justinlooney/singnet/master/install.sh \
    | sudo bash -s -- --uninstall
```

This removes the binary and service but **preserves** your configuration and
database. To remove everything:

```bash
sudo rm -rf /etc/aegis /var/lib/aegis /var/log/aegis
sudo userdel -r aegis 2>/dev/null || true
```

---

## High Availability (Two-Instance Setup)

For production deployments where Aegis must remain running even if one
machine fails, deploy two instances: one `active` (primary) and one
`passive_standby` (standby). Both must share the same SQLite database
(via NFS, shared block storage, or database replication).

**Primary instance** (`/etc/aegis/aegis.env`):
```
AEGIS_VERIFIER_MODE=active
AEGIS_INSTANCE_ID=aegis-primary
```

**Standby instance** (`/etc/aegis/aegis.env`):
```
AEGIS_VERIFIER_MODE=passive_standby
AEGIS_INSTANCE_ID=aegis-standby
AEGIS_DB_PATH=/mnt/shared/aegis.db
```

The standby monitors the primary's heartbeat. If the primary is silent for
10 seconds (configurable via `AEGIS_PROMOTION_TIMEOUT`), the standby
automatically promotes itself to active and begins enforcing posture.

---

## Firewall Configuration

If you have a firewall (ufw, iptables, firewalld), allow the Aegis port:

```bash
# ufw
sudo ufw allow 8090/tcp comment "Aegis Safety Kernel"

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
export AEGIS_ADMIN_TOKEN="your-secret-token-here"
export AEGIS_PORT="8090"
export AEGIS_DB_PATH="/var/lib/aegis/aegis.db"
export AEGIS_VERIFIER_MODE="active"

curl -fsSL https://raw.githubusercontent.com/justinlooney/singnet/master/install.sh \
    | sudo -E bash -s -- --non-interactive
```

The `-E` flag passes your environment variables through sudo.

---

## Troubleshooting

### Service Won't Start

```bash
sudo journalctl -u aegis-verifier -n 100
```

Common causes:
- **Port already in use**: Change `AEGIS_VERIFIER_ADDR` in `/etc/aegis/aegis.env`
- **Database directory not writable**: `sudo chown aegis:aegis /var/lib/aegis`
- **Missing admin token**: Ensure `AEGIS_ADMIN_TOKEN` is set in `/etc/aegis/aegis.env`

### API Returns 401 Unauthorized

Your admin token is wrong or missing. Check:
```bash
# View the configured token (requires root)
sudo grep AEGIS_ADMIN_TOKEN /etc/aegis/aegis.env
```

Use it in requests:
```bash
curl -H "Authorization: Bearer $(sudo grep AEGIS_ADMIN_TOKEN /etc/aegis/aegis.env | cut -d= -f2)" \
     http://localhost:8090/fleet/posture
```

### API Returns 503 Service Unavailable

The admin token environment variable is empty or missing. Restart the service
after ensuring `AEGIS_ADMIN_TOKEN` is set in `/etc/aegis/aegis.env`.

### Database Errors

```bash
# Check disk space
df -h /var/lib/aegis

# Check file ownership
ls -la /var/lib/aegis/

# Fix ownership if wrong
sudo chown -R aegis:aegis /var/lib/aegis
```

### Architecture Mismatch

If you see `Exec format error`, the binary architecture doesn't match your
system. The installer detects architecture automatically — if it downloaded
the wrong binary, open an issue at https://github.com/justinlooney/singnet/issues
with the output of `uname -m`.

---

## Security Notes

- The admin token is equivalent to root access to the Aegis API. Treat it
  like a password.
- `/etc/aegis/aegis.env` is readable by root and the `aegis` group only
  (mode 640). Do not change this permission.
- The `aegis` system user has no login shell and no home directory outside
  `/var/lib/aegis`. It cannot be used to log in.
- All administrative API calls are logged to the tamper-evident audit chain
  stored in the database.
- For production deployments, place Aegis behind a TLS-terminating reverse
  proxy (nginx, Caddy) and restrict direct port access.

---

## Getting Help

- **Documentation**: https://github.com/justinlooney/singnet
- **Issues**: https://github.com/justinlooney/singnet/issues
- **Logs**: `sudo journalctl -u aegis-verifier -f`
