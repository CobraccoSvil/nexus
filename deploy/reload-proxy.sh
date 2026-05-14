#!/usr/bin/env bash
# deploy/reload-proxy.sh - Aggiorna nginx su PROXY_HOST (.03).
#
# Renderizza deploy/templates/nexus-proxy.conf.tmpl con $PROD_HOST e lo
# installa in /etc/nginx/sites-available/nexus, fa backup del vecchio,
# valida con nginx -t, ricarica e fa smoke test esterno.
#
# Uso: ./deploy/reload-proxy.sh [--dry-run]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/remote.sh
. "$SCRIPT_DIR/lib/remote.sh"

DRY_RUN="0"
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN="1" ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -15
            exit 0 ;;
    esac
done

TEMPLATE="$SCRIPT_DIR/templates/nexus-proxy.conf.tmpl"
[ -f "$TEMPLATE" ] || die "Template non trovato: $TEMPLATE"

print_header "Reload nginx su $PROXY_HOST -> upstream $PROD_HOST:80"

# === Render template =========================================================
log "Render template con PROD_HOST=$PROD_HOST"
RENDERED="/tmp/nexus-proxy.$$.conf"
trap "rm -f $RENDERED" EXIT

export PROD_HOST
envsubst '${PROD_HOST}' < "$TEMPLATE" > "$RENDERED"

if [ "$DRY_RUN" = "1" ]; then
    info "Dry-run: contenuto renderizzato:"
    sed 's/^/    /' "$RENDERED"
    exit 0
fi

# === Pre-check =================================================================
remote_check_reachable "$PROXY_HOST" || die "SSH a $PROXY_HOST non raggiungibile"
remote_exec_quiet "$PROXY_HOST" "sudo -n true" \
    || die "sudo passwordless richiesto su $PROXY_HOST"

# Verifica che PROD_HOST sia raggiungibile DAL proxy (firewall LAN)
log "Test connettivita' $PROXY_HOST -> $PROD_HOST:80"
if ! remote_exec_quiet "$PROXY_HOST" "nc -zv -w 5 $PROD_HOST 80"; then
    warn "Da $PROXY_HOST non riesco a connettermi a $PROD_HOST:80"
    warn "Verifica: nginx attivo su $PROD_HOST + firewall LAN aperto"
    confirm "Continuo lo stesso (l'upgrade rompera' il sito)?" || die "Aborted"
fi

# === Backup config esistente ===================================================
TIMESTAMP=$(date +%F-%H%M%S)
BACKUP="/etc/nginx/sites-available/nexus.bak.$TIMESTAMP"
log "Backup config corrente -> $BACKUP"
remote_exec "$PROXY_HOST" "
    if [ -f /etc/nginx/sites-available/nexus ]; then
        sudo cp /etc/nginx/sites-available/nexus '$BACKUP'
    else
        echo 'Nessun /etc/nginx/sites-available/nexus esistente (primo deploy)'
    fi
"

# === Upload + validate + reload ================================================
log "Upload config renderizzato"
scp $SSH_OPTS "$RENDERED" "${SSH_USER}@${PROXY_HOST}:/tmp/nexus.conf"

log "Validazione e reload nginx"
remote_exec "$PROXY_HOST" "
    sudo cp /tmp/nexus.conf /etc/nginx/sites-available/nexus
    # Assicurati che sia abilitato in sites-enabled
    sudo ln -sf /etc/nginx/sites-available/nexus /etc/nginx/sites-enabled/nexus
    if ! sudo nginx -t 2>&1; then
        echo 'nginx -t FALLITO, ripristino backup'
        if [ -f '$BACKUP' ]; then
            sudo cp '$BACKUP' /etc/nginx/sites-available/nexus
            sudo nginx -t
        fi
        exit 1
    fi
    sudo systemctl reload nginx
    rm -f /tmp/nexus.conf
"

# === Smoke test esterno ========================================================
log "Smoke test $PUBLIC_URL"
sleep 2
if curl -fsS --max-time 10 -o /dev/null -w '%{http_code}' "$PUBLIC_URL/health"; then
    printf '\n'
    log "Reload OK - $PUBLIC_URL raggiungibile"
else
    err "Smoke test fallito su $PUBLIC_URL/health"
    err "Backup disponibile: ssh $PROXY_HOST 'sudo cp $BACKUP /etc/nginx/sites-available/nexus && sudo systemctl reload nginx'"
    exit 1
fi
