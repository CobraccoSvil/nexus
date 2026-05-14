#!/usr/bin/env bash
# Script wrapper per eseguire il bootstrap step-by-step.
# Uso: bash deploy/run-bootstrap.sh [--skip-to N]
set -euo pipefail

cd /home/administrator/ideai

export PROD_HOST=192.168.0.6
export PROXY_HOST=192.168.0.3
export SSH_USER=administrator
export DEPLOY_DIR=/opt/ideai
export PUBLIC_URL=https://nexus.cobracco.it
export ALLOW_DIRTY=1
export ASSUME_YES=1

source deploy/lib/remote.sh

SKIP_TO="${1:-0}"
SKIP_TO="${SKIP_TO#--skip-to=}"
[ "$SKIP_TO" = "--skip-to" ] && SKIP_TO="${2:-0}"

run_step() {
    local n="$1" desc="$2"
    if [ "$n" -le "${SKIP_TO:-0}" ]; then
        echo "[SKIP] Step $n: $desc"
        return 0
    fi
    step "$n" 10 "$desc"
}

# === Step 1: Pre-check ========================================================
run_step 1 "Pre-check SSH, sudo, risorse"
if [ "1" -gt "${SKIP_TO:-0}" ]; then
    remote_check_reachable "$PROD_HOST" || die "SSH a ${SSH_USER}@${PROD_HOST} non raggiungibile"
    remote_exec_quiet "$PROD_HOST" "sudo -n true" || die "sudo passwordless non configurato"
    disk_gb=$(remote_exec "$PROD_HOST" "df --output=avail -BG / | tail -1 | tr -d 'G '")
    ram_mb=$(remote_exec "$PROD_HOST" "free -m | awk '/^Mem:/ {print \$2}'")
    [ "${disk_gb:-0}" -ge 10 ] || die "Spazio disco insufficiente: ${disk_gb}GB (servono >= 10GB)"
    info "  OK  Disk ${disk_gb}GB, RAM ${ram_mb}MB"
    acquire_lock "$PROD_HOST" "ideai-bootstrap"
fi

# === Step 2: Sync sorgenti ====================================================
run_step 2 "Sync sorgenti -> $DEPLOY_DIR"
if [ "2" -gt "${SKIP_TO:-0}" ]; then
    COMMIT="$(commit_hash)"
    info "  HEAD = $COMMIT"
    remote_exec "$PROD_HOST" "sudo mkdir -p '$DEPLOY_DIR' && sudo chown ${SSH_USER}:${SSH_USER} '$DEPLOY_DIR'"
    sync_sources "$PROD_HOST" "$DEPLOY_DIR"
fi

# === Step 3: prod-setup-dev101.sh =============================================
run_step 3 "Toolchain (prod-setup-dev101.sh)"
if [ "3" -gt "${SKIP_TO:-0}" ]; then
    remote_exec "$PROD_HOST" "sudo bash '$DEPLOY_DIR/deploy/prod-setup-dev101.sh'"
fi

# === Step 4: Stack dati ========================================================
run_step 4 "Postgres 16 + Redis 7 + Qdrant"
if [ "4" -gt "${SKIP_TO:-0}" ]; then
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
fi

# === Step 5: Setup DB ==========================================================
run_step 5 "Database ai_orchestrator"
if [ "5" -gt "${SKIP_TO:-0}" ]; then
    DB_PASSWORD="${DB_PASSWORD:-$(openssl rand -hex 16)}"
    remote_exec "$PROD_HOST" "
        set -e
        sudo -u postgres psql <<SQL
DO \\\$\\\$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'postgres_app') THEN
        CREATE USER postgres_app WITH PASSWORD '$DB_PASSWORD' CREATEDB;
    ELSE
        ALTER USER postgres_app WITH PASSWORD '$DB_PASSWORD';
    END IF;
END
\\\$\\\$;
SELECT 'CREATE DATABASE ai_orchestrator OWNER postgres_app'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'ai_orchestrator')\\gexec
SELECT 'CREATE DATABASE ai_orchestrator_shadow OWNER postgres_app'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'ai_orchestrator_shadow')\\gexec
SQL
    "
    info "  OK  DB pronti (password: $DB_PASSWORD)"
    echo "$DB_PASSWORD" > /tmp/ideai-db-password.txt
fi

# === Step 6: .env ==============================================================
run_step 6 "Generazione /opt/ideai/.env"
if [ "6" -gt "${SKIP_TO:-0}" ]; then
    # Leggi password DB se generata allo step 5
    if [ -z "${DB_PASSWORD:-}" ] && [ -f /tmp/ideai-db-password.txt ]; then
        DB_PASSWORD="$(cat /tmp/ideai-db-password.txt)"
    fi
    DB_PASSWORD="${DB_PASSWORD:-CHANGEME}"

    JWT_SECRET="${JWT_SECRET:-$(openssl rand -hex 32)}"

    # Recupera API keys dal vecchio .env su .03 se disponibile
    OPENAI_API_KEY="${OPENAI_API_KEY:-}"
    ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-}"
    GOOGLE_API_KEY="${GOOGLE_API_KEY:-}"
    MISTRAL_API_KEY="${MISTRAL_API_KEY:-}"
    DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY:-}"

    if [ -z "$OPENAI_API_KEY" ]; then
        info "  Tentativo recupero API keys da .03..."
        OLD_ENV=$(ssh $SSH_OPTS "${SSH_USER}@${PROXY_HOST}" 'sudo cat /opt/ideai/.env 2>/dev/null || sudo cat /opt/ai-orchestrator/.env 2>/dev/null' 2>/dev/null || true)
        if [ -n "$OLD_ENV" ]; then
            OPENAI_API_KEY=$(echo "$OLD_ENV" | grep '^OPENAI_API_KEY=' | cut -d= -f2- || true)
            ANTHROPIC_API_KEY=$(echo "$OLD_ENV" | grep '^ANTHROPIC_API_KEY=' | cut -d= -f2- || true)
            GOOGLE_API_KEY=$(echo "$OLD_ENV" | grep '^GOOGLE_API_KEY=' | cut -d= -f2- || true)
            MISTRAL_API_KEY=$(echo "$OLD_ENV" | grep '^MISTRAL_API_KEY=' | cut -d= -f2- || true)
            DEEPSEEK_API_KEY=$(echo "$OLD_ENV" | grep '^DEEPSEEK_API_KEY=' | cut -d= -f2- || true)
            info "  API keys recuperate da .03"
        else
            warn "  Impossibile recuperare API keys da .03 (aggiungere manualmente dopo)"
        fi
    fi

    remote_exec "$PROD_HOST" "cat > $DEPLOY_DIR/.env << 'ENVEOF'
# Generato da bootstrap $(date -u +%FT%TZ)
DATABASE_URL=postgres://postgres_app:${DB_PASSWORD}@localhost:5432/ai_orchestrator
SHADOW_POSTGRES_URL=postgres://postgres_app:${DB_PASSWORD}@localhost:5432/ai_orchestrator_shadow
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
JWT_SECRET=${JWT_SECRET}
RUST_LOG=info,sqlx=warn,hyper=warn
NEXT_PUBLIC_BASE_URL=${PUBLIC_URL}
OPENAI_API_KEY=${OPENAI_API_KEY}
ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
GOOGLE_API_KEY=${GOOGLE_API_KEY}
MISTRAL_API_KEY=${MISTRAL_API_KEY}
DEEPSEEK_API_KEY=${DEEPSEEK_API_KEY}
ENVEOF
    chmod 600 $DEPLOY_DIR/.env"
    info "  OK  .env generato (chmod 600)"
fi

echo ""
log "Step 1-6 completati. Prossimo: build (step 7)."
echo "  Per continuare: bash deploy/run-build.sh"
