#!/usr/bin/env bash
# Setup produzione su server-prod — eseguire UNA SOLA VOLTA come root (o con sudo).
# Installa toolchain, crea struttura directory, nginx, systemd units.
# Idempotente: può essere rieseguito senza danni.
#
# Uso: scp deploy/prod-setup-server-prod.sh administrator@:/tmp/
#      ssh administrator@ "sudo bash /tmp/prod-setup-server-prod.sh"

set -euo pipefail

APP_USER="administrator"
APP_PATH="/opt/ideai"
LOG_PATH="/var/log/ideai"

echo "══════════════════════════════════════════════════"
echo "  IDEAI — Setup produzione su server-prod"
echo "══════════════════════════════════════════════════"

# ── 1. Struttura directory ────────────────────────────────────────────────────
echo "[1/8] Struttura directory..."
mkdir -p "${APP_PATH}"/{bin,logs,config,deploy}
mkdir -p "${LOG_PATH}"
chown -R "${APP_USER}:${APP_USER}" "${APP_PATH}" "${LOG_PATH}"
echo "  ✓ ${APP_PATH} e ${LOG_PATH} pronti"

# ── 2. Dipendenze sistema ─────────────────────────────────────────────────────
echo "[2/8] Dipendenze sistema..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq \
    build-essential pkg-config libssl-dev libpq-dev \
    curl wget git unzip jq ca-certificates \
    nginx protobuf-compiler python3 python3-pip python3-venv
echo "  ✓ Pacchetti installati"

# ── 3. Rust ───────────────────────────────────────────────────────────────────
echo "[3/8] Rust toolchain..."
if ! sudo -u "${APP_USER}" bash -lc "command -v cargo" &>/dev/null; then
    sudo -u "${APP_USER}" bash -c \
        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --quiet"
    echo "  ✓ Rust installato"
else
    sudo -u "${APP_USER}" bash -lc "rustup update stable --quiet"
    echo "  ✓ Rust aggiornato: $(sudo -u ${APP_USER} bash -lc 'rustc --version')"
fi

# ── 4. Node.js + pnpm ─────────────────────────────────────────────────────────
echo "[4/8] Node.js + pnpm..."
if ! command -v node &>/dev/null; then
    curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
    apt-get install -y -qq nodejs
fi
if ! command -v pnpm &>/dev/null; then
    npm install -g pnpm --silent
fi
echo "  ✓ Node $(node --version), pnpm $(pnpm --version)"

# ── 5. Python venv per Neural Core ────────────────────────────────────────────
echo "[5/8] Python venv per Neural Core..."
VENV="${APP_PATH}/.venv"
if [ ! -d "$VENV" ]; then
    sudo -u "${APP_USER}" python3 -m venv "$VENV"
fi
if [ -f "${APP_PATH}/brain/pyproject.toml" ]; then
    sudo -u "${APP_USER}" bash -c "${VENV}/bin/pip install -q --upgrade pip && \
        ${VENV}/bin/pip install -q -e '${APP_PATH}/brain[all]'"
    echo "  ✓ Dipendenze Python installate"
else
    echo "  ⚠ brain/pyproject.toml non trovato — installa le dep manualmente dopo il deploy"
fi

# ── 6. Systemd units ──────────────────────────────────────────────────────────
echo "[6/8] Systemd units..."

install_unit() {
    local name="$1"
    local src="${APP_PATH}/deploy/systemd/${name}.service"
    local dst="/etc/systemd/system/${name}.service"
    if [ -f "$src" ]; then
        cp "$src" "$dst"
        echo "  ✓ ${name}.service"
    else
        echo "  ⚠ ${name}.service non trovato in ${src}"
    fi
}

# Copia units dalla directory deploy (sincronizzate con il codice)
if [ -d "${APP_PATH}/deploy/systemd" ]; then
    for unit in nexus-neural nexus-core nexus-admin nexus-chat nexus-billing nexus-docs nexus-plugins nexus-webide; do
        install_unit "$unit"
    done
    systemctl daemon-reload
    for unit in nexus-neural nexus-core nexus-admin nexus-chat nexus-billing nexus-docs nexus-plugins nexus-webide; do
        systemctl enable "$unit" 2>/dev/null || true
    done
    echo "  ✓ Tutte le units installate e abilitate"
else
    echo "  ⚠ deploy/systemd non trovato — esegui prima deploy-server-prod.sh"
fi

# ── 7. Nginx ──────────────────────────────────────────────────────────────────
echo "[7/8] Nginx..."
NGINX_CONF="/etc/nginx/sites-available/ideai"
cp "${APP_PATH}/deploy/nginx-microservices.conf" "$NGINX_CONF" 2>/dev/null || \
    cp "${APP_PATH}/deploy/nginx-prod.conf"       "$NGINX_CONF" 2>/dev/null || true

if [ -f "$NGINX_CONF" ]; then
    ln -sf "$NGINX_CONF" /etc/nginx/sites-enabled/ideai
    rm -f /etc/nginx/sites-enabled/default
    nginx -t && systemctl enable nginx --now && systemctl reload nginx
    echo "  ✓ Nginx configurato e ricaricato"
else
    echo "  ⚠ Config nginx non trovata — copia manualmente in /etc/nginx/sites-available/ideai"
fi

# ── 8. Logrotate ──────────────────────────────────────────────────────────────
echo "[8/8] Logrotate..."
cat > /etc/logrotate.d/ideai <<'LOGROTATE'
/var/log/ideai/*.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    create 0644 administrator administrator
    sharedscripts
    postrotate
        systemctl reload nexus-core nexus-admin nexus-chat 2>/dev/null || true
    endscript
}
LOGROTATE
echo "  ✓ Logrotate configurato"

echo ""
echo "══════════════════════════════════════════════════"
echo "  Setup completato."
echo ""
echo "  Passo successivo:"
echo "    1. Copia il file .env in ${APP_PATH}/.env"
echo "    2. Esegui dal PC di sviluppo: make deploy"
echo "    3. Poi: sudo systemctl start nexus-core nexus-webide"
echo "══════════════════════════════════════════════════"
