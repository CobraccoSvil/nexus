#!/usr/bin/env bash
# Avvio completo dello stack IDEAI in WSL per sviluppo locale.
# Avvia lo stack completo in WSL.
# Uso: ./scripts/dev-wsl.sh [stop|status|logs <servizio>]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LOG_DIR="/tmp/ideai-logs"
mkdir -p "$LOG_DIR"

# Colori
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'

info()    { echo -e "${CYAN}[ideai]${NC} $*"; }
success() { echo -e "${GREEN}[ok]${NC}   $*"; }
warn()    { echo -e "${YELLOW}[warn]${NC} $*"; }
error()   { echo -e "${RED}[err]${NC}  $*"; }

# ── Env ────────────────────────────────────────────────────────────────────────
if [ ! -f .env ]; then
    if [ -f .env.local.example ]; then
        cp .env.local.example .env
        warn ".env non trovato — copiato da .env.local.example. Configura le API key prima di procedere."
    else
        error ".env mancante. Copia .env.local.example in .env e configura le variabili."
        exit 1
    fi
fi
set -a; source .env; set +a

# ── Comandi ausiliari ──────────────────────────────────────────────────────────
cmd_stop() {
    info "Arresto tutti i servizi IDEAI..."
    pkill -f "mcp-core"        2>/dev/null || true
    pkill -f "admin-service"   2>/dev/null || true
    pkill -f "chat-service"    2>/dev/null || true
    pkill -f "doc-service"     2>/dev/null || true
    pkill -f "billing-service" 2>/dev/null || true
    pkill -f "plugin-service"  2>/dev/null || true
    pkill -f "brain.grpc_server" 2>/dev/null || true
    pkill -f "next-server"     2>/dev/null || true
    pkill -f "next dev"        2>/dev/null || true
    pkill -f "tsx.*server.ts"  2>/dev/null || true
    pkill -f "nexus-gateway"   2>/dev/null || true
    docker compose -f docker-compose.local.yml down 2>/dev/null || true
    success "Stack arrestato."
    exit 0
}

cmd_status() {
    echo ""
    echo "  Porte in ascolto:"
    ss -tlnp 2>/dev/null | grep -E '3000|4000|4010|4020|4030|4040|4050|8001|50051|6379|6333' | \
        awk '{print "    " $4}' | sort || true
    echo ""
    echo "  Health check:"
    for port in 4000 4010 4020 4030 4040 4050; do
        code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${port}/health" 2>/dev/null || echo "down")
        if [ "$code" = "200" ]; then
            echo -e "    ${GREEN}✓${NC} :${port} → OK"
        else
            echo -e "    ${RED}✗${NC} :${port} → ${code}"
        fi
    done
    echo ""
    exit 0
}

cmd_logs() {
    local svc="${2:-core}"
    local logfile="$LOG_DIR/${svc}.log"
    if [ -f "$logfile" ]; then
        tail -f "$logfile"
    else
        error "Log non trovato: $logfile"
        echo "  Servizi disponibili: core, admin, chat, docs, billing, plugins, neural, webide"
        exit 1
    fi
}

case "${1:-start}" in
    stop)   cmd_stop ;;
    status) cmd_status ;;
    logs)   cmd_logs "$@" ;;
esac

# ── Prerequisiti ───────────────────────────────────────────────────────────────
info "Verifica prerequisiti..."

if ! command -v docker &>/dev/null; then
    error "Docker non trovato. Installa Docker Desktop (con integrazione WSL2) o Docker Engine in WSL."
    exit 1
fi
if ! docker info &>/dev/null; then
    error "Docker non è in esecuzione. Avvia Docker Desktop o 'sudo service docker start'."
    exit 1
fi
if ! command -v cargo &>/dev/null; then
    error "Rust/Cargo non trovato. Esegui: curl https://sh.rustup.rs -sSf | sh"
    exit 1
fi
if ! command -v pnpm &>/dev/null; then
    error "pnpm non trovato. Esegui: npm install -g pnpm"
    exit 1
fi

# ── Connettività database ──────────────────────────────────────────────────
DB_HOST="${DATABASE_URL##*@}"; DB_HOST="${DB_HOST%%:*}"
info "Verifica connettività Postgres su ${DB_HOST}..."
if ! pg_isready -h "$DB_HOST" -p 5432 -q 2>/dev/null; then
    warn "Postgres su ${DB_HOST}:5432 non raggiungibile — verifica che il container Docker sia avviato."
    warn "I servizi che richiedono il DB potrebbero non partire correttamente."
fi

# ── Docker locale ──────────────────────────────────────────────────────────────
info "Avvio servizi Docker locali (redis, qdrant, monitoring)..."
if ss -tln 2>/dev/null | grep -qE '(:|\.)6379\s'; then
    warn "Porta 6379 già in uso: userò Redis già attivo su localhost e NON avvierò il container redis."
    docker compose -f docker-compose.local.yml up -d --quiet-pull postgres-nexus qdrant otel-collector jaeger prometheus grafana
else
    docker compose -f docker-compose.local.yml up -d --quiet-pull
fi

info "Attesa redis..."
if ss -tln 2>/dev/null | grep -qE '(:|\.)6379\s'; then
    for i in $(seq 1 30); do
        redis-cli ping &>/dev/null && break || sleep 1
    done
else
    for i in $(seq 1 30); do
        docker compose -f docker-compose.local.yml exec -T redis redis-cli ping &>/dev/null && break || sleep 1
    done
fi

# ── Node deps ──────────────────────────────────────────────────────────────────
if [ ! -d node_modules ]; then
    info "Installazione dipendenze Node.js..."
    pnpm install --frozen-lockfile
fi

# ── Build Rust (debug, più veloce per dev) ─────────────────────────────────────
info "Build Rust (debug)..."
cargo build --workspace 2>&1 | tail -5
RUST_BIN="./target/debug"

# ── Neural Core (Python) ───────────────────────────────────────────────────────
info "Avvio Neural Core (Python, :8001 + gRPC :50051)..."
pkill -f "brain.grpc_server" 2>/dev/null || true
nohup python3 -m brain.grpc_server.main --rest \
    > "$LOG_DIR/neural.log" 2>&1 &
NEURAL_PID=$!
echo $NEURAL_PID > "$LOG_DIR/neural.pid"
sleep 4

# ── Microservizi Rust ──────────────────────────────────────────────────────────
start_rust_svc() {
    local name="$1"
    shift || true
    local bin="$RUST_BIN/$name"
    info "Avvio ${name}..."
    pkill -f "$name" 2>/dev/null || true
    sleep 0.5
    # Permette di passare variabili d'ambiente extra come "KEY=VALUE".
    # Esempio: start_rust_svc "browser-bridge-mcp" BROWSER_BRIDGE_BIND_HOST="0.0.0.0"
    for pair in "$@"; do
        export "$pair"
    done
    if [ -f "$bin" ]; then
        nohup "$bin" > "$LOG_DIR/${name}.log" 2>&1 &
    else
        warn "${name}: binario non trovato, uso cargo run"
        nohup cargo run -p "$name" > "$LOG_DIR/${name}.log" 2>&1 &
    fi
    # Cleanup env vars extra per non inquinare gli altri servizi.
    for pair in "$@"; do
        local varname="${pair%%=*}"
        unset "$varname"
    done
    echo $! > "$LOG_DIR/${name}.pid"
}

start_rust_svc "mcp-core"
sleep 3
start_rust_svc "admin-service"
start_rust_svc "billing-service"
start_rust_svc "chat-service"
start_rust_svc "doc-service"
start_rust_svc "plugin-service"
# WSL2: esponiamo il bridge su 0.0.0.0 per permettere a Chrome Windows
# di raggiungere http://127.0.0.1:4055/extension/update.xml via port-forward.
start_rust_svc "browser-bridge-mcp" BROWSER_BRIDGE_BIND_HOST="0.0.0.0" BROWSER_BRIDGE_PUBLIC_HOST="127.0.0.1"
sleep 3

# ── Nexus Gateway (Rust, :4060) ───────────────────────────────────────────────
info "Avvio Nexus LLM Gateway (:4060)..."
# Migrazione Fase 6: il gateway e' il binario Rust del crate nexus-gateway. Il
# vecchio server Node/TypeScript (apps/nexus-gateway) e' stato eliminato.
pkill -f "$REPO_ROOT/target/debug/nexus-gateway" 2>/dev/null || true
sleep 0.5
GATEWAY_BIN="$REPO_ROOT/target/debug/nexus-gateway"
if [ ! -x "$GATEWAY_BIN" ]; then
    info "Build nexus-gateway (Rust) mancante, compilo..."
    ( cd "$REPO_ROOT" && cargo build -p nexus-gateway --bin nexus-gateway ) >> "$LOG_DIR/nexus-gateway.log" 2>&1 || warn "build nexus-gateway fallita"
fi
# Carichiamo `.env` in bash; le config (policy/alias/chiavi) sono risolte dal DB
# all'avvio (regola G). cwd = REPO_ROOT per i file config relativi.
setsid bash -lc "set -a && source \"$REPO_ROOT/.env\" && set +a && cd \"$REPO_ROOT\" && exec \"$GATEWAY_BIN\" > \"$LOG_DIR/nexus-gateway.log\" 2>&1" &
echo $! > "$LOG_DIR/nexus-gateway.pid"

# Smoke check porta
for i in $(seq 1 10); do
    if ss -tln 2>/dev/null | grep -qE '(:|\.)4060\s'; then
        break
    fi
    sleep 0.4
done
if ! ss -tln 2>/dev/null | grep -qE '(:|\.)4060\s'; then
    warn "Nexus Gateway non in ascolto su :4060 (controlla $LOG_DIR/nexus-gateway.log)"
fi

# ── Web IDE (Next.js) ──────────────────────────────────────────────────────────
info "Avvio Web IDE (Next.js, :3000)..."
pkill -f "next dev" 2>/dev/null || true
pkill -f "next-server" 2>/dev/null || true
# setsid + nohup per isolare dalla sessione WSL ed evitare SIGHUP alla chiusura del terminale.
# Nota: teniamo il redirect dentro bash -lc per preservare cwd e log coerenti.
setsid bash -lc "cd \"${REPO_ROOT}/apps/web-ide\" && nohup ./node_modules/.bin/next dev -H 0.0.0.0 -p 3000 > \"${LOG_DIR}/webide.log\" 2>&1" &
echo $! > "$LOG_DIR/webide.pid"
sleep 5

# ── Smoke check Browser Bridge daemon (:4055) ─────────────────────────────────
for i in $(seq 1 10); do
    if ss -tln 2>/dev/null | grep -qE '(:|\.)4055\s'; then
        break
    fi
    sleep 0.4
done
if ss -tln 2>/dev/null | grep -qE '(:|\.)4055\s'; then
    success "Browser Bridge daemon in ascolto su :4055"
else
    warn "Browser Bridge daemon NON in ascolto su :4055 (controlla $LOG_DIR/browser-bridge-mcp.log)"
fi

# ── Status finale ──────────────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════"
echo "  IDEAI — Stack locale avviato"
echo "════════════════════════════════════════════════════"
echo "  Web IDE         →  http://localhost:3000"
echo "  Core API        →  http://localhost:4000"
echo "  Nexus Gateway   →  http://localhost:4060"
echo "  Nexus Health    →  http://localhost:4060/health"
echo "  Browser Bridge  →  http://127.0.0.1:4055/health"
echo "  Jaeger UI       →  http://localhost:16686"
echo "  Grafana         →  http://localhost:3001  (admin/admin)"
echo "  Prometheus      →  http://localhost:9090"
echo "  Qdrant          →  http://localhost:6333/dashboard"
echo ""
echo "  Database        →  ${DB_HOST}:5432 "
echo ""
echo "  Log:  ./scripts/dev-wsl.sh logs <core|admin|chat|neural|nexus-gateway|webide>"
echo "  Stop: ./scripts/dev-wsl.sh stop"
echo "════════════════════════════════════════════════════"

sleep 8
echo ""
cmd_status
