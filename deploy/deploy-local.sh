#!/bin/bash
# Deploy IDEAI in locale su WSL.
# Uso: ./deploy/deploy-local.sh [--rust] [--web] [--service <nome>] [--menu] [--debug] [--sync]
#
#   --rust              build + restart solo backend Rust
#   --web               build + restart solo web-ide (Next.js)
#   --service <nome>    restart solo il servizio indicato (es. mcp-core, brain, nexus-gateway, web-ide)
#   --menu              mostra menu interattivo per scegliere il servizio
#                       (equivalente a --service senza nome)
#   --debug             compila Rust in debug (stacktrace completi, compilazione rapida)
#   --list-services     elenca i servizi disponibili e esce
#   --sync              sincronizza worktree Windows -> WSL prima del deploy
#   --sync-only         sincronizza e basta (senza build/restart)
#   (senza flag)        build tutto + restart tutti i servizi

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="${ROOT}/.env"
# Directory binari — impostata dopo il parsing dei flag (release o debug)
BIN_DIR=""
CARGO_PROFILE_FLAG=""

export PATH="/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
export HOME=/home/administrator

# Carica .env: usa sia source (per le variabili bash) che lettura diretta (per env).
# source può fallire silenziosamente su alcune variabili con caratteri speciali;
# le variabili critiche vengono lette esplicitamente con grep+cut.
if [ -f "$ENV_FILE" ]; then
    set -a; source "$ENV_FILE" || true; set +a
    # Lettura diretta dei valori critici (override per sicurezza)
    _read_env() { grep -m1 "^${1}=" "$ENV_FILE" 2>/dev/null | cut -d= -f2-; }
    DATABASE_URL="${DATABASE_URL:-$(_read_env DATABASE_URL)}"
    POSTGRES_URL="${POSTGRES_URL:-$(_read_env POSTGRES_URL)}"
    JWT_SECRET="${JWT_SECRET:-$(_read_env JWT_SECRET)}"
    NEXUS_GATEWAY_PORT="${NEXUS_GATEWAY_PORT:-$(_read_env NEXUS_GATEWAY_PORT)}"
    export DATABASE_URL POSTGRES_URL JWT_SECRET NEXUS_GATEWAY_PORT
fi

log() { echo "==> $*"; }

RUST_ONLY=false
WEB_ONLY=false
SINGLE_SERVICE=""
SHOW_MENU=false
LIST_SERVICES=false
DO_SYNC=false
SYNC_ONLY=false
DEBUG_BUILD=false

# ─── Catalogo servizi gestibili via --service ────────────────────────────────
# Formato: nome|kind|descrizione
#   kind = rust       → binario Rust in target/{release,debug}, gestito da start_service()
#          web-ide    → Next.js, build + start_webide()
#          brain      → Python module brain.grpc_server.main
#          gateway    → Node.js dist/server.js con env var aggiuntive
#          builtin    → microservizi Rust comuni (admin/chat/billing/...)
SERVICES_CATALOG=(
    "mcp-core|rust|Routing AI, agent runner, ToolRunner gRPC (porta 4000)"
    "brain|brain|Neural Core Python: classifier, agenti LangGraph, embeddings (porta 8001)"
    "web-ide|web-ide|Frontend Next.js (porta 3000)"
    "nexus-gateway|gateway|API gateway LLM (porta 4060) — Node.js"
    "admin-service|builtin|Backend admin UI (porta 4010)"
    "chat-service|builtin|Backend chat UI (porta 4020)"
    "doc-service|builtin|Backend doc generator (porta 4030)"
    "billing-service|builtin|Backend billing/quote (porta 4040)"
    "plugin-service|builtin|Backend plugin manager (porta 4050)"
    "browser-bridge-mcp|builtin|MCP browser bridge (porta 4055)"
)

# Worktree Windows montato in WSL (autodetect dal path dello script)
WIN_WORKTREE=""
_detect_win_worktree() {
    local candidates=(
        "/mnt/d/Sviluppo/ideai/fervent-cohen-fad855"
        "/mnt/d/Sviluppo/IDEAI"
    )
    for c in "${candidates[@]}"; do
        if [ -d "$c/crates" ] && [ -d "$c/brain" ]; then
            WIN_WORKTREE="$c"
            return
        fi
    done
}

sync_from_windows() {
    _detect_win_worktree
    if [ -z "$WIN_WORKTREE" ]; then
        log "SKIP sync: nessun worktree Windows trovato in /mnt/d/"
        return 1
    fi
    log "Sync: ${WIN_WORKTREE} -> ${ROOT}"
    rsync -a \
        --exclude='target/' \
        --exclude='node_modules/' \
        --exclude='.next/' \
        --exclude='.env' \
        --exclude='.env.build' \
        --exclude='.git/' \
        --exclude='.git' \
        --exclude='*.pyc' \
        --exclude='__pycache__/' \
        --exclude='.turbo/' \
        --exclude='dist/' \
        --exclude='projects/' \
        "${WIN_WORKTREE}/" "${ROOT}/"
    log "Conversione CRLF -> LF..."
    find "${ROOT}" \( -name '*.rs' -o -name '*.py' -o -name '*.sql' -o -name '*.ts' -o -name '*.tsx' -o -name '*.sh' -o -name '*.toml' -o -name '*.json' -o -name '*.yaml' -o -name '*.yml' \) -exec sed -i 's/\r$//' {} + 2>/dev/null
    log "Sync completato."
}

while [ $# -gt 0 ]; do
    case "$1" in
        --rust)            RUST_ONLY=true ;;
        --web)             WEB_ONLY=true ;;
        --service)
            shift
            SINGLE_SERVICE="${1:-}"
            # `--service` senza nome → apre il menu interattivo
            if [ -z "$SINGLE_SERVICE" ]; then SHOW_MENU=true; fi
            ;;
        --menu)            SHOW_MENU=true ;;
        --list-services)   LIST_SERVICES=true ;;
        --debug)           DEBUG_BUILD=true ;;
        --sync)            DO_SYNC=true ;;
        --sync-only)       SYNC_ONLY=true; DO_SYNC=true ;;
    esac
    shift
done

# ── Configura profilo build Rust (debug/release) ────────────────────────────
if $DEBUG_BUILD; then
    BIN_DIR="${ROOT}/target/debug"
    CARGO_PROFILE_FLAG=""
    export RUST_BACKTRACE=1
    export RUST_LOG="${RUST_LOG:-debug}"
    log "Modalita DEBUG: stacktrace completi, RUST_BACKTRACE=1, compilazione rapida"
else
    BIN_DIR="${ROOT}/target/release"
    CARGO_PROFILE_FLAG="--release"
fi

if $DO_SYNC; then
    sync_from_windows
fi

if $SYNC_ONLY; then
    exit 0
fi

stop_service() {
    local name="$1"
    pkill -f "$name" 2>/dev/null && sleep 1 || true
}

start_service() {
    local name="$1"
    shift
    local bin="${BIN_DIR}/${name}"
    local logfile="/tmp/nexus-${name}.log"
    # Esporta le variabili extra (es. ENABLE_TOOL_RUNNER=1) nell'ambiente corrente
    # prima di lanciare il processo, poi le rimuove per non inquinare il resto.
    local env_backup=""
    for pair in "$@"; do
        export "$pair"
    done
    if [ ! -f "$bin" ]; then
        log "ATTENZIONE: ${bin} non trovato, avvio tramite cargo run"
        setsid nohup bash -c "cd ${ROOT} && cargo run -p ${name} ${CARGO_PROFILE_FLAG}" > "$logfile" 2>&1 < /dev/null &
    else
        setsid nohup "$bin" > "$logfile" 2>&1 < /dev/null &
    fi
    local pid=$!
    disown || true
    # Rimuove le variabili extra dall'ambiente dello shell corrente
    for pair in "$@"; do
        local varname="${pair%%=*}"
        unset "$varname"
    done
    echo "  ${name} PID=${pid} log=${logfile}"
}

# Mappa dei servizi con eventuali env var extra necessarie all'avvio.
# Formato: "nome:VAR1=val1:VAR2=val2" — le coppie dopo il primo ":" sono env var.
declare -A SERVICE_ENV
SERVICE_ENV["browser-bridge-mcp"]="BROWSER_BRIDGE_PORT=${BROWSER_BRIDGE_PORT:-4055}"

start_service_with_env() {
    local name="$1"
    local extra_env="${SERVICE_ENV[$name]:-}"
    if [ -n "$extra_env" ]; then
        start_service "$name" "$extra_env"
    else
        start_service "$name"
    fi
}

stop_brain() {
    pkill -f 'brain.grpc_server.main' 2>/dev/null || true
    sleep 1
}

start_brain() {
    local logfile="/tmp/nexus-neural.log"
    setsid nohup env \
        DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}" \
        HF_HUB_OFFLINE=1 \
        TRANSFORMERS_OFFLINE=1 \
        python3 -m brain.grpc_server.main --rest \
        > "$logfile" 2>&1 < /dev/null &
    local pid=$!
    disown || true
    echo "  brain PID=${pid} log=${logfile}"
}

stop_gateway() {
    pkill -f 'apps/nexus-gateway/dist/server\.js' 2>/dev/null || true
    sleep 1
}

start_gateway() {
    local logfile="/tmp/nexus-gateway.log"
    setsid nohup env \
        NODE_ENV=production \
        DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}" \
        POSTGRES_URL="${POSTGRES_URL:-${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}}" \
        NEXUS_GATEWAY_PORT="${NEXUS_GATEWAY_PORT:-4060}" \
        NEXUS_LLM_POLICY_FILE="${NEXUS_LLM_POLICY_FILE:-${ROOT}/config/policies/default.yaml}" \
        NEXUS_MODEL_ALIASES_FILE="${NEXUS_MODEL_ALIASES_FILE:-${ROOT}/config/model-aliases.yaml}" \
        JWT_SECRET="${JWT_SECRET:-}" \
        node "${ROOT}/apps/nexus-gateway/dist/server.js" \
        > "$logfile" 2>&1 < /dev/null &
    local pid=$!
    disown || true
    echo "  nexus-gateway PID=${pid} log=${logfile}"
}

# Ritorna il `kind` di un servizio (rust|brain|gateway|web-ide|builtin) o
# vuoto se il nome non e' nel catalogo.
service_kind() {
    local name="$1"
    for entry in "${SERVICES_CATALOG[@]}"; do
        IFS='|' read -r sn kind _desc <<< "$entry"
        if [ "$sn" = "$name" ]; then
            echo "$kind"
            return
        fi
    done
    echo ""
}

list_services() {
    echo "Servizi disponibili (--service <nome>):"
    printf "  %-22s %-12s %s\n" "NOME" "TIPO" "DESCRIZIONE"
    for entry in "${SERVICES_CATALOG[@]}"; do
        IFS='|' read -r sn kind desc <<< "$entry"
        printf "  %-22s %-12s %s\n" "$sn" "$kind" "$desc"
    done
}

# Menu interattivo: mostra la lista numerata, legge la scelta da stdin.
# Imposta SINGLE_SERVICE alla scelta dell'utente. Esce con 1 se annullato.
prompt_service_menu() {
    echo ""
    echo "═════════════════════════════════════════════════════════════════════"
    echo " Quale servizio vuoi riavviare?"
    echo "═════════════════════════════════════════════════════════════════════"
    local i=1
    local names=()
    for entry in "${SERVICES_CATALOG[@]}"; do
        IFS='|' read -r sn kind desc <<< "$entry"
        printf "  %2d) %-22s [%-7s] %s\n" "$i" "$sn" "$kind" "$desc"
        names+=("$sn")
        i=$((i+1))
    done
    echo "   0) annulla"
    echo ""
    local choice
    read -r -p "Scegli [0-${#names[@]}]: " choice
    if [ -z "$choice" ] || [ "$choice" = "0" ]; then
        echo "Annullato."
        exit 0
    fi
    if ! [[ "$choice" =~ ^[0-9]+$ ]] || [ "$choice" -lt 1 ] || [ "$choice" -gt "${#names[@]}" ]; then
        echo "Scelta non valida: $choice" >&2
        exit 1
    fi
    SINGLE_SERVICE="${names[$((choice-1))]}"
    echo "→ Selezionato: $SINGLE_SERVICE"
    echo ""
}

stop_webide() {
    # Fix M49: il pattern "server\.js" matchava ANCHE il nexus-gateway
    # (node apps/nexus-gateway/dist/server.js), causando il kill collaterale
    # del gateway a ogni rebuild --web. Ora il match e' ristretto al solo
    # binary del web-ide (apps/web-ide/server.js).
    pkill -f "apps/web-ide/server\.js" 2>/dev/null || true
    pkill -f "next-server" 2>/dev/null || true
    pkill -f "next start"  2>/dev/null || true
    sleep 1
}

start_webide() {
    local logfile="/tmp/nexus-webide.log"
    setsid nohup env NODE_ENV=production node "${ROOT}/apps/web-ide/server.js" \
        > "$logfile" 2>&1 < /dev/null &
    disown || true
    echo "  web-ide PID=$! log=${logfile}"
}

build_webide() {
    log "Build web-ide (Next.js)..."
    cd "${ROOT}/apps/web-ide"
    # Pulizia cache build prima del rebuild. Senza questo, una `.next/` cached
    # con stato inconsistente (es. dopo merge/stash) puo' includere chunks
    # vecchi che escludono moduli per tree-shaking errato. Esempio osservato:
    # dispatcher SSE non incluso nel bundle nonostante presente nei sorgenti.
    rm -rf .next .turbo
    NODE_ENV=production node_modules/.bin/next build
    cd "$ROOT"
}

# ── Gestione --list-services e --menu (devono precedere il branch --service) ──
if $LIST_SERVICES; then
    list_services
    exit 0
fi

if $SHOW_MENU; then
    prompt_service_menu
fi

# ── Restart singolo servizio ──────────────────────────────────────────────────
if [ -n "$SINGLE_SERVICE" ]; then
    kind=$(service_kind "$SINGLE_SERVICE")
    if [ -z "$kind" ]; then
        # Compat: se il nome non e' nel catalogo, lo trattiamo come rust legacy
        # (la vecchia behavior dello script). Logga avviso ma procedi.
        log "AVVISO: '${SINGLE_SERVICE}' non e' in SERVICES_CATALOG, provo come rust binary."
        kind="rust"
    fi
    log "Restart ${SINGLE_SERVICE} (kind=${kind})..."
    case "$kind" in
        rust|builtin)
            stop_service "$SINGLE_SERVICE"
            cargo build ${CARGO_PROFILE_FLAG} -p "$SINGLE_SERVICE" 2>&1 | tail -5
            start_service_with_env "$SINGLE_SERVICE"
            ;;
        brain)
            stop_brain
            start_brain
            ;;
        gateway)
            stop_gateway
            start_gateway
            ;;
        web-ide)
            build_webide
            stop_webide
            start_webide
            ;;
        *)
            echo "Errore: kind sconosciuto '${kind}' per '${SINGLE_SERVICE}'" >&2
            exit 1
            ;;
    esac
    sleep 2
    log "Fatto."
    exit 0
fi

# ── Solo web-ide ──────────────────────────────────────────────────────────────
if $WEB_ONLY; then
    build_webide
    stop_webide
    start_webide
    sleep 3
    log "Fatto."
    exit 0
fi

# ── Solo Rust ─────────────────────────────────────────────────────────────────
if $RUST_ONLY; then
    log "Build Rust ($($DEBUG_BUILD && echo 'debug' || echo 'release'))..."
    cd "$ROOT"
    cargo build ${CARGO_PROFILE_FLAG} --workspace 2>&1 | tail -10
    log "Restart servizi Rust..."
    for svc in mcp-core admin-service chat-service billing-service doc-service plugin-service browser-bridge-mcp; do
        stop_service "$svc"
    done
    start_service "mcp-core"
    sleep 3
    for svc in admin-service chat-service billing-service doc-service plugin-service; do
        start_service "$svc"
    done
    start_service_with_env "browser-bridge-mcp"
    sleep 2
    log "Fatto."
    exit 0
fi

# ── Build + restart completo ──────────────────────────────────────────────────
log "Build Rust ($($DEBUG_BUILD && echo 'debug' || echo 'release'))..."
cd "$ROOT"
cargo build ${CARGO_PROFILE_FLAG} --workspace 2>&1 | tail -10

build_webide

log "Arresto servizi in esecuzione..."
for svc in mcp-core admin-service chat-service billing-service doc-service plugin-service browser-bridge-mcp "brain.grpc_server.main" "nexus-gateway" "apps/nexus-gateway"; do
    stop_service "$svc"
done
stop_webide
sleep 2

log "Avvio Neural Core (Python) con REST endpoint su :8001..."
setsid nohup env DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}" \
    HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
    python3 -m brain.grpc_server.main --rest > /tmp/nexus-neural.log 2>&1 < /dev/null &
disown || true
sleep 4

log "Avvio Nexus Gateway (Node.js su :4060)..."
setsid nohup env \
    NODE_ENV=production \
    DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}" \
    POSTGRES_URL="${POSTGRES_URL:-${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}}" \
    NEXUS_GATEWAY_PORT="${NEXUS_GATEWAY_PORT:-4060}" \
    NEXUS_LLM_POLICY_FILE="${NEXUS_LLM_POLICY_FILE:-${ROOT}/config/policies/default.yaml}" \
    NEXUS_MODEL_ALIASES_FILE="${NEXUS_MODEL_ALIASES_FILE:-${ROOT}/config/model-aliases.yaml}" \
    JWT_SECRET="${JWT_SECRET:-}" \
    node "${ROOT}/apps/nexus-gateway/dist/server.js" > /tmp/nexus-gateway.log 2>&1 < /dev/null &
disown || true
sleep 2

log "Avvio mcp-core..."
# ToolRunner e AgentRouter sono abilitati dalla tabella settings nel DB
# (chiavi tool_runner_enabled, agent_router_enabled — categoria agent).
# Le env var ENABLE_TOOL_RUNNER / ENABLE_AGENT_ROUTER restano come override
# di emergenza ma non devono essere passate qui in condizioni normali.
start_service "mcp-core"
sleep 3

log "Avvio microservizi..."
for svc in admin-service chat-service billing-service doc-service plugin-service; do
    start_service "$svc"
done
sleep 3

log "Avvio browser-bridge-mcp..."
start_service_with_env "browser-bridge-mcp"
sleep 1

log "Avvio web-ide..."
start_webide
sleep 3

echo ""
log "Porte attive:"
ss -tlnp 2>/dev/null | grep -E '3000|4000|4010|4020|4030|4040|4050|4055|4060|8001' || true

echo ""
log "Health check:"
declare -A PORT_LABEL=(
    [3000]="web-ide          "
    [4000]="mcp-core         "
    [4010]="admin-service    "
    [4020]="chat-service     "
    [4030]="doc-service      "
    [4040]="billing-service  "
    [4050]="plugin-service   "
    [4055]="browser-bridge   "
    [4060]="nexus-gateway    "
    [8001]="brain (Python)   "
)
for port in 3000 4000 4010 4020 4030 4040 4050 4055 4060 8001; do
    label="${PORT_LABEL[$port]}"
    # Per brain usa /health, per web-ide controlla semplicemente la connessione TCP
    if [ "$port" = "3000" ]; then
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/" 2>/dev/null || echo "down")
    else
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health" 2>/dev/null || echo "down")
    fi
    [ "$code" = "200" ] && echo "  :${port} ${label} OK" || echo "  :${port} ${label} ${code}"
done

echo ""
log "Deploy locale completato."
