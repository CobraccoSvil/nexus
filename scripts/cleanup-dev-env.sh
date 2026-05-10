#!/usr/bin/env bash
#
# cleanup-dev-env.sh — Pulizia dell'ambiente di sviluppo IDEAI/Nexus.
#
# Esegue, con conferma esplicita per ogni passo distruttivo:
#   1. Backup obbligatorio del DB Nexus (container ideai-postgres-nexus-1)
#   2. Conferma generale di procedere
#   3. Rimozione container Docker orfano (freelance-postgres-dev)
#   4. Rimozione worktree git Windows prunable
#   5. cargo clean del workspace Rust (recupera ~18 GB)
#   6. Reinstall pulito di node_modules
#   7. Disinstallazione Postgres nativo Linux (cluster 16)
#   8. Verifica finale
#
# Idempotente: ogni passo è no-op se già completato.
# Fail-fast: termina al primo errore irrecuperabile.
# Da eseguire come utente `administrator` su WSL Ubuntu.
#
# Uso: bash scripts/cleanup-dev-env.sh

set -euo pipefail

# ── Costanti ────────────────────────────────────────────────────────────────
readonly REPO_ROOT="/home/administrator/ideai"
readonly BACKUP_DIR="/home/administrator/backups"
readonly NEXUS_PG_CONTAINER="ideai-postgres-nexus-1"
readonly ORPHAN_CONTAINER="freelance-postgres-dev"
readonly WORKTREE_WINDOWS_PATH="D:/Sviluppo/ideai/recursing-poincare-2a5ee2"
readonly TS="$(date +%Y%m%d-%H%M%S)"
readonly LOG_FILE="${BACKUP_DIR}/cleanup-${TS}.log"
readonly BACKUP_FILE="${BACKUP_DIR}/nexus-${TS}.sql.gz"

# Colori per output (disabilitati se non TTY)
if [ -t 1 ]; then
    readonly C_RED=$'\e[31m'
    readonly C_GREEN=$'\e[32m'
    readonly C_YELLOW=$'\e[33m'
    readonly C_BLUE=$'\e[34m'
    readonly C_BOLD=$'\e[1m'
    readonly C_RESET=$'\e[0m'
else
    readonly C_RED=""
    readonly C_GREEN=""
    readonly C_YELLOW=""
    readonly C_BLUE=""
    readonly C_BOLD=""
    readonly C_RESET=""
fi

# ── Helper logging ───────────────────────────────────────────────────────────
log()  { printf '%s[%s]%s %s\n' "$C_BLUE" "$(date +%H:%M:%S)" "$C_RESET" "$*" | tee -a "$LOG_FILE"; }
ok()   { printf '%s[OK]%s %s\n' "$C_GREEN" "$C_RESET" "$*" | tee -a "$LOG_FILE"; }
warn() { printf '%s[WARN]%s %s\n' "$C_YELLOW" "$C_RESET" "$*" | tee -a "$LOG_FILE"; }
err()  { printf '%s[ERR]%s %s\n' "$C_RED" "$C_RESET" "$*" | tee -a "$LOG_FILE" >&2; }

# Conferma [y/N] con default no. Ritorna 0 se sì, 1 se no/altro.
confirm() {
    local prompt="$1"
    local reply
    printf '%s%s [y/N]:%s ' "$C_BOLD" "$prompt" "$C_RESET"
    read -r reply
    case "${reply,,}" in
        y|yes|si|s) return 0 ;;
        *)          return 1 ;;
    esac
}

# Trap su exit non-zero per dire dove ci si è fermati
on_error() {
    local exit_code=$?
    err "Script terminato con codice $exit_code allo step: ${CURRENT_STEP:-sconosciuto}"
    err "Log completo: $LOG_FILE"
    exit "$exit_code"
}
trap on_error ERR

CURRENT_STEP="init"

# ── 0. Pre-flight ────────────────────────────────────────────────────────────
preflight() {
    CURRENT_STEP="pre-flight"
    log "═══ Pre-flight check ═══"

    # WSL only
    if ! grep -qiE 'microsoft|wsl' /proc/version 2>/dev/null; then
        err "Questo script deve girare su WSL Ubuntu. Rilevato kernel non-WSL."
        exit 1
    fi
    ok "Ambiente WSL rilevato"

    # Repo root
    if [ ! -d "$REPO_ROOT" ]; then
        err "Repo root $REPO_ROOT non trovato"
        exit 1
    fi
    cd "$REPO_ROOT"
    ok "cd $REPO_ROOT"

    # Docker raggiungibile
    if ! docker info >/dev/null 2>&1; then
        err "Docker non raggiungibile da WSL (docker info fallito)"
        exit 1
    fi
    ok "Docker raggiungibile"

    # Backup dir
    mkdir -p "$BACKUP_DIR"
    ok "Backup dir: $BACKUP_DIR"

    # Log file
    : >"$LOG_FILE"
    ok "Log file: $LOG_FILE"

    log ""
    log "Operazioni che lo script tenterà in sequenza:"
    log "  1. Backup DB Nexus (obbligatorio)"
    log "  2. Conferma generale"
    log "  3. Rimozione container orfano $ORPHAN_CONTAINER"
    log "  4. Rimozione worktree git Windows prunable"
    log "  5. cargo clean (con conferma)"
    log "  6. Reinstall node_modules (con conferma)"
    log "  7. Disinstallazione Postgres nativo Linux (con conferma + safety check)"
    log "  8. Verifica finale"
    log ""
}

# ── 1. Backup DB Nexus (obbligatorio) ────────────────────────────────────────
backup_nexus_db() {
    CURRENT_STEP="backup-db-nexus"
    log "═══ Backup DB Nexus ═══"

    if ! docker ps --filter "name=^${NEXUS_PG_CONTAINER}$" --format '{{.Names}}' \
            | grep -q "$NEXUS_PG_CONTAINER"; then
        err "Container $NEXUS_PG_CONTAINER non running. Avvialo prima di lanciare lo script."
        exit 1
    fi
    ok "Container $NEXUS_PG_CONTAINER running"

    # Estrae user/db dalle env del container, fallback a nexus/nexus
    local pg_user pg_db
    pg_user="$(docker exec "$NEXUS_PG_CONTAINER" sh -c 'echo "$POSTGRES_USER"' 2>/dev/null \
                | tr -d '\r' || true)"
    pg_db="$(docker exec "$NEXUS_PG_CONTAINER" sh -c 'echo "$POSTGRES_DB"' 2>/dev/null \
                | tr -d '\r' || true)"
    pg_user="${pg_user:-nexus}"
    pg_db="${pg_db:-nexus}"
    log "Backup: user=$pg_user db=$pg_db (rilevati dalle env del container)"

    # Dump completo (pg_dumpall include tutti i db, ruoli e tablespace)
    if ! docker exec "$NEXUS_PG_CONTAINER" pg_dumpall -U "$pg_user" 2>>"$LOG_FILE" \
            | gzip >"$BACKUP_FILE"; then
        err "pg_dumpall fallito. Lo script si ferma prima di toccare qualsiasi cosa."
        rm -f "$BACKUP_FILE"
        exit 1
    fi

    if [ ! -s "$BACKUP_FILE" ]; then
        err "File di backup vuoto: $BACKUP_FILE"
        exit 1
    fi

    local size
    size="$(du -h "$BACKUP_FILE" | cut -f1)"
    ok "Backup creato: $BACKUP_FILE ($size)"

    # Verifica integrità gzip
    if ! gunzip -t "$BACKUP_FILE" 2>/dev/null; then
        err "Backup corrotto (gzip integrity check fallito)"
        exit 1
    fi
    ok "Integrità gzip verificata"
}

# ── 2. Conferma generale ─────────────────────────────────────────────────────
confirm_proceed() {
    CURRENT_STEP="confirm-proceed"
    log "═══ Conferma generale ═══"
    echo
    if ! confirm "Procedere con tutti i cleanup successivi?"; then
        warn "Abortito dall'utente. Backup DB rimane in $BACKUP_FILE."
        exit 0
    fi
}

# ── 3. Rimozione container orfano ────────────────────────────────────────────
cleanup_orphan_container() {
    CURRENT_STEP="cleanup-orphan-container"
    log "═══ Rimozione container orfano $ORPHAN_CONTAINER ═══"

    if docker ps -a --filter "name=^${ORPHAN_CONTAINER}$" --format '{{.Names}}' \
            | grep -q "$ORPHAN_CONTAINER"; then
        docker rm -f "$ORPHAN_CONTAINER" >>"$LOG_FILE" 2>&1 || true
        ok "Container $ORPHAN_CONTAINER rimosso"
    else
        ok "Container $ORPHAN_CONTAINER già assente (no-op)"
    fi
}

# ── 4. Rimozione worktree Windows prunable ───────────────────────────────────
cleanup_worktree_windows() {
    CURRENT_STEP="cleanup-worktree-windows"
    log "═══ Rimozione worktree git Windows ═══"

    # Prova prima il remove esplicito (può fallire se path Windows non accessibile)
    git -C "$REPO_ROOT" worktree remove --force "$WORKTREE_WINDOWS_PATH" 2>/dev/null || true

    # Prune in ogni caso (rimuove riferimenti orfani dal repo principale)
    git -C "$REPO_ROOT" worktree prune --verbose >>"$LOG_FILE" 2>&1 || true

    # Verifica che non sia più listato
    if git -C "$REPO_ROOT" worktree list | grep -qF "$WORKTREE_WINDOWS_PATH"; then
        warn "Il worktree è ancora listato in 'git worktree list'."
        warn "Riprovare con: git worktree prune --verbose --expire=now"
    else
        ok "Worktree $WORKTREE_WINDOWS_PATH non più registrato"
    fi

    log ""
    log "Per rimuovere fisicamente la dir Windows, esegui da PowerShell/cmd HOST:"
    log "  rmdir /s /q D:\\Sviluppo\\ideai\\recursing-poincare-2a5ee2"
    log "(Lo script non lo esegue automaticamente: territorio Windows.)"
}

# ── 5. cargo clean ───────────────────────────────────────────────────────────
cleanup_target() {
    CURRENT_STEP="cargo-clean"
    log "═══ cargo clean (recupera ~18 GB) ═══"

    if [ ! -d "$REPO_ROOT/target" ]; then
        ok "target/ già pulito (no-op)"
        return 0
    fi

    local size_before
    size_before="$(du -sh "$REPO_ROOT/target" 2>/dev/null | cut -f1 || echo "?")"
    log "target/ attuale: $size_before"

    if ! confirm "Eseguire 'cargo clean --workspace'? Il prossimo build sara' full (5-10 min)"; then
        warn "Skip cargo clean"
        return 0
    fi

    cd "$REPO_ROOT"
    cargo clean --workspace >>"$LOG_FILE" 2>&1
    ok "cargo clean completato"

    if [ -d "$REPO_ROOT/target" ]; then
        local size_after
        size_after="$(du -sh "$REPO_ROOT/target" 2>/dev/null | cut -f1 || echo "?")"
        log "target/ dopo: $size_after"
    fi
}

# ── 6. Reinstall node_modules ────────────────────────────────────────────────
cleanup_node_modules() {
    CURRENT_STEP="node-modules-clean-install"
    log "═══ Reinstall pulito di node_modules ═══"

    if ! confirm "Pulire e reinstallare TUTTI i node_modules del monorepo? Puo' richiedere 3-5 min"; then
        warn "Skip node_modules clean install"
        return 0
    fi

    cd "$REPO_ROOT"

    log "Cancellazione directory node_modules ricorsivamente..."
    # -prune evita di scendere dentro un node_modules già trovato
    # Esclude target/ per sicurezza (Rust build dir)
    find "$REPO_ROOT" \
        -type d \
        \( -name node_modules -o -path '*/target' \) \
        -prune \
        -name node_modules \
        -print \
        -exec rm -rf {} + 2>>"$LOG_FILE" || true
    ok "node_modules rimossi"

    log "Esecuzione pnpm install..."
    if pnpm install >>"$LOG_FILE" 2>&1; then
        ok "pnpm install completato"
    else
        err "pnpm install fallito. Vedi $LOG_FILE per dettagli."
        exit 1
    fi
}

# ── 7. Disinstallazione Postgres nativo Linux ────────────────────────────────
uninstall_native_postgres() {
    CURRENT_STEP="uninstall-native-postgres"
    log "═══ Disinstallazione Postgres nativo Linux ═══"

    # Rilevamento installazione nativa
    if ! command -v pg_lsclusters >/dev/null 2>&1; then
        ok "Postgres nativo già assente (pg_lsclusters non trovato, no-op)"
        return 0
    fi

    log "Cluster nativi rilevati:"
    pg_lsclusters | tee -a "$LOG_FILE"
    log ""

    # Safety check: lista database del cluster nativo
    log "Database nel cluster nativo:"
    local db_list=""
    if db_list="$(sudo -u postgres psql -lt 2>/dev/null | awk -F'|' '{gsub(/ /,"",$1); if($1!="") print $1}' \
                    | grep -vE '^(postgres|template0|template1)$' || true)"; then
        if [ -n "$db_list" ]; then
            warn "ATTENZIONE: il cluster nativo contiene database NON di sistema:"
            printf '  - %s\n' $db_list | tee -a "$LOG_FILE"
            warn "Se contengono dati che ti servono, ABORTA e fai backup prima."
            log ""
            if ! confirm "Ho letto la lista, procedo con la disinstallazione?"; then
                warn "Skip disinstallazione Postgres nativo"
                return 0
            fi
        else
            ok "Cluster nativo contiene solo db di sistema"
        fi
    fi

    if ! confirm "Disinstallare definitivamente Postgres nativo Linux (apt purge + cancellazione data dir)?"; then
        warn "Skip disinstallazione Postgres nativo"
        return 0
    fi

    log "Stop e disable del servizio postgresql..."
    sudo systemctl stop postgresql 2>>"$LOG_FILE" || true
    sudo systemctl disable postgresql 2>>"$LOG_FILE" || true
    ok "Servizio postgresql fermato e disabilitato"

    log "apt purge dei pacchetti postgresql..."
    sudo apt purge -y \
        'postgresql-16' \
        'postgresql-client-16' \
        'postgresql-common' \
        'postgresql-client-common' \
        'postgresql-contrib' >>"$LOG_FILE" 2>&1 || true
    sudo apt autoremove -y >>"$LOG_FILE" 2>&1 || true
    ok "Pacchetti rimossi"

    log "Rimozione directory residue..."
    sudo rm -rf /var/lib/postgresql /etc/postgresql /var/log/postgresql 2>>"$LOG_FILE" || true
    ok "Directory residue rimosse"

    # Verifica
    if command -v pg_lsclusters >/dev/null 2>&1; then
        warn "pg_lsclusters ancora presente, controllare residui"
    else
        ok "Postgres nativo disinstallato"
    fi
}

# ── 8. Verifica finale ───────────────────────────────────────────────────────
final_verification() {
    CURRENT_STEP="final-verification"
    log "═══ Verifica finale ═══"

    log ""
    log "Spazio disco /home:"
    df -h /home | tee -a "$LOG_FILE"

    log ""
    log "Dimensione $REPO_ROOT:"
    du -sh "$REPO_ROOT" 2>/dev/null | tee -a "$LOG_FILE" || true

    log ""
    log "Container infra Nexus running:"
    docker ps --filter "name=ideai-" --format 'table {{.Names}}\t{{.Status}}' | tee -a "$LOG_FILE"

    log ""
    log "Servizi systemd Nexus user-level running:"
    systemctl --user list-units --state=running --no-pager 2>/dev/null \
        | grep -i nexus | tee -a "$LOG_FILE" || warn "Nessun servizio nexus-* running"

    log ""
    log "Backup DB di questa sessione:"
    ls -lah "$BACKUP_FILE" | tee -a "$LOG_FILE"

    log ""
    log "Postgres nativo: $(command -v pg_lsclusters >/dev/null 2>&1 && echo 'ANCORA PRESENTE' || echo 'rimosso')"

    log ""
    ok "Cleanup completato. Log: $LOG_FILE"
    log ""
    log "Per testare il backup:"
    log "  gunzip -t $BACKUP_FILE && zcat $BACKUP_FILE | head -50"
    log ""
    log "Per ricostruire i Rust artifacts (se hai eseguito cargo clean):"
    log "  ./deploy/deploy-local.sh --rust"
}

# ── Main ─────────────────────────────────────────────────────────────────────
main() {
    preflight
    backup_nexus_db
    confirm_proceed
    cleanup_orphan_container
    cleanup_worktree_windows
    cleanup_target
    cleanup_node_modules
    uninstall_native_postgres
    final_verification
}

main "$@"
