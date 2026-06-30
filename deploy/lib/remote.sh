#!/usr/bin/env bash
# deploy/lib/remote.sh - Helpers condivisi per gli script di deploy produzione.
# Sourced da: bootstrap-prod.sh, deploy-prod.sh, reload-proxy.sh, health-check.sh,
#             cleanup-old-host.sh.
#
# Variabili attese (esportate dal chiamante o dal Makefile):
#   PROD_HOST    IP host applicativo (default: 192.168.0.6)
#   PROXY_HOST   IP proxy esterno     (default: 192.168.0.3)
#   SSH_USER     utente SSH           (default: administrator)
#   DEPLOY_DIR   path root remoto     (default: /opt/ideai)
#   PUBLIC_URL   URL pubblico         (default: https://nexus.cobracco.it)
#
# Convenzioni: niente emoji nei sorgenti (vedi CLAUDE.md sez. A).

set -euo pipefail

PROD_HOST="${PROD_HOST:-192.168.0.6}"
PROXY_HOST="${PROXY_HOST:-192.168.0.3}"
SSH_USER="${SSH_USER:-administrator}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/ideai}"
PUBLIC_URL="${PUBLIC_URL:-https://nexus.cobracco.it}"
SSH_OPTS="${SSH_OPTS:--o StrictHostKeyChecking=accept-new -o BatchMode=yes -o ServerAliveInterval=30}"

# Colori (TTY only)
if [ -t 1 ]; then
    C_RED='\033[0;31m'; C_GREEN='\033[0;32m'; C_YELLOW='\033[1;33m'
    C_BLUE='\033[0;34m'; C_DIM='\033[2m'; C_NC='\033[0m'
else
    C_RED=''; C_GREEN=''; C_YELLOW=''; C_BLUE=''; C_DIM=''; C_NC=''
fi

log()  { printf '%b[deploy]%b %s\n' "$C_GREEN" "$C_NC" "$*"; }
info() { printf '%b[info]%b  %s\n' "$C_BLUE"  "$C_NC" "$*"; }
warn() { printf '%b[warn]%b  %s\n' "$C_YELLOW" "$C_NC" "$*" >&2; }
err()  { printf '%b[error]%b %s\n' "$C_RED"   "$C_NC" "$*" >&2; }
die()  { err "$*"; exit 1; }
step() { printf '%b[%s/%s]%b %s\n' "$C_DIM" "$1" "$2" "$C_NC" "$3"; }

# remote_exec HOST CMD - esegue CMD via SSH con retry esponenziale (3 tentativi).
# Stdout/stderr propagati. Exit code = ultimo tentativo.
remote_exec() {
    local host="$1"; shift
    local cmd="$*"
    local attempt=1 max=3 delay=2 rc=0
    while [ "$attempt" -le "$max" ]; do
        if ssh $SSH_OPTS "${SSH_USER}@${host}" "$cmd"; then
            return 0
        fi
        rc=$?
        if [ "$attempt" -lt "$max" ]; then
            warn "SSH tentativo $attempt/$max fallito su $host (rc=$rc), retry in ${delay}s..."
            sleep "$delay"
            delay=$((delay * 2))
        fi
        attempt=$((attempt + 1))
    done
    return "$rc"
}

# remote_exec_quiet HOST CMD - come remote_exec ma sopprime output (utile per probe).
remote_exec_quiet() {
    local host="$1"; shift
    ssh $SSH_OPTS "${SSH_USER}@${host}" "$*" >/dev/null 2>&1
}

# remote_check_reachable HOST - true se SSH risponde su HOST.
remote_check_reachable() {
    local host="$1"
    ssh $SSH_OPTS -o ConnectTimeout=5 "${SSH_USER}@${host}" 'exit 0' 2>/dev/null
}

# sync_sources HOST DEST_DIR - sincronizza i sorgenti via git archive + ssh tar.
# Funziona da Windows nativo, WSL, Linux. Trasferisce HEAD + diff + untracked.
# Esclude: .env*, *.key, *.pem, *.log, target/, node_modules/, .next/.
sync_sources() {
    local host="$1" dest="$2"
    local repo_root
    repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "Non sono in un repo git"

    log "Sync sorgenti -> ${host}:${dest}"
    remote_exec "$host" "mkdir -p '$dest'"

    info "  [1/2] File committati (git archive)"
    ( cd "$repo_root" && git archive HEAD --format=tar ) \
        | ssh $SSH_OPTS "${SSH_USER}@${host}" "tar xf - -C '$dest'"

    info "  [2/2] File modificati + untracked"
    local extras
    extras=$( cd "$repo_root" && {
        git diff HEAD --name-only 2>/dev/null
        git ls-files --others --exclude-standard 2>/dev/null
    } | sed 's/\r$//' \
      | grep -vE '\.(env|key|pem|log)$|^\.env|^nexus\.env' \
      || true )

    if [ -n "$extras" ]; then
        local files=()
        while IFS= read -r f; do
            [ -n "$f" ] && [ -f "$repo_root/$f" ] && files+=("$f")
        done <<< "$extras"
        if [ "${#files[@]}" -gt 0 ]; then
            info "        ${#files[@]} file extra"
            ( cd "$repo_root" && tar czf - "${files[@]}" 2>/dev/null ) \
                | ssh $SSH_OPTS "${SSH_USER}@${host}" "tar xzf - -C '$dest'"
        fi
    fi

    # Normalizza CRLF -> LF sui .sql (sqlx Migrator verifica checksum, vuole LF)
    remote_exec_quiet "$host" \
        "find '$dest/db/migrations' -name '*.sql' -exec sed -i 's/\\r//' {} + 2>/dev/null || true"

    # Garantisce permessi di esecuzione su tutti gli script di deploy
    # (git su Windows puo' non preservare il bit +x)
    remote_exec_quiet "$host" \
        "find '$dest/deploy' '$dest/scripts' -name '*.sh' -exec chmod +x {} + 2>/dev/null || true"
}

# acquire_lock HOST NAME - prova ad acquisire un lock /tmp/NAME.lock su HOST.
# Stampa errore e fallisce se gia' detenuto. Il lock e' rilasciato al termine
# della sessione SSH (flock --close).
acquire_lock() {
    local host="$1" name="$2"
    local lockfile="/tmp/${name}.lock"
    if ! remote_exec_quiet "$host" \
        "flock --nonblock --close --conflict-exit-code 11 '$lockfile' true"; then
        die "Lock $lockfile gia' detenuto su $host (altro deploy in corso?)"
    fi
}

# run_with_lock HOST NAME CMD - esegue CMD su HOST tenendo il lock NAME per
# tutta la durata. Se il lock e' gia' detenuto, fallisce immediatamente.
run_with_lock() {
    local host="$1" name="$2"; shift 2
    local lockfile="/tmp/${name}.lock"
    remote_exec "$host" \
        "flock --nonblock --close --conflict-exit-code 11 '$lockfile' -c $(printf %q "$*")"
}

# confirm PROMPT - chiede [y/N] su TTY, auto-yes se CI=true.
confirm() {
    local prompt="$1"
    if [ "${CI:-}" = "true" ] || [ "${ASSUME_YES:-}" = "1" ]; then
        return 0
    fi
    if [ ! -t 0 ]; then
        warn "Non-TTY e ASSUME_YES non impostato, default: NO"
        return 1
    fi
    local reply
    printf '%b[?]%b %s [y/N] ' "$C_YELLOW" "$C_NC" "$prompt"
    read -r reply
    [[ "$reply" =~ ^[Yy]$ ]]
}

# require_clean_tree - exit se git working tree e' sporco (allow override con --allow-dirty)
require_clean_tree() {
    if [ "${ALLOW_DIRTY:-0}" = "1" ]; then
        warn "ALLOW_DIRTY=1, salto verifica working tree"
        return 0
    fi
    if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
        warn "Working tree sporco. File modificati:"
        git status --short | head -20
        warn "Override con: ALLOW_DIRTY=1 $0 ..."
        die "Commit o stash prima di deployare"
    fi
}

# commit_hash - hash corrente HEAD (12 char)
commit_hash() {
    git rev-parse --short=12 HEAD 2>/dev/null || echo "unknown"
}

# print_header TEXT - stampa banner separatore
print_header() {
    local text="$1"
    local sep="=============================================================="
    printf '\n%b%s%b\n' "$C_GREEN" "$sep" "$C_NC"
    printf '%b%s%b\n'   "$C_GREEN" "  $text" "$C_NC"
    printf '%b%s%b\n\n' "$C_GREEN" "$sep" "$C_NC"
}
