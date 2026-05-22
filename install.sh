#!/usr/bin/env bash
# install.sh — Aegis Safety Kernel Installer
#
# Installs the Aegis Safety Kernel Verifier Service on Debian/Ubuntu systems.
# Supports x86_64, aarch64 (NVIDIA Jetson, Raspberry Pi), and armv7.
#
# Usage (recommended — pulls latest release from GitHub):
#   curl -fsSL https://raw.githubusercontent.com/justinlooney/singnet/master/install.sh | sudo bash
#
# Usage (from downloaded release archive):
#   sudo bash install.sh
#
# Usage (non-interactive with environment variables pre-set):
#   AEGIS_ADMIN_TOKEN=mytoken AEGIS_PORT=8090 sudo bash install.sh --non-interactive
#
# What this script does:
#   1. Detects system architecture
#   2. Downloads the correct Aegis binary (or uses bundled binary if present)
#   3. Creates a dedicated 'aegis' system user and required directories
#   4. Prompts for configuration (port, token, database location)
#   5. Writes /etc/aegis/aegis.env with your configuration
#   6. Installs and starts the systemd service
#   7. Verifies the service is running correctly
#
# Requirements:
#   - Debian 11+ or Ubuntu 20.04+ (or compatible derivative)
#   - systemd
#   - sudo / root access
#   - Internet access (if downloading binary from GitHub)
#
# Support: https://github.com/justinlooney/singnet/issues

set -euo pipefail

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

AEGIS_USER="aegis"
AEGIS_GROUP="aegis"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/aegis"
DATA_DIR="/var/lib/aegis"
LOG_DIR="/var/log/aegis"
SERVICE_NAME="aegis-verifier"
SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"
ENV_FILE="${CONFIG_DIR}/aegis.env"
BINARY_NAME="aegis_verifier_service"
GITHUB_REPO="justinlooney/singnet"
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"

# Minimum supported OS versions
MIN_UBUNTU="20.04"
MIN_DEBIAN="11"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

info()    { echo -e "${BLUE}[INFO]${NC}  $*"; }
success() { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
fatal()   { error "$*"; exit 1; }
bold()    { echo -e "${BOLD}$*${NC}"; }
section() { echo ""; echo -e "${BOLD}━━━ $* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"; }

# ---------------------------------------------------------------------------
# Banner
# ---------------------------------------------------------------------------

print_banner() {
    echo ""
    echo -e "${BOLD}"
    cat << 'EOF'
     ___          _
    /   \  ___   __ _  (_) ___
   / /\ / / _ \ / _` | | |/ __|
  / /_// |  __/| (_| | | |\__ \
 /___,'   \___| \__, | |_||___/
                |___/
  Safety Kernel Verifier — Installer
EOF
    echo -e "${NC}"
    echo "  Deterministic safety enforcement for autonomous systems"
    echo "  Supports: autonomous vehicles, drones, robots, industrial"
    echo ""
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

NON_INTERACTIVE=false
FORCE_REINSTALL=false
UNINSTALL=false
SKIP_SERVICE_START=false

for arg in "$@"; do
    case $arg in
        --non-interactive) NON_INTERACTIVE=true ;;
        --force)           FORCE_REINSTALL=true ;;
        --uninstall)       UNINSTALL=true ;;
        --no-start)        SKIP_SERVICE_START=true ;;
        --help|-h)
            echo "Usage: sudo bash install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --non-interactive  Skip prompts, use env vars or defaults"
            echo "  --force            Reinstall even if already installed"
            echo "  --uninstall        Remove Aegis and all its files"
            echo "  --no-start         Install but don't start the service"
            echo "  --help             Show this help"
            echo ""
            echo "Environment variables (for --non-interactive):"
            echo "  AEGIS_ADMIN_TOKEN  Admin bearer token (required)"
            echo "  AEGIS_PORT         Listen port (default: 8090)"
            echo "  AEGIS_DB_PATH      Database path (default: /var/lib/aegis/aegis.db)"
            echo "  AEGIS_VERIFIER_MODE active or passive_standby (default: active)"
            exit 0
            ;;
        *)
            warn "Unknown argument: $arg (ignored)"
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------

do_uninstall() {
    section "Uninstalling Aegis"

    if systemctl is-active --quiet "${SERVICE_NAME}" 2>/dev/null; then
        info "Stopping service..."
        systemctl stop "${SERVICE_NAME}"
    fi

    if systemctl is-enabled --quiet "${SERVICE_NAME}" 2>/dev/null; then
        info "Disabling service..."
        systemctl disable "${SERVICE_NAME}"
    fi

    [ -f "${SERVICE_FILE}" ] && rm -f "${SERVICE_FILE}" && info "Removed service file"
    systemctl daemon-reload

    [ -f "${INSTALL_DIR}/${BINARY_NAME}" ] && rm -f "${INSTALL_DIR}/${BINARY_NAME}" && info "Removed binary"

    echo ""
    warn "The following were NOT removed (may contain your data):"
    warn "  ${CONFIG_DIR}  (configuration including admin token)"
    warn "  ${DATA_DIR}    (SQLite database)"
    warn "  ${LOG_DIR}     (log files)"
    echo ""
    echo "To remove everything including data:"
    echo "  sudo rm -rf ${CONFIG_DIR} ${DATA_DIR} ${LOG_DIR}"
    echo "  sudo userdel -r ${AEGIS_USER} 2>/dev/null || true"
    echo ""
    success "Aegis service uninstalled."
    exit 0
}

[ "${UNINSTALL}" = true ] && do_uninstall

# ---------------------------------------------------------------------------
# Root check
# ---------------------------------------------------------------------------

if [ "$(id -u)" -ne 0 ]; then
    fatal "This installer must be run as root. Try: sudo bash install.sh"
fi

# ---------------------------------------------------------------------------
# System checks
# ---------------------------------------------------------------------------

section "Checking System"

# OS check
if [ -f /etc/os-release ]; then
    . /etc/os-release
    OS_ID="${ID:-unknown}"
    OS_VERSION="${VERSION_ID:-0}"
    info "Operating system: ${PRETTY_NAME:-${OS_ID} ${OS_VERSION}}"
else
    warn "Cannot detect OS — proceeding anyway"
    OS_ID="unknown"
fi

# systemd check
if ! command -v systemctl &>/dev/null; then
    fatal "systemd is required but not found. Aegis uses systemd for service management."
fi
success "systemd detected"

# Architecture detection
ARCH=$(uname -m)
case "${ARCH}" in
    x86_64)              BINARY_ARCH="x86_64-linux"   ;;
    aarch64|arm64)       BINARY_ARCH="aarch64-linux"  ;;
    armv7l|armv7)        BINARY_ARCH="armv7-linux"    ;;
    *)
        fatal "Unsupported architecture: ${ARCH}
Aegis supports: x86_64 (Intel/AMD), aarch64 (Jetson/Pi/Graviton), armv7 (embedded ARM)
Please open an issue at https://github.com/${GITHUB_REPO}/issues"
        ;;
esac
success "Architecture: ${ARCH} → using ${BINARY_ARCH} binary"

# Check for existing installation
if [ -f "${INSTALL_DIR}/${BINARY_NAME}" ] && [ "${FORCE_REINSTALL}" = false ]; then
    EXISTING_VERSION=$("${INSTALL_DIR}/${BINARY_NAME}" --version 2>/dev/null || echo "unknown")
    warn "Aegis is already installed (${EXISTING_VERSION})"
    warn "Run with --force to reinstall, or --uninstall to remove"
    echo ""
    echo "Current service status:"
    systemctl status "${SERVICE_NAME}" --no-pager 2>/dev/null || echo "  (service not running)"
    exit 0
fi

# Required tools
for tool in curl sha256sum; do
    if ! command -v "${tool}" &>/dev/null; then
        fatal "${tool} is required but not installed. Run: sudo apt-get install -y curl coreutils"
    fi
done
success "Required tools available"

# ---------------------------------------------------------------------------
# Binary acquisition
# ---------------------------------------------------------------------------

section "Installing Aegis Binary"

# Check if binary is bundled (running from release archive)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLED_BINARY="${SCRIPT_DIR}/aegis/${BINARY_NAME}"

if [ -f "${BUNDLED_BINARY}" ]; then
    info "Using bundled binary"
    BINARY_PATH="${BUNDLED_BINARY}"
else
    info "Downloading from GitHub releases..."

    # Get latest release URL
    RELEASE_JSON=$(curl -fsSL "${GITHUB_API}" 2>/dev/null) || \
        fatal "Cannot reach GitHub API. Check internet connection or use a release archive."

    DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | \
        grep -o '"browser_download_url": "[^"]*'"${BINARY_ARCH}"'\.tar\.gz"' | \
        head -1 | \
        sed 's/"browser_download_url": "//;s/"//')

    CHECKSUM_URL=$(echo "${RELEASE_JSON}" | \
        grep -o '"browser_download_url": "[^"]*SHA256SUMS"' | \
        head -1 | \
        sed 's/"browser_download_url": "//;s/"//')

    VERSION=$(echo "${RELEASE_JSON}" | \
        grep -o '"tag_name": "[^"]*"' | \
        head -1 | \
        sed 's/"tag_name": "//;s/"//')

    if [ -z "${DOWNLOAD_URL}" ]; then
        fatal "No release found for ${BINARY_ARCH}. Check https://github.com/${GITHUB_REPO}/releases"
    fi

    info "Latest version: ${VERSION}"
    info "Downloading ${BINARY_ARCH} binary..."

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "${TMPDIR}"' EXIT

    ARCHIVE="${TMPDIR}/aegis.tar.gz"
    curl -fsSL --progress-bar "${DOWNLOAD_URL}" -o "${ARCHIVE}" || \
        fatal "Download failed. URL: ${DOWNLOAD_URL}"

    # Verify checksum if available
    if [ -n "${CHECKSUM_URL}" ]; then
        info "Verifying checksum..."
        CHECKSUMS="${TMPDIR}/SHA256SUMS"
        curl -fsSL "${CHECKSUM_URL}" -o "${CHECKSUMS}" 2>/dev/null || \
            warn "Checksum file unavailable — skipping verification"

        if [ -f "${CHECKSUMS}" ]; then
            ARCHIVE_NAME=$(basename "${DOWNLOAD_URL}")
            if grep -q "${ARCHIVE_NAME}" "${CHECKSUMS}"; then
                (cd "${TMPDIR}" && \
                    grep "${ARCHIVE_NAME}" SHA256SUMS | \
                    sed "s|${ARCHIVE_NAME}|aegis.tar.gz|" | \
                    sha256sum -c --quiet) || \
                    fatal "Checksum verification FAILED — download may be corrupt or tampered"
                success "Checksum verified"
            fi
        fi
    fi

    # Extract
    info "Extracting..."
    tar -xzf "${ARCHIVE}" -C "${TMPDIR}"
    BINARY_PATH="${TMPDIR}/aegis/${BINARY_NAME}"

    if [ ! -f "${BINARY_PATH}" ]; then
        fatal "Binary not found in archive. Archive contents:"
        tar -tzf "${ARCHIVE}" | head -20
    fi
fi

# Install binary
chmod 755 "${BINARY_PATH}"
cp "${BINARY_PATH}" "${INSTALL_DIR}/${BINARY_NAME}"
success "Binary installed to ${INSTALL_DIR}/${BINARY_NAME}"

# ---------------------------------------------------------------------------
# User and directory setup
# ---------------------------------------------------------------------------

section "Creating System User and Directories"

# Create system user (no login shell, no home directory in /home)
if ! id "${AEGIS_USER}" &>/dev/null; then
    useradd \
        --system \
        --no-create-home \
        --home-dir "${DATA_DIR}" \
        --shell /usr/sbin/nologin \
        --comment "Aegis Safety Kernel Service" \
        "${AEGIS_USER}"
    success "Created system user: ${AEGIS_USER}"
else
    info "System user ${AEGIS_USER} already exists"
fi

# Create directories with correct ownership
for dir in "${CONFIG_DIR}" "${DATA_DIR}" "${LOG_DIR}"; do
    mkdir -p "${dir}"
done

# Config: root-owned, aegis-readable (contains admin token)
chown root:${AEGIS_GROUP} "${CONFIG_DIR}"
chmod 750 "${CONFIG_DIR}"

# Data: aegis-owned (database writes)
chown ${AEGIS_USER}:${AEGIS_GROUP} "${DATA_DIR}"
chmod 750 "${DATA_DIR}"

# Logs: aegis-owned
chown ${AEGIS_USER}:${AEGIS_GROUP} "${LOG_DIR}"
chmod 750 "${LOG_DIR}"

success "Directories created"
info "  Config:   ${CONFIG_DIR}"
info "  Database: ${DATA_DIR}"
info "  Logs:     ${LOG_DIR}"

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

section "Configuration"

echo ""
echo "Aegis requires a few configuration values."
echo "Press Enter to accept the default shown in [brackets]."
echo ""

# --- Admin Token ---
echo -e "${BOLD}Admin Token${NC}"
echo "  The admin token is a secret bearer token required for all"
echo "  administrative operations (registering nodes, viewing posture,"
echo "  submitting federation reports). Keep this secure."
echo "  It must be provided in API calls as: Authorization: Bearer <token>"
echo ""

if [ -n "${AEGIS_ADMIN_TOKEN:-}" ]; then
    ADMIN_TOKEN="${AEGIS_ADMIN_TOKEN}"
    info "Using AEGIS_ADMIN_TOKEN from environment"
elif [ "${NON_INTERACTIVE}" = true ]; then
    fatal "AEGIS_ADMIN_TOKEN must be set in non-interactive mode"
else
    # Check if upgrading and token already exists
    if [ -f "${ENV_FILE}" ] && grep -q "^AEGIS_ADMIN_TOKEN=" "${ENV_FILE}"; then
        EXISTING_TOKEN=$(grep "^AEGIS_ADMIN_TOKEN=" "${ENV_FILE}" | cut -d= -f2-)
        echo -n "  Keep existing token? [Y/n]: "
        read -r KEEP_TOKEN
        if [[ "${KEEP_TOKEN:-Y}" =~ ^[Yy]$ ]]; then
            ADMIN_TOKEN="${EXISTING_TOKEN}"
            info "Keeping existing admin token"
        else
            ADMIN_TOKEN=""
        fi
    fi

    if [ -z "${ADMIN_TOKEN:-}" ]; then
        echo -n "  Enter admin token (leave blank to generate a secure random token): "
        read -r -s ADMIN_TOKEN
        echo ""
    fi
fi

# Generate token if not provided
if [ -z "${ADMIN_TOKEN:-}" ]; then
    ADMIN_TOKEN=$(openssl rand -hex 32 2>/dev/null || \
                  cat /proc/sys/kernel/random/uuid 2>/dev/null | tr -d '-' || \
                  head -c 32 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9' | head -c 32)
    GENERATED_TOKEN=true
else
    GENERATED_TOKEN=false
fi

# --- Port ---
echo ""
echo -e "${BOLD}Listen Port${NC}"
echo "  The port Aegis listens on for HTTP API requests."
echo "  Default 8090 is recommended unless it conflicts with other services."
echo ""

DEFAULT_PORT="8090"
if [ "${NON_INTERACTIVE}" = true ]; then
    PORT="${AEGIS_PORT:-${DEFAULT_PORT}}"
else
    echo -n "  Port [${DEFAULT_PORT}]: "
    read -r PORT
    PORT="${PORT:-${DEFAULT_PORT}}"
fi

# Validate port
if ! [[  "${PORT}" =~ ^[0-9]+$ ]] || [ "${PORT}" -lt 1 ] || [ "${PORT}" -gt 65535 ]; then
    warn "Invalid port '${PORT}', using default ${DEFAULT_PORT}"
    PORT="${DEFAULT_PORT}"
fi
info "Listen address: 0.0.0.0:${PORT}"

# --- Database path ---
echo ""
echo -e "${BOLD}Database Location${NC}"
echo "  Aegis stores its fleet registry, audit chain, and posture history"
echo "  in a SQLite database. This file should be on a persistent volume."
echo "  For production deployments, consider a dedicated disk or volume."
echo ""

DEFAULT_DB="${DATA_DIR}/aegis.db"
if [ "${NON_INTERACTIVE}" = true ]; then
    DB_PATH="${AEGIS_DB_PATH:-${DEFAULT_DB}}"
else
    echo -n "  Database path [${DEFAULT_DB}]: "
    read -r DB_PATH
    DB_PATH="${DB_PATH:-${DEFAULT_DB}}"
fi
info "Database: ${DB_PATH}"

# Ensure database directory exists and is writable by aegis user
DB_DIR=$(dirname "${DB_PATH}")
mkdir -p "${DB_DIR}"
chown ${AEGIS_USER}:${AEGIS_GROUP} "${DB_DIR}"

# --- Verifier mode ---
echo ""
echo -e "${BOLD}Verifier Mode${NC}"
echo "  active          — This is the primary Aegis instance. It enforces"
echo "                    posture and writes to the cache. Use this for"
echo "                    single-instance or primary HA deployments."
echo ""
echo "  passive_standby — This instance observes and audits but does not"
echo "                    enforce. It will automatically promote to active"
echo "                    if the primary fails. Use for HA standby nodes."
echo ""

DEFAULT_MODE="active"
if [ "${NON_INTERACTIVE}" = true ]; then
    VERIFIER_MODE="${AEGIS_VERIFIER_MODE:-${DEFAULT_MODE}}"
else
    echo -n "  Mode [${DEFAULT_MODE}]: "
    read -r VERIFIER_MODE
    VERIFIER_MODE="${VERIFIER_MODE:-${DEFAULT_MODE}}"
fi

case "${VERIFIER_MODE}" in
    active|passive_standby|passive|standby)
        info "Mode: ${VERIFIER_MODE}"
        ;;
    *)
        warn "Unknown mode '${VERIFIER_MODE}', using 'active'"
        VERIFIER_MODE="active"
        ;;
esac

# ---------------------------------------------------------------------------
# Write configuration file
# ---------------------------------------------------------------------------

section "Writing Configuration"

# Backup existing config if present
if [ -f "${ENV_FILE}" ]; then
    cp "${ENV_FILE}" "${ENV_FILE}.bak"
    info "Backed up existing config to ${ENV_FILE}.bak"
fi

cat > "${ENV_FILE}" << EOF
# Aegis Safety Kernel — Environment Configuration
# Generated by installer on $(date -u '+%Y-%m-%dT%H:%M:%SZ')
# Edit this file to change configuration, then run:
#   sudo systemctl restart aegis-verifier

# ── Security ──────────────────────────────────────────────────────────────
# Admin bearer token — required for all administrative API calls.
# Keep this secret. Rotate by editing this file and restarting the service.
# API usage: Authorization: Bearer <value>
AEGIS_ADMIN_TOKEN=${ADMIN_TOKEN}

# ── Network ───────────────────────────────────────────────────────────────
# Address and port to listen on.
# Use 127.0.0.1:${PORT} to restrict to localhost (if behind a reverse proxy).
# Use 0.0.0.0:${PORT} to listen on all interfaces (default).
AEGIS_VERIFIER_ADDR=0.0.0.0:${PORT}

# ── Storage ───────────────────────────────────────────────────────────────
# Path to the SQLite database file.
# This file contains the fleet registry, audit chain, and posture history.
# Back this up regularly in production deployments.
AEGIS_DB_PATH=${DB_PATH}

# ── Operation Mode ────────────────────────────────────────────────────────
# active          = Primary instance. Enforces posture. Writes to cache.
# passive_standby = HA standby. Observes only. Auto-promotes if primary fails.
AEGIS_VERIFIER_MODE=${VERIFIER_MODE}

# ── Identity and Ingress (advanced) ───────────────────────────────────────
# Set to true to require x-aegis-client-id header on identity-gated routes.
# Leave false for standard deployments.
AEGIS_TRUSTED_INGRESS_MODE=false

# Header name used for client identity (when AEGIS_TRUSTED_INGRESS_MODE=true)
AEGIS_CLIENT_ID_HEADER=x-aegis-client-id

# ── High Availability (optional) ──────────────────────────────────────────
# Unique identifier for this Aegis instance (used in HA deployments).
# Leave blank to use hostname automatically.
# AEGIS_INSTANCE_ID=

# Heartbeat interval for primary → standby signaling (milliseconds).
# Default: 2000 (2 seconds)
# AEGIS_HEARTBEAT_INTERVAL=2000

# Promotion timeout — standby promotes if primary silent for this long (ms).
# Default: 10000 (10 seconds)
# AEGIS_PROMOTION_TIMEOUT=10000

# ── Supervisor Reset Key (optional) ───────────────────────────────────────
# Required only if using supervisor reset operations.
# Must be non-empty and ≤ 64 bytes if set.
# AEGIS_SUPERVISOR_RESET_KEY=
EOF

# Secure the config file — contains the admin token
chown root:${AEGIS_GROUP} "${ENV_FILE}"
chmod 640 "${ENV_FILE}"

success "Configuration written to ${ENV_FILE}"

# ---------------------------------------------------------------------------
# systemd service
# ---------------------------------------------------------------------------

section "Installing systemd Service"

# Write service file (use bundled version or generate inline)
BUNDLED_SERVICE="${SCRIPT_DIR}/systemd/aegis-verifier.service"

if [ -f "${BUNDLED_SERVICE}" ]; then
    cp "${BUNDLED_SERVICE}" "${SERVICE_FILE}"
else
    cat > "${SERVICE_FILE}" << EOF
[Unit]
Description=Aegis Safety Kernel Verifier Service
Documentation=https://github.com/${GITHUB_REPO}
After=network.target
Wants=network.target

[Service]
User=${AEGIS_USER}
Group=${AEGIS_GROUP}
ExecStart=${INSTALL_DIR}/${BINARY_NAME}
WorkingDirectory=${DATA_DIR}
EnvironmentFile=${ENV_FILE}
Restart=on-failure
RestartSec=5s
StartLimitIntervalSec=60s
StartLimitBurst=3
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=${DATA_DIR} ${LOG_DIR}
StandardOutput=journal
StandardError=journal
SyslogIdentifier=aegis-verifier
MemoryMax=512M
TasksMax=64

[Install]
WantedBy=multi-user.target
EOF
fi

chmod 644 "${SERVICE_FILE}"
systemctl daemon-reload
systemctl enable "${SERVICE_NAME}"
success "Service installed and enabled (will start on boot)"

# ---------------------------------------------------------------------------
# Start service
# ---------------------------------------------------------------------------

if [ "${SKIP_SERVICE_START}" = false ]; then
    section "Starting Service"

    systemctl start "${SERVICE_NAME}"

    # Wait for service to become healthy
    info "Waiting for Aegis to start..."
    MAX_WAIT=30
    WAITED=0
    while [ ${WAITED} -lt ${MAX_WAIT} ]; do
        if curl -fsSL --max-time 2 \
            "http://127.0.0.1:${PORT}/health" &>/dev/null; then
            break
        fi
        sleep 1
        WAITED=$((WAITED + 1))
        printf "."
    done
    echo ""

    if curl -fsSL --max-time 2 \
        "http://127.0.0.1:${PORT}/health" &>/dev/null; then
        success "Aegis is running and healthy"
    else
        warn "Service started but health check did not respond within ${MAX_WAIT}s"
        warn "Check logs: sudo journalctl -u ${SERVICE_NAME} -n 50"
    fi
fi

# ---------------------------------------------------------------------------
# Post-install summary
# ---------------------------------------------------------------------------

section "Installation Complete"

echo ""
success "Aegis Safety Kernel installed successfully"
echo ""
bold "Service Management:"
echo "  Status:   sudo systemctl status ${SERVICE_NAME}"
echo "  Logs:     sudo journalctl -u ${SERVICE_NAME} -f"
echo "  Restart:  sudo systemctl restart ${SERVICE_NAME}"
echo "  Stop:     sudo systemctl stop ${SERVICE_NAME}"
echo ""
bold "API Endpoint:"
echo "  Health:   http://$(hostname -I | awk '{print $1}'):${PORT}/health"
echo "  Posture:  http://$(hostname -I | awk '{print $1}'):${PORT}/fleet/posture"
echo ""
bold "Configuration:"
echo "  File:     ${ENV_FILE}"
echo "  Database: ${DB_PATH}"
echo "  Mode:     ${VERIFIER_MODE}"
echo ""

if [ "${GENERATED_TOKEN}" = true ]; then
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${YELLOW}  IMPORTANT: Save your admin token — it will not be shown again${NC}"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    echo -e "  Admin Token: ${BOLD}${ADMIN_TOKEN}${NC}"
    echo ""
    echo "  This token is also stored in ${ENV_FILE}"
    echo "  (readable by root and the aegis group only)"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
fi

echo ""
bold "Quick API Test:"
echo "  curl http://localhost:${PORT}/health"
echo "  curl -H 'Authorization: Bearer \${AEGIS_ADMIN_TOKEN}' \\"
echo "       http://localhost:${PORT}/fleet/posture"
echo ""
bold "Documentation:"
echo "  https://github.com/${GITHUB_REPO}/blob/master/INSTALL.md"
echo ""
