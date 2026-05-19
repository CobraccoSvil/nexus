#!/usr/bin/env bash
# deploy/bootstrap-prod.sh - Setup one-shot di un host applicativo Nexus.
#
# Esegue, idempotente, sull'host PROD_HOST:
#   1. Pre-check: SSH, sudo, disk space, RAM
#   2. Sync repo da workstation
#   3. prod-setup-dev101.sh: pacchetti, Rust, Node, Python venv, nginx, systemd, logrotate
#   4. Postgres 16 + Redis 7 (apt) + Qdrant (Docker singolo container)
#   5. Setup DB nexus + nexus_shadow
#   6. Genera /opt/ideai/.env (JWT_SECRET random + prompt API keys)
#   7. Build iniziale Rust + Node + Python (delegato a deploy-prod.sh --all --first-build)
#   8. Patch nginx-microservices.conf con set_real_ip_from $PROXY_HOST
#   9. Enable systemd units
#   10. Smoke test interno
#
# Lock: /tmp/ideai-bootstrap.lock (flock --conflict-exit-code 11).
# Uso:  ./deploy/bootstrap-prod.sh [--skip-build] [--reuse-env]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/remote.sh
. "$SCRIPT_DIR/lib/remote.sh"

SKIP_BUILD="0"
REUSE_ENV="0"
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD="1" ;;
        --reuse-env)  REUSE_ENV="1" ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -25
            exit 0 ;;
        *) die "Flag sconosciuto: $arg" ;;
    esac
done

print_header "Bootstrap Nexus su $PROD_HOST (proxy: $PROXY_HOST)"

# === Step 1: Pre-check ========================================================
step 1 10 "Pre-check SSH, sudo, risorse"

remote_check_reachable "$PROD_HOST" \
    || die "SSH a ${SSH_USER}@${PROD_HOST} non raggiungibile"

remote_exec_quiet "$PROD_HOST" "sudo -n true" \
    || die "sudo passwordless non configurato per $SSH_USER su $PROD_HOST"

disk_gb=$(remote_exec "$PROD_HOST" "df --output=avail -BG / | tail -1 | tr -d 'G '")
[ "${disk_gb:-0}" -ge 20 ] \
    || die "Spazio disco insufficiente su /: ${disk_gb}GB (servono >= 20GB)"

ram_mb=$(remote_exec "$PROD_HOST" "free -m | awk '/^Mem:/ {print \$2}'")
[ "${ram_mb:-0}" -ge 4000 ] \
    || warn "RAM ${ram_mb}MB (raccomandato >= 4GB per build Rust)"

info "  OK  Disk ${disk_gb}GB, RAM ${ram_mb}MB"

# Lock bootstrap
acquire_lock "$PROD_HOST" "ideai-bootstrap"

# === Step 2: Sync sorgenti ====================================================
step 2 10 "Sync sorgenti -> $DEPLOY_DIR"

require_clean_tree || true
COMMIT="$(commit_hash)"
info "  HEAD = $COMMIT"

remote_exec "$PROD_HOST" "sudo mkdir -p '$DEPLOY_DIR' && sudo chown ${SSH_USER}:${SSH_USER} '$DEPLOY_DIR'"
sync_sources "$PROD_HOST" "$DEPLOY_DIR"

# === Step 3: prod-setup-dev101.sh =============================================
step 3 10 "Installazione toolchain + systemd + nginx (prod-setup-dev101.sh)"

remote_exec "$PROD_HOST" "sudo bash '$DEPLOY_DIR/deploy/prod-setup-dev101.sh'"

# === Step 4: Stack dati ========================================================
step 4 10 "Postgres 16 + Redis 7 + Qdrant"

remote_exec "$PROD_HOST" '
    set -e
    if ! command -v psql >/dev/null 2>&1; then
        sudo apt-get install -y -qq postgresql-16 postgresql-contrib-16
    fi
    if ! command -v redis-cli >/dev/null 2>&1; then
        sudo apt-get install -y -qq redis-server
    fi
    sudo systemctl enable --now postgresql redis-server
'

# Qdrant via Docker singolo container, label di progetto (direttiva E CLAUDE.md)
remote_exec "$PROD_HOST" '
    if ! docker ps --filter "name=^ideai-qdrant$" --format "{{.Names}}" | grep -q ideai-qdrant; then
        docker rm -f ideai-qdrant 2>/dev/null || true
        docker run -d \
            --name ideai-qdrant \
            --restart unless-stopped \
            --label com.docker.compose.project=ideai \
            -p 127.0.0.1:6333:6333 \
            -p 127.0.0.1:6334:6334 \
            -v qdrant-data:/qdrant/storage \
            qdrant/qdrant:v1.13.4
    fi
'
info "  OK  Postgres + Redis + Qdrant attivi"

# === Step 5: Setup DB ==========================================================
step 5 10 "Database nexus + nexus_shadow"

DB_PASSWORD="${DB_PASSWORD:-$(openssl rand -hex 16)}"
remote_exec "$PROD_HOST" "
    set -e
    sudo -u postgres psql <<SQL
DO \$\$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'postgres_app') THEN
        CREATE USER postgres_app WITH PASSWORD '$DB_PASSWORD' CREATEDB;
    ELSE
        ALTER USER postgres_app WITH PASSWORD '$DB_PASSWORD';
    END IF;
END
\$\$;
SELECT 'CREATE DATABASE nexus OWNER postgres_app'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'nexus')\\gexec
SELECT 'CREATE DATABASE nexus_shadow OWNER postgres_app'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'nexus_shadow')\\gexec
SQL
"
info "  OK  DB pronti (password salvata in /opt/ideai/.env)"

# === Step 6: /opt/ideai/.env ===================================================
step 6 10 "Generazione /opt/ideai/.env"

if [ "$REUSE_ENV" = "1" ] \
   && remote_exec_quiet "$PROD_HOST" "test -f $DEPLOY_DIR/.env"; then
    info "  --reuse-env: salto generazione, file gia' presente"
else
    JWT_SECRET="${JWT_SECRET:-$(openssl rand -hex 32)}"

    # API keys: prompt interattivo se TTY, altrimenti placeholder
    if [ -t 0 ] && [ "${ASSUME_YES:-}" != "1" ]; then
        printf '\n%bAPI keys provider (Invio per saltare):%b\n' "$C_YELLOW" "$C_NC"
        read -r -p "  OPENAI_API_KEY:    " OPENAI_API_KEY_IN     || true
        read -r -p "  ANTHROPIC_API_KEY: " ANTHROPIC_API_KEY_IN  || true
        read -r -p "  GOOGLE_API_KEY:    " GOOGLE_API_KEY_IN     || true
        read -r -p "  MISTRAL_API_KEY:   " MISTRAL_API_KEY_IN    || true
        read -r -p "  DEEPSEEK_API_KEY:  " DEEPSEEK_API_KEY_IN   || true
    fi

    # Render .env via heredoc remoto
    remote_exec "$PROD_HOST" "
        sudo tee $DEPLOY_DIR/.env >/dev/null <<ENV
# Generato da bootstrap-prod.sh il \$(date -u +%FT%TZ)
DATABASE_URL=postgres://postgres_app:$DB_PASSWORD@localhost:5432/nexus
SHADOW_POSTGRES_URL=postgres://postgres_app:$DB_PASSWORD@localhost:5432/nexus_shadow
REDIS_URL=redis://localhost:6379
QDRANT_URL=http://localhost:6334
MCP_SERVER_PORT=4000
NEURAL_CORE_URL=http://127.0.0.1:50051
WEB_APP_PORT=3000
ADMIN_SERVICE_PORT=4010
CHAT_SERVICE_PORT=4020
DOC_SERVICE_PORT=4030
BILLING_SERVICE_PORT=4040
PLUGIN_SERVICE_PORT=4050
CORE_SERVICE_URL=http://127.0.0.1:4000
BILLING_SERVICE_URL=http://127.0.0.1:4040
PLUGIN_SERVICE_URL=http://127.0.0.1:4050
JWT_SECRET=$JWT_SECRET
RUST_LOG=info,sqlx=warn,hyper=warn
NEXT_PUBLIC_BASE_URL=$PUBLIC_URL
OPENAI_API_KEY=${OPENAI_API_KEY_IN:-}
ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY_IN:-}
GOOGLE_API_KEY=${GOOGLE_API_KEY_IN:-}
MISTRAL_API_KEY=${MISTRAL_API_KEY_IN:-}
DEEPSEEK_API_KEY=${DEEPSEEK_API_KEY_IN:-}
ENV
        sudo chown ${SSH_USER}:${SSH_USER} $DEPLOY_DIR/.env
        sudo chmod 600 $DEPLOY_DIR/.env
    "
    info "  OK  .env generato (chmod 600)"
fi

# === Step 7: Build iniziale ====================================================
step 7 10 "Build iniziale (Rust + Node + Python)"

if [ "$SKIP_BUILD" = "1" ]; then
    warn "  --skip-build: salto build (eseguire 'make deploy' dopo)"
else
    remote_exec "$PROD_HOST" "
        set -e
        cd $DEPLOY_DIR
        export PATH=\$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin
        echo '  Cargo build --release...'
        ~/.cargo/bin/cargo build --release -p mcp-core 2>&1 | tail -5
        mkdir -p bin bin/.previous
        cp -f target/release/mcp-core bin/

        echo '  pnpm install + build web-ide...'
        pnpm install --frozen-lockfile --silent 2>&1 | tail -3
        pnpm --filter @ai-orchestrator/web-ide build 2>&1 | tail -5

        echo '  pip install brain...'
        ${DEPLOY_DIR}/.venv/bin/pip install -q -e 'brain[all]' 2>&1 | tail -3 || true
    "
    info "  OK  Build completato"
fi

# === Step 8: Patch nginx-microservices.conf ===================================
step 8 10 "Patch nginx (trust proxy $PROXY_HOST)"

remote_exec "$PROD_HOST" "
    NGINX_CONF=/etc/nginx/sites-available/ideai
    if ! sudo grep -q 'set_real_ip_from $PROXY_HOST' \"\$NGINX_CONF\"; then
        sudo sed -i '/server_name _;/a\\    set_real_ip_from $PROXY_HOST;\\n    real_ip_header X-Real-IP;\\n    real_ip_recursive on;' \"\$NGINX_CONF\"
    fi
    sudo nginx -t && sudo systemctl reload nginx
"

# === Step 9: systemd enable ====================================================
step 9 10 "systemd enable + start"

remote_exec "$PROD_HOST" "
    sudo systemctl daemon-reload
    for unit in nexus-neural nexus-core nexus-webide; do
        sudo systemctl enable --now \$unit
    done
    # Microservizi opzionali (best-effort, alcuni non sono ancora attivi)
    for unit in nexus-admin nexus-chat nexus-billing nexus-docs nexus-plugins; do
        if [ -f /etc/systemd/system/\$unit.service ]; then
            sudo systemctl enable --now \$unit 2>/dev/null || true
        fi
    done
"
info "  Attendo 10s per readiness dei servizi..."
sleep 10

# === Step 10: Health check =====================================================
step 10 10 "Health check"

if "$SCRIPT_DIR/health-check.sh" --verbose; then
    print_header "Bootstrap COMPLETATO su $PROD_HOST"
    info "Commit deployato: $COMMIT"
    info "Prossimi passi:"
    info "  1) make proxy-reload   # aggiorna nginx su $PROXY_HOST"
    info "  2) make health         # verifica end-to-end"
    info "  3) make deploy         # deploy ricorrenti futuri"
else
    err "Health check FALLITO. Servizi non pronti."
    err "Controlla i log: ssh ${SSH_USER}@${PROD_HOST} 'sudo journalctl -u nexus-core -n 50'"
    exit 1
fi
