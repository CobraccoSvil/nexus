#!/usr/bin/env bash
# deploy-nexus.sh — Deploy Nexus su server-prod.
# Compatibile Windows (Git Bash/WSL) — usa git archive + scp invece di rsync.
# Uso: bash scripts/deploy-nexus.sh [--rust-only | --web-only | --full]
set -euo pipefail

REMOTE="${DEPLOY_HOST:-administrator@nexus-prod}"
DEPLOY_DIR="/opt/ai-orchestrator"
MCP_BIN="$DEPLOY_DIR/target/release/mcp-core"
MCP_LOG="/tmp/mcp.log"
HEALTH_URL="http://127.0.0.1:4000/api/health"
VERSION_URL="http://127.0.0.1:3000/nexus/version"
MODE="${1:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[deploy]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
err()  { echo -e "${RED}[error]${NC} $*"; exit 1; }
info() { echo -e "${BLUE}[info]${NC} $*"; }

# ── 0. Mostra versioni correnti in esecuzione ─────────────────────────────────
show_current_versions() {
    info "Versioni attualmente in esecuzione su server-prod:"

    local mcp_build web_build web_date
    mcp_build=$(ssh "$REMOTE" \
        "curl -fsS $HEALTH_URL 2>/dev/null | grep -o '\"build_time\":\"[^\"]*\"' | cut -d'\"' -f4 || echo '?'" \
        2>/dev/null || echo "?")
    web_build=$(ssh "$REMOTE" \
        "curl -fsS $VERSION_URL 2>/dev/null | grep -o '\"buildId\":\"[^\"]*\"' | cut -d'\"' -f4 || echo '?'" \
        2>/dev/null || echo "?")
    web_date=$(ssh "$REMOTE" \
        "curl -fsS $VERSION_URL 2>/dev/null | grep -o '\"buildDate\":\"[^\"]*\"' | cut -d'\"' -f4 || echo '?'" \
        2>/dev/null || echo "?")

    echo "  Backend  (mcp-core) : build_time = ${mcp_build:-?}"
    echo "  Frontend (web-ide)  : buildId    = ${web_build:-?}"
    if [ -n "$web_date" ] && [ "$web_date" != "?" ] && [ "$web_date" != "null" ]; then
        echo "                        built at   = $web_date"
    fi
    echo
}

# ── 1. Sync sorgenti via git archive + tar per modifiche uncommitted ──────────
# Non richiede rsync — funziona su Windows (Git Bash / WSL).
# Usa sed 's/\r//' per eliminare i CRLF che git produce su Windows.
sync_sources() {
    log "Sincronizzazione sorgenti → server-prod..."
    cd "$REPO_ROOT"

    ssh "$REMOTE" "mkdir -p '$DEPLOY_DIR'"

    # [1/2] Tutti i file committati in HEAD via git archive
    log "  [1/2] File committati (git archive | ssh tar)..."
    git archive HEAD --format=tar | ssh "$REMOTE" "tar xf - -C '$DEPLOY_DIR'"

    # [2/2] File modificati rispetto a HEAD + file non-tracciati
    #        git diff HEAD  → file modificati (staged o unstaged, sia nuovi che vecchi)
    #        ls-files --others → file non ancora committati (untracked, non in .gitignore)
    #        sed 's/\r//'  → rimuove \r dai path (git su Windows produce CRLF)
    local extra_files
    extra_files=$(
        { git diff HEAD --name-only 2>/dev/null; git ls-files --others --exclude-standard 2>/dev/null; } \
        | sed 's/\r//' \
        | grep -vE '\.(env|key|pem|log)$|nexus\.env|^\.env' \
        || true
    )

    if [ -z "$extra_files" ]; then
        log "  [2/2] Nessun file extra da sincronizzare."
    else
        # Filtra solo file esistenti sul disco locale
        local tar_args=()
        while IFS= read -r f; do
            [ -n "$f" ] || continue
            [ -f "$f" ] || continue
            tar_args+=("$f")
        done <<< "$extra_files"

        if [ ${#tar_args[@]} -eq 0 ]; then
            log "  [2/2] Nessun file extra valido."
        else
            log "  [2/2] File modificati/nuovi: ${#tar_args[@]} file..."
            printf '%s\n' "${tar_args[@]}"
            tar czf - "${tar_args[@]}" 2>/dev/null | ssh "$REMOTE" "tar xzf - -C '$DEPLOY_DIR'"
        fi
    fi

    # Normalizza CRLF→LF sui file .sql (git archive su Windows produce CRLF,
    # ma sqlx controlla i checksum e li vuole LF)
    ssh "$REMOTE" "
        find '$DEPLOY_DIR/db/migrations' -name '*.sql' -exec sed -i 's/\r//' {} +
    " 2>/dev/null || true

    log "Sync completato."
}

# ── 2. Build Rust sul server ──────────────────────────────────────────────────
build_rust() {
    log "Build Rust su server-prod (può richiedere 2-5 min)..."
    local build_start
    build_start=$(date +%s)
    ssh "$REMOTE" "
        echo '$build_start' > $DEPLOY_DIR/.last_build_ts
        cd $DEPLOY_DIR && ~/.cargo/bin/cargo build -p mcp-core --release 2>&1
    " | grep -E 'error\[|Compiling mcp-core|Finished|warning.*unused.*variable' || true
    log "Build Rust completato."
}

# ── 3. Build frontend ─────────────────────────────────────────────────────────
build_web() {
    log "Build frontend su server-prod..."
    ssh "$REMOTE" "
        set -euo pipefail
        cd $DEPLOY_DIR
        source .env 2>/dev/null || true
        pnpm install --frozen-lockfile --silent 2>&1 | tail -3
        pnpm --filter @ai-orchestrator/web-ide build 2>&1 | tail -10
    "
    log "Build frontend completato."
}

# ── 4. Swap del backend (breve gap, non zero-downtime puro) ───────────────────
# Nota: hyper/axum non usa SO_REUSEPORT, quindi il nuovo processo non può
# co-listenere sulla stessa porta. L'ordine è: stop old → attendi porta libera
# → start new → verifica. Nginx davanti assorbe il gap (~1s) con retry.
swap_backend() {
    log "Swap backend..."
    ssh "$REMOTE" "
        set -euo pipefail

        # pgrep -x matcha esattamente il nome del processo 'mcp-core' e NON
        # include eventuali shell parent/script che contengono la stringa
        # 'target/release/mcp-core' nella command line.
        OLD_PIDS=\$(pgrep -x mcp-core || true)

        if [ -n \"\$OLD_PIDS\" ]; then
            echo \"  Stop vecchi processi mcp-core: \$OLD_PIDS\"
            kill -TERM \$OLD_PIDS 2>/dev/null || true
            for i in \$(seq 1 10); do
                sleep 0.5
                if [ -z \"\$(pgrep -x mcp-core || true)\" ]; then break; fi
            done
            REMAINING=\$(pgrep -x mcp-core || true)
            if [ -n \"\$REMAINING\" ]; then
                echo \"  SIGTERM non rispettato, SIGKILL su \$REMAINING...\"
                kill -9 \$REMAINING 2>/dev/null || true
                sleep 1
            fi
        fi

        # Attendi che la porta 4000 sia libera (max 5s)
        for i in \$(seq 1 10); do
            if ! ss -tln 'sport = :4000' 2>/dev/null | tail -n +2 | grep -q .; then break; fi
            sleep 0.5
        done

        cd $DEPLOY_DIR
        nohup ./target/release/mcp-core > $MCP_LOG 2>&1 &
        NEW_PID=\$!
        disown \$NEW_PID 2>/dev/null || true
        echo \"  Nuovo processo avviato: PID \$NEW_PID\"

        # Attesa readiness: il processo deve essere vivo e rispondere sull'health endpoint
        for i in \$(seq 1 30); do
            sleep 1
            if ! kill -0 \$NEW_PID 2>/dev/null; then
                echo '  ✗ Nuovo processo morto prematuramente — ultimi log:'
                tail -20 $MCP_LOG
                exit 1
            fi
            BUILD=\$(curl -fsS $HEALTH_URL 2>/dev/null | grep -o '\"build_time\":\"[^\"]*\"' | cut -d'\"' -f4 || echo '')
            if [ -n \"\$BUILD\" ]; then
                echo \"  ✓ Nuovo backend attivo (build_time=\$BUILD)\"
                exit 0
            fi
        done

        echo '  ✗ Nuovo processo vivo ma non risponde sulla porta 4000 dopo 30s'
        tail -20 $MCP_LOG
        exit 1
    "
}

# ── 5. Restart web frontend (con attesa readiness) ────────────────────────────
restart_web() {
    log "Riavvio frontend..."
    ssh "$REMOTE" "
        cd $DEPLOY_DIR && bash scripts/dev-server-101.sh restart-web 2>&1
    "
}

# ── 6. Verifica deploy completa ───────────────────────────────────────────────
verify() {
    log "Verifica deploy..."

    # Backend
    local mcp_build expected_build
    mcp_build=$(ssh "$REMOTE" \
        "curl -fsS $HEALTH_URL 2>/dev/null | grep -o '\"build_time\":\"[^\"]*\"' | cut -d'\"' -f4 || echo ''" \
        2>/dev/null || echo "")
    expected_build=$(ssh "$REMOTE" \
        "cat $DEPLOY_DIR/.last_build_ts 2>/dev/null || echo 0" \
        2>/dev/null || echo "0")

    if [ -z "$mcp_build" ]; then
        warn "⚠ Backend non risponde sulla porta 4000"
    elif [ "$mcp_build" -ge "$expected_build" ] 2>/dev/null; then
        log "✓ Backend OK  (build_time=$mcp_build)"
    else
        warn "⚠ Backend: possibile versione vecchia (build_time=$mcp_build, atteso>=$expected_build)"
    fi

    # Frontend
    local web_build web_date
    web_build=$(ssh "$REMOTE" \
        "curl -fsS $VERSION_URL 2>/dev/null | grep -o '\"buildId\":\"[^\"]*\"' | cut -d'\"' -f4 || echo ''" \
        2>/dev/null || echo "")
    web_date=$(ssh "$REMOTE" \
        "curl -fsS $VERSION_URL 2>/dev/null | grep -o '\"buildDate\":\"[^\"]*\"' | cut -d'\"' -f4 || echo ''" \
        2>/dev/null || echo "")

    if [ -z "$web_build" ] || [ "$web_build" = "unknown" ]; then
        warn "⚠ Frontend non risponde sulla porta 3000"
    else
        log "✓ Frontend OK"
        echo "     buildId  = $web_build"
        [ -n "$web_date" ] && [ "$web_date" != "null" ] && echo "     built at = $web_date"
    fi

    echo
    info "Processi attivi:"
    ssh "$REMOTE" "ss -tlnp | grep -E '3000|4000'" 2>/dev/null || true
}

# ── 6b. Smoke test NexusBridge (4 endpoint) ───────────────────────────────────
verify_nexus() {
    log "Smoke test NexusBridge..."
    # Esegue lo script remoto sincronizzato da sync_sources.
    # Lo script esce con 0 se tutti gli endpoint rispondono, 1 altrimenti.
    if ssh "$REMOTE" "bash $DEPLOY_DIR/scripts/nexus-smoke-test.sh localhost:4000" 2>&1; then
        log "✓ Nexus endpoints OK"
    else
        warn "⚠ Nexus endpoints degradati — vedi output sopra"
        warn "   Se /nexus/healthz → 503, NexusBridge::init_global() non è stato"
        warn "   invocato al boot. Controlla i log: ssh $REMOTE 'tail -50 $MCP_LOG'"
    fi
}

# ── 7. Controlla setup iniziale (dipendenze sistema) ──────────────────────────
check_setup() {
    local missing
    missing=$(ssh "$REMOTE" "
        dpkg -l libatk1.0-0 libgbm1 libasound2 2>/dev/null | grep -c '^ii' | grep -q '^3$' || echo 'missing'
    " 2>/dev/null || true)

    if [ -n "$missing" ]; then
        warn "╔══════════════════════════════════════════════════════════════╗"
        warn "║  SETUP INIZIALE MANCANTE — esegui una volta come root:      ║"
        warn "║  ssh -t $REMOTE \\                                           ║"
        warn "║    \"sudo bash $DEPLOY_DIR/scripts/dev-server-101.sh setup\"  ║"
        warn "╚══════════════════════════════════════════════════════════════╝"
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────────
show_current_versions

case "$MODE" in
    --rust-only)
        sync_sources
        build_rust
        swap_backend
        verify
        verify_nexus
        check_setup
        ;;
    --web-only)
        sync_sources
        build_web
        restart_web
        verify
        check_setup
        ;;
    --full | "")
        sync_sources
        build_rust
        build_web
        swap_backend
        restart_web
        verify
        verify_nexus
        check_setup
        ;;
    *)
        echo "Uso: $0 [--rust-only | --web-only | --full]"
        exit 1
        ;;
esac

log "Deploy completato."
