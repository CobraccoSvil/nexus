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
#   --clean             forza la purge di .next/.turbo prima del build web (di
#                       default la cache viene riusata; serve solo dopo merge/stash)
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
    # NB: le PORTE dei servizi NON si leggono piu' da .env (regola G): ogni
    # servizio le risolve dal DB (settings) all'avvio. Qui esportiamo solo le
    # credenziali di bootstrap necessarie a raggiungere il DB.
    export DATABASE_URL POSTGRES_URL JWT_SECRET
fi

# Helper: legge una porta dal DB (settings) per gli health-check dello script.
# Stessa fonte di verita' dei servizi (regola G). Usa il container Postgres
# Nexus; se non raggiungibile ritorna vuoto (health-check degradato, non fatale).
_db_port() {
    docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -tAc \
        "SELECT value FROM settings WHERE key = '$1'" 2>/dev/null | tr -d '[:space:]'
}

log() { echo "==> $*"; }

CLEAN_BUILD=false
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
        --clean)           CLEAN_BUILD=true ;;
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

# ── Modelli ML per la feature onnx (semantic embedder) ──────────────────────
# Idempotente: scarica model.onnx/tokenizer.json solo se mancanti. Non versionati
# nel repo perche' troppo grandi (vedi .gitignore + scripts/fetch-models.sh).
if [ -x "${ROOT}/scripts/fetch-models.sh" ]; then
    "${ROOT}/scripts/fetch-models.sh" || log "WARN: fetch-models fallito; OnnxMiniLmEmbedder cadra' sul fallback HashEmbedder"
fi

if $DO_SYNC; then
    sync_from_windows
fi

if $SYNC_ONLY; then
    exit 0
fi

# Ferma in modo AFFIDABILE tutti i processi che matchano il pattern (regola H):
# SIGTERM -> attende l'uscita graceful (poll) -> SIGKILL se non muore entro il
# timeout. Sostituisce il vecchio `pkill + sleep 1` che lasciava una RACE: il
# nuovo processo veniva avviato mentre il vecchio era ancora attaccato alla
# porta, lasciando due istanze a servire richieste a intermittenza (binario
# vecchio vs nuovo). Insieme al single-instance lock nel codice (mcp-core/brain)
# garantisce una sola istanza per porta.
_stop_pattern() {
    local pattern="$1"
    local label="${2:-$pattern}"
    pgrep -f "$pattern" >/dev/null 2>&1 || return 0  # nessun processo: nulla da fare
    pkill -TERM -f "$pattern" 2>/dev/null || true
    local i=0
    while pgrep -f "$pattern" >/dev/null 2>&1; do
        i=$((i + 1))
        if [ "$i" -gt 30 ]; then  # ~15s di graceful, poi forza
            log "stop ${label}: ancora vivo dopo 15s -> SIGKILL"
            pkill -KILL -f "$pattern" 2>/dev/null || true
            sleep 1
            break
        fi
        sleep 0.5
    done
}

# Unit systemd --user dei servizi del meta-progetto (auto-restart on-failure,
# install: deploy/install-nexus-units.sh). Se per <name> esiste
# nexus-<name>.service, start/stop DEVONO passare da systemctl: un pkill
# diretto sul processo gestito dalla unit verrebbe rilanciato da Restart=
# creando doppia istanza sulla porta.
_service_unit_installed() {
    systemctl --user cat "nexus-$1.service" >/dev/null 2>&1
}

stop_service() {
    local name="$1"
    if _service_unit_installed "$name"; then
        systemctl --user stop "nexus-${name}.service" 2>/dev/null || true
    fi
    # Il pattern-kill sotto resta come defense-in-depth contro processi nohup
    # legacy residui (transizione pre-unit). Innocuo se la unit e' gia' ferma.
    # Root cause (regola H): `pkill -f "$name"` su nudo nome di servizio matcha
    # anche la command line dello script stesso quando invocato come
    # `deploy-local.sh --service mcp-core` (l'argomento "mcp-core" e' presente
    # nella riga di comando di bash/wsl che ospita lo script). Risultato: lo
    # script si auto-uccideva con SIGTERM (exit 15) prima di buildare/riavviare.
    # Colpiva solo i servizi con nome == argomento (mcp-core, admin-service, ...);
    # il brain era immune perche' il suo pattern e' 'brain.grpc_server.main'.
    #
    # Fix: matchare il suffisso del PATH del binario eseguibile
    # (`target/<profilo>/<nome>`), che non compare mai nella command line dello
    # script. Il pattern e' indipendente da:
    #   - modo di invocazione: assoluto (start_service usa ${BIN_DIR}/${name})
    #     o relativo (es. ./target/release/mcp-core avviato da Start-Dev.ps1);
    #   - profilo (debug|release): cosi' un restart in release ferma comunque un
    #     vecchio processo debug ancora attaccato alla porta, e viceversa.
    # L'ancoraggio finale ([[:space:]]|$) evita match parziali (es. mcp-core-x).
    # Coperto anche il fallback `cargo run -p ${name}` (binario non ancora buildato).
    #
    # NB: lo script gira con `set -euo pipefail`. `pkill` ritorna 1 quando NON
    # trova processi (caso normale: servizio gia' fermo) -> senza `|| true` il
    # `set -e` farebbe terminare lo script proprio qui. Il `|| true` e' quindi
    # obbligatorio, non cosmetico.
    _stop_pattern "target/(debug|release)/${name}([[:space:]]|\$)" "$name"
    _stop_pattern "cargo run -p ${name}" "${name} (cargo)"
    return 0
}

start_service() {
    local name="$1"
    shift
    local bin="${BIN_DIR}/${name}"
    local logfile="/tmp/nexus-${name}.log"
    # Ramo systemd: aggiorna il symlink stabile al binario del profilo corrente
    # (debug|release) e riavvia la unit. NB: le eventuali env extra ($@) NON
    # vengono propagate qui — una unit che ne richiede deve dichiararle nel
    # proprio file .service (oggi nessuna unit installata le richiede).
    # ADR 0028 L3: se la unit --system e' installata (deploy/install-system-units.sh)
    # ha la precedenza — i servizi core girano sotto PID 1, immuni dalla caduta del
    # manager --user in WSL. Restart via sudo (puo' chiedere la password).
    if [ -f "$bin" ] && [ -f "/etc/systemd/system/nexus-${name}.service" ]; then
        mkdir -p "${ROOT}/target/nexus-current"
        ln -sfn "$bin" "${ROOT}/target/nexus-current/${name}"
        sudo systemctl restart "nexus-${name}.service"
        echo "  ${name} via systemd --system nexus-${name}.service (PID1, ADR 0028 L3) log=${logfile}"
        return
    fi
    if [ -f "$bin" ] && _service_unit_installed "$name"; then
        mkdir -p "${ROOT}/target/nexus-current"
        ln -sfn "$bin" "${ROOT}/target/nexus-current/${name}"
        systemctl --user restart "nexus-${name}.service"
        echo "  ${name} via systemd nexus-${name}.service (auto-restart on-failure) log=${logfile}"
        return
    fi
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
    # Nessun supervisor esterno per mcp-core (rimosso 2026-06-07): era una toppa
    # al sintomo. La causa dei crash e' affrontata alla radice — isolamento
    # process-group di tutti gli spawn (un kill di un processo di progetto non
    # puo' risalire a mcp-core) + logging del mittente SIGTERM per diagnosi. In
    # produzione il restart spetta a systemd (Restart=always).
}

# Mappa dei servizi con eventuali env var extra necessarie all'avvio.
# Formato: "nome:VAR1=val1:VAR2=val2" — le coppie dopo il primo ":" sono env var.
# NB: nessuna porta qui (regola G): browser-bridge-mcp risolve browser_bridge_port
# dal DB all'avvio, come tutti gli altri servizi.
declare -A SERVICE_ENV

start_service_with_env() {
    local name="$1"
    local extra_env="${SERVICE_ENV[$name]:-}"
    if [ -n "$extra_env" ]; then
        start_service "$name" "$extra_env"
    else
        start_service "$name"
    fi
}

# Unit systemd --user del brain (auto-restart on-failure). Se installata
# (deploy/install-brain-unit.sh), start/stop DEVONO passare da systemctl:
# un pkill diretto sul processo gestito dalla unit verrebbe rilanciato da
# Restart= entro 5s, creando doppia istanza sulla porta col nohup successivo.
_brain_unit_installed() {
    systemctl --user cat nexus-brain.service >/dev/null 2>&1
}

stop_brain() {
    if _brain_unit_installed; then
        systemctl --user stop nexus-brain.service 2>/dev/null || true
    fi
    # Defense-in-depth: ferma anche eventuali processi nohup legacy residui
    # (transizione pre-unit). Innocuo se la unit e' gia' stoppata.
    _stop_pattern 'brain.grpc_server.main' 'brain'
}

start_brain() {
    local logfile="/tmp/nexus-neural.log"
    # ADR 0028 L3: unit --system ha la precedenza (vedi start dei servizi Rust).
    if [ -f "/etc/systemd/system/nexus-brain.service" ]; then
        sudo systemctl restart nexus-brain.service
        echo "  brain via systemd --system nexus-brain.service (PID1, ADR 0028 L3) log=${logfile}"
        return
    fi
    if _brain_unit_installed; then
        systemctl --user restart nexus-brain.service
        echo "  brain via systemd nexus-brain.service (auto-restart on-failure) log=${logfile}"
        return
    fi
    setsid nohup env \
        DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}" \
        HF_HUB_OFFLINE=1 \
        TRANSFORMERS_OFFLINE=1 \
        python3 -m brain.grpc_server.main --rest \
        > "$logfile" 2>&1 < /dev/null &
    local pid=$!
    disown || true
    echo "  brain PID=${pid} log=${logfile} (nohup legacy: installa l'auto-restart con deploy/install-brain-unit.sh)"
}

stop_gateway() {
    pkill -f 'apps/nexus-gateway/dist/server\.js' 2>/dev/null || true
    sleep 1
}

build_gateway() {
    # Il gateway e' TypeScript: senza questo build dist/server.js non esiste e
    # start_gateway fallisce con "file non trovato". turbo builda anche le
    # dipendenze workspace (@nexus/shared, @nexus/llm-gateway, @nexus/audit).
    log "Build nexus-gateway (TypeScript)..."
    pnpm exec turbo run build --filter=@ideai/nexus-gateway-server
}

start_gateway() {
    local logfile="/tmp/nexus-gateway.log"
    # Niente NEXUS_GATEWAY_PORT (regola G): il gateway risolve nexus_gateway_port
    # dal DB all'avvio. Qui solo le credenziali DB e i file di config.
    setsid nohup env \
        NODE_ENV=production \
        DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}" \
        POSTGRES_URL="${POSTGRES_URL:-${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}}" \
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
    # ADR 0028 L3: se gestito da systemd --system, fermalo via systemctl —
    # un pkill verrebbe rilanciato da Restart=on-failure entro pochi secondi
    # (doppia istanza sulla porta col restart successivo).
    if [ -f "/etc/systemd/system/nexus-web-ide.service" ]; then
        sudo systemctl stop nexus-web-ide.service 2>/dev/null || true
    fi
    # Defense-in-depth: residui nohup legacy. Fix M49: il pattern "server\.js"
    # matchava ANCHE il nexus-gateway (node apps/nexus-gateway/dist/server.js),
    # causando il kill collaterale del gateway a ogni rebuild --web. Match
    # ristretto al solo binary del web-ide (apps/web-ide/server.js).
    pkill -f "apps/web-ide/server\.js" 2>/dev/null || true
    pkill -f "next-server" 2>/dev/null || true
    pkill -f "next start"  2>/dev/null || true
    sleep 1
}

start_webide() {
    local logfile="/tmp/nexus-webide.log"
    # ADR 0028 L3: unit --system ha la precedenza (come Rust e brain). Avviata da
    # PID1 (root), systemd apre il log in append COME root e passa il fd al
    # processo (User=): scrive su /tmp/nexus-webide.log anche se il file e' di
    # proprieta' root, risolvendo il "Permission denied" del nohup --user.
    if [ -f "/etc/systemd/system/nexus-web-ide.service" ]; then
        sudo systemctl restart nexus-web-ide.service
        echo "  web-ide via systemd --system nexus-web-ide.service (PID1, ADR 0028 L3) log=${logfile}"
        return
    fi
    if systemctl --user cat nexus-web-ide.service >/dev/null 2>&1; then
        systemctl --user restart nexus-web-ide.service
        echo "  web-ide via systemd nexus-web-ide.service (auto-restart on-failure) log=${logfile}"
        return
    fi
    setsid nohup env NODE_ENV=production node "${ROOT}/apps/web-ide/server.js" \
        > "$logfile" 2>&1 < /dev/null &
    local pid=$!
    disown || true
    echo "  web-ide PID=${pid} log=${logfile} (nohup legacy: installa l'auto-restart con deploy/install-system-units.sh)"
}

build_webide() {
    log "Build web-ide (Next.js)..."
    cd "${ROOT}/apps/web-ide"
    # Purge cache CONDIZIONALE (--clean). La purge incondizionata era una toppa:
    # serviva solo dopo merge/stash, quando una `.next/` con stato inconsistente
    # includeva chunks vecchi (es. dispatcher SSE escluso per tree-shaking
    # errato). Nel deploy quotidiano la cache di Next/Turbo e' valida e
    # riusarla taglia minuti dal rebuild. Forzare la pulizia con --clean.
    if [ "${CLEAN_BUILD:-false}" = true ]; then
        log "  (--clean) rimozione cache .next/.turbo"
        rm -rf .next .turbo
    fi
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
            build_gateway
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
# I servizi Rust si fermano con stop_service (pattern target/<profilo>/<nome>).
# brain e gateway NON sono binari Rust: vanno fermati con le loro funzioni
# dedicate (pattern 'brain.grpc_server.main' / 'dist/server.js'), altrimenti
# stop_service non li matcha e restano processi orfani (doppio brain).
for svc in mcp-core admin-service chat-service billing-service doc-service plugin-service browser-bridge-mcp; do
    stop_service "$svc"
done
stop_brain
stop_gateway
stop_webide
sleep 2

log "Avvio Neural Core (Python, porte dal DB)..."
start_brain
sleep 4

log "Avvio Nexus Gateway (Node.js, porta dal DB)..."
build_gateway
start_gateway
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
log "Health check (porte risolte dal DB - regola G):"
# Coppie chiave_settings:nome. La porta viene dal DB, stessa fonte di verita'
# usata dai servizi: niente liste di porte hardcoded nello script.
for entry in \
    "web_ide_port:web-ide" \
    "mcp_core_http_port:mcp-core" \
    "admin_service_port:admin-service" \
    "chat_service_port:chat-service" \
    "doc_service_port:doc-service" \
    "billing_service_port:billing-service" \
    "plugin_service_port:plugin-service" \
    "browser_bridge_port:browser-bridge" \
    "nexus_gateway_port:nexus-gateway" \
    "brain_rest_port:brain"; do
    key="${entry%%:*}"; name="${entry#*:}"
    port="$(_db_port "$key")"
    if [ -z "$port" ]; then
        printf "  %-16s porta non leggibile dal DB (%s)\n" "$name" "$key"
        continue
    fi
    # web-ide non espone /health: prova la root.
    if [ "$key" = "web_ide_port" ]; then
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/" 2>/dev/null || echo "down")
    else
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health" 2>/dev/null || echo "down")
    fi
    if [ "$code" = "200" ]; then
        printf "  :%-5s %-16s OK\n" "$port" "$name"
    else
        printf "  :%-5s %-16s %s\n" "$port" "$name" "$code"
    fi
done

echo ""
log "Deploy locale completato."
