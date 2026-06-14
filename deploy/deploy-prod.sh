#!/usr/bin/env bash
# deploy/deploy-prod.sh - Update incrementale di Nexus su PROD_HOST.
#
# Flusso:
#   1. Pre-check (working tree, branch)
#   2. Acquisisci lock su PROD_HOST
#   3. Sync sorgenti via git archive
#   4. Backup binari correnti in bin/.previous/
#   5. Build selettivo (Rust / web-ide / brain / nexus-gateway)
#   6. Restart systemd selettivo
#   7. Health check -- se fallisce: rollback binari + restart
#   8. Log a /var/log/ideai/deploy.log
#
# Flag (cumulabili):
#   --rust       Rebuild Rust (mcp-core + microservizi)
#   --web        Rebuild web-ide (Next.js)
#   --brain      Pip install brain (Python)
#   --gateway    Rebuild nexus-gateway (Rust)
#   --all        Tutti i precedenti
#
# Altre opzioni:
#   --no-build         Solo restart, no build
#   --no-health        Skip health-check finale
#   --allow-dirty      Permette working tree sporco
#   --branch <name>    Deploya un branch diverso da main
#   --first-build      Skip git pull (usato da bootstrap-prod.sh)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/remote.sh
. "$SCRIPT_DIR/lib/remote.sh"

# Flag defaults
BUILD_RUST=0
BUILD_WEB=0
BUILD_BRAIN=0
BUILD_GATEWAY=0
SKIP_BUILD=0
SKIP_HEALTH=0
FIRST_BUILD=0
BRANCH="main"

while [ $# -gt 0 ]; do
    case "$1" in
        --rust)         BUILD_RUST=1 ;;
        --web)          BUILD_WEB=1 ;;
        --brain)        BUILD_BRAIN=1 ;;
        --gateway)      BUILD_GATEWAY=1 ;;
        --all)          BUILD_RUST=1; BUILD_WEB=1; BUILD_BRAIN=1; BUILD_GATEWAY=1 ;;
        --no-build)     SKIP_BUILD=1 ;;
        --no-health)    SKIP_HEALTH=1 ;;
        --allow-dirty)  export ALLOW_DIRTY=1 ;;
        --first-build)  FIRST_BUILD=1 ;;
        --branch)       shift; BRANCH="$1" ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -30
            exit 0 ;;
        *) die "Flag sconosciuto: $1" ;;
    esac
    shift
done

# Default: --all se nessun flag specificato
if [ "$SKIP_BUILD" -eq 0 ] \
   && [ "$BUILD_RUST" -eq 0 ] && [ "$BUILD_WEB" -eq 0 ] \
   && [ "$BUILD_BRAIN" -eq 0 ] && [ "$BUILD_GATEWAY" -eq 0 ]; then
    BUILD_RUST=1; BUILD_WEB=1; BUILD_BRAIN=1; BUILD_GATEWAY=1
fi

print_header "Deploy Nexus -> $PROD_HOST (branch: $BRANCH)"

# === Pre-check ===============================================================
remote_check_reachable "$PROD_HOST" || die "SSH a $PROD_HOST non raggiungibile"
require_clean_tree
COMMIT="$(commit_hash)"
info "HEAD = $COMMIT, target = ${DEPLOY_DIR}"

# Lock deploy (acquisito ed esteso per tutta la durata via flock --close)
LOCKFILE="/tmp/ideai-deploy.lock"

# Avvolgiamo il deploy in un unico SSH che mantiene il lock.
# Strategia: prepariamo le variabili in locale, sincronizziamo i sorgenti,
# poi eseguiamo build+restart in un singolo blocco SSH sotto flock.

# === Sync sorgenti ===========================================================
if [ "$FIRST_BUILD" -eq 0 ]; then
    sync_sources "$PROD_HOST" "$DEPLOY_DIR"
else
    info "First-build mode: skip sync (gia' fatto da bootstrap)"
fi

# === Build + restart sotto lock ==============================================
# Flag remoti
R_RUST=$BUILD_RUST
R_WEB=$BUILD_WEB
R_BRAIN=$BUILD_BRAIN
R_GATEWAY=$BUILD_GATEWAY
R_SKIP_BUILD=$SKIP_BUILD

# Lista unit systemd da riavviare in base ai flag
units_to_restart=""
[ "$BUILD_RUST" -eq 1 ] && units_to_restart="$units_to_restart nexus-core nexus-admin nexus-chat nexus-billing nexus-docs nexus-plugins"
[ "$BUILD_WEB" -eq 1 ]  && units_to_restart="$units_to_restart nexus-webide"
[ "$BUILD_BRAIN" -eq 1 ] && units_to_restart="$units_to_restart nexus-neural"
[ "$BUILD_GATEWAY" -eq 1 ] && units_to_restart="$units_to_restart nexus-gateway"

log "Acquisisco lock $LOCKFILE su $PROD_HOST"
log "Build e restart in corso..."

REMOTE_SCRIPT=$(cat <<'REMOTE_EOF'
set -euo pipefail
DEPLOY_DIR="__DEPLOY_DIR__"
COMMIT="__COMMIT__"
R_RUST=__R_RUST__
R_WEB=__R_WEB__
R_BRAIN=__R_BRAIN__
R_GATEWAY=__R_GATEWAY__
R_SKIP_BUILD=__R_SKIP_BUILD__
UNITS_TO_RESTART="__UNITS__"

cd "$DEPLOY_DIR"
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"

# Backup binari correnti (rollback automatico in caso di failure)
mkdir -p bin bin/.previous
if [ -f bin/mcp-core ]; then
    cp -f bin/* bin/.previous/ 2>/dev/null || true
fi

BUILD_START=$(date +%s)
echo "$BUILD_START" > .last_build_ts

# === Build Rust ===
if [ "$R_RUST" -eq 1 ] && [ "$R_SKIP_BUILD" -eq 0 ]; then
    echo "[build] cargo build --release -p mcp-core ..."
    ~/.cargo/bin/cargo build --release -p mcp-core 2>&1 | tail -5
    cp -f target/release/mcp-core bin/
    # Microservizi attivi (se i pacchetti esistono nel workspace)
    for pkg in admin-service chat-service billing-service doc-service plugin-service; do
        if [ -f "target/release/$pkg" ]; then
            cp -f "target/release/$pkg" bin/
        fi
    done
fi

# === Build Web ===
if [ "$R_WEB" -eq 1 ] && [ "$R_SKIP_BUILD" -eq 0 ]; then
    echo "[build] pnpm install + build web-ide ..."
    pnpm install --frozen-lockfile --silent 2>&1 | tail -3
    pnpm --filter @ai-orchestrator/web-ide build 2>&1 | tail -5
fi

# === Build Gateway (Rust) ===
# Migrazione Fase 6: il gateway e' il binario Rust del crate nexus-gateway. Il
# vecchio server Node (apps/nexus-gateway) e' stato eliminato.
if [ "$R_GATEWAY" -eq 1 ] && [ "$R_SKIP_BUILD" -eq 0 ]; then
    echo "[build] cargo build --release -p nexus-gateway ..."
    ~/.cargo/bin/cargo build --release -p nexus-gateway --bin nexus-gateway 2>&1 | tail -5
    cp -f target/release/nexus-gateway bin/
fi

# === Build Brain (Python) ===
if [ "$R_BRAIN" -eq 1 ] && [ "$R_SKIP_BUILD" -eq 0 ]; then
    echo "[build] pip install -e brain[all] ..."
    "$DEPLOY_DIR/.venv/bin/pip" install -q -e 'brain[all]' 2>&1 | tail -3 || true
fi

# === Restart systemd ===
echo "[restart] units: $UNITS_TO_RESTART"
for unit in $UNITS_TO_RESTART; do
    if systemctl list-unit-files | grep -q "^${unit}.service"; then
        sudo systemctl restart "$unit"
        echo "  $unit restarted"
    fi
done

# === Log deploy ===
sudo mkdir -p /var/log/ideai
echo "$(date -u +%FT%TZ) commit=$COMMIT rust=$R_RUST web=$R_WEB brain=$R_BRAIN gateway=$R_GATEWAY units=\"$UNITS_TO_RESTART\"" \
    | sudo tee -a /var/log/ideai/deploy.log >/dev/null

echo "[done] deploy commit=$COMMIT in $(( $(date +%s) - BUILD_START ))s"
REMOTE_EOF
)

REMOTE_SCRIPT="${REMOTE_SCRIPT//__DEPLOY_DIR__/$DEPLOY_DIR}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__COMMIT__/$COMMIT}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__R_RUST__/$R_RUST}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__R_WEB__/$R_WEB}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__R_BRAIN__/$R_BRAIN}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__R_GATEWAY__/$R_GATEWAY}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__R_SKIP_BUILD__/$R_SKIP_BUILD}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__UNITS__/$units_to_restart}"

# Esecuzione sotto flock (exit 11 se gia' detenuto)
set +e
ssh $SSH_OPTS "${SSH_USER}@${PROD_HOST}" \
    "flock --nonblock --close --conflict-exit-code 11 '$LOCKFILE' bash -s" <<< "$REMOTE_SCRIPT"
DEPLOY_RC=$?
set -e

if [ "$DEPLOY_RC" -eq 11 ]; then
    die "Lock $LOCKFILE detenuto su $PROD_HOST (altro deploy in corso?)"
elif [ "$DEPLOY_RC" -ne 0 ]; then
    err "Deploy fallito (rc=$DEPLOY_RC). Avvio rollback..."
    remote_exec "$PROD_HOST" "
        cd $DEPLOY_DIR
        if [ -d bin/.previous ] && [ -n \"\$(ls -A bin/.previous 2>/dev/null)\" ]; then
            cp -f bin/.previous/* bin/
            for unit in $units_to_restart; do
                sudo systemctl restart \$unit 2>/dev/null || true
            done
            echo 'Rollback binari completato'
        fi
    " || true
    exit 1
fi

# === Health check ============================================================
if [ "$SKIP_HEALTH" -eq 1 ]; then
    warn "--no-health: skip health check"
    log "Deploy commit=$COMMIT completato (senza health check)"
    exit 0
fi

info "Attendo 5s readiness servizi..."
sleep 5

if "$SCRIPT_DIR/health-check.sh"; then
    print_header "DEPLOY OK -- commit $COMMIT"
    exit 0
else
    err "Health check fallito dopo deploy. Avvio rollback..."
    remote_exec "$PROD_HOST" "
        cd $DEPLOY_DIR
        if [ -d bin/.previous ] && [ -n \"\$(ls -A bin/.previous 2>/dev/null)\" ]; then
            cp -f bin/.previous/* bin/
            for unit in $units_to_restart; do
                sudo systemctl restart \$unit 2>/dev/null || true
            done
        fi
    "
    sleep 5
    err "Rollback completato. Stato post-rollback:"
    "$SCRIPT_DIR/health-check.sh" || true
    exit 1
fi
