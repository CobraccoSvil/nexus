#!/usr/bin/env bash
# deploy/cleanup-old-host.sh - Dismissione del vecchio Nexus su PROXY_HOST.
#
# DA ESEGUIRE SOLO DOPO che il nuovo Nexus su PROD_HOST e' verde da >= 24h.
# Backup tar.gz in /root/ideai-old-backup-<data>.tar.gz prima della rimozione.
#
# Operazioni:
#   1. Estrae secret dal vecchio .env (per backup)
#   2. Stop e disable systemd units nexus-*
#   3. Stop container Docker filtrati per progetto (label com.docker.compose.project=ideai)
#   4. Backup /opt/ideai + /opt/ai-orchestrator + /var/log/ideai
#   5. Rimuove filesystem
#   6. Stop Postgres locale (NON purge - lascia il pacchetto)
#
# Uso: ./deploy/cleanup-old-host.sh [--force]
#      Senza --force chiede conferma interattiva.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/remote.sh
. "$SCRIPT_DIR/lib/remote.sh"

FORCE="0"
for arg in "$@"; do
    case "$arg" in
        --force|-f) FORCE="1"; export ASSUME_YES=1 ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -20
            exit 0 ;;
    esac
done

print_header "Cleanup VECCHIO Nexus su $PROXY_HOST"

warn "Questa operazione DISTRUGGE permanentemente il Nexus su $PROXY_HOST"
warn "Prerequisiti:"
warn "  - Il nuovo Nexus su $PROD_HOST funziona correttamente da >= 24h"
warn "  - Tutti i secret (.env, API keys) sono stati copiati su $PROD_HOST"
warn "  - Il DNS nexus.cobracco.it gia' transita per $PROXY_HOST/nginx -> $PROD_HOST"
printf '\n'

confirm "Procedo con il cleanup di $PROXY_HOST?" || die "Cleanup abortito"

# === Step 1: Estrai secret per backup =========================================
log "Estrazione secret per audit"
if remote_exec_quiet "$PROXY_HOST" "sudo test -f /opt/ideai/.env"; then
    SECRETS_FILE="/tmp/ideai-old-secrets-$(date +%F-%H%M).env"
    remote_exec "$PROXY_HOST" "sudo cat /opt/ideai/.env" \
        | grep -E '_API_KEY|JWT_SECRET|DATABASE_URL' > "$SECRETS_FILE" || true
    info "  Secret estratti in $SECRETS_FILE (locale, verifica e poi cancella)"
elif remote_exec_quiet "$PROXY_HOST" "sudo test -f /opt/ai-orchestrator/.env"; then
    SECRETS_FILE="/tmp/ai-orch-old-secrets-$(date +%F-%H%M).env"
    remote_exec "$PROXY_HOST" "sudo cat /opt/ai-orchestrator/.env" \
        | grep -E '_API_KEY|JWT_SECRET|DATABASE_URL' > "$SECRETS_FILE" || true
    info "  Secret estratti in $SECRETS_FILE"
else
    info "  Nessun .env trovato (gia' rimosso?)"
fi

# === Step 2: Stop systemd =====================================================
log "Stop e disable systemd units"
remote_exec "$PROXY_HOST" "
    for unit in nexus-core nexus-admin nexus-chat nexus-billing nexus-docs \
                nexus-plugins nexus-webide nexus-neural nexus-gateway nexus-gateway-node; do
        if systemctl list-unit-files 2>/dev/null | grep -q \"^\${unit}.service\"; then
            sudo systemctl disable --now \$unit 2>/dev/null || true
            echo \"  disabled: \$unit\"
        fi
    done
    sudo rm -f /etc/systemd/system/nexus-*.service
    sudo systemctl daemon-reload
"

# === Step 3: Docker cleanup (FILTRATO per progetto) ===========================
log "Stop container Docker (label com.docker.compose.project=ideai)"
remote_exec "$PROXY_HOST" '
    set -e
    containers=$(docker ps --filter "label=com.docker.compose.project=ideai" -q 2>/dev/null || true)
    if [ -n "$containers" ]; then
        echo "  Stop: $containers"
        docker stop $containers
    fi
    containers_all=$(docker ps -a --filter "label=com.docker.compose.project=ideai" -q 2>/dev/null || true)
    if [ -n "$containers_all" ]; then
        echo "  Rm: $containers_all"
        docker rm $containers_all
    fi
    # Volumi noti del progetto
    for vol in postgres-data qdrant-data grafana-data; do
        if docker volume ls -q 2>/dev/null | grep -qx "$vol"; then
            echo "  Rm volume: $vol"
            docker volume rm "$vol" 2>/dev/null || true
        fi
    done
'

# === Step 4: Backup filesystem ================================================
log "Backup filesystem prima della rimozione"
remote_exec "$PROXY_HOST" "
    set -e
    BACKUP=/root/ideai-old-backup-\$(date +%F-%H%M).tar.gz
    PATHS=()
    [ -d /opt/ideai ] && PATHS+=(/opt/ideai)
    [ -d /opt/ai-orchestrator ] && PATHS+=(/opt/ai-orchestrator)
    [ -d /var/log/ideai ] && PATHS+=(/var/log/ideai)

    if [ \${#PATHS[@]} -gt 0 ]; then
        sudo tar czf \"\$BACKUP\" \"\${PATHS[@]}\" 2>/dev/null || true
        echo \"  Backup salvato: \$BACKUP (\$(sudo du -h \"\$BACKUP\" | cut -f1))\"
    else
        echo '  Nessun path da archiviare'
    fi
"

# === Step 5: Rimozione filesystem =============================================
log "Rimozione /opt/ideai, /opt/ai-orchestrator, /var/log/ideai"
remote_exec "$PROXY_HOST" "
    sudo rm -rf /opt/ideai /opt/ai-orchestrator /var/log/ideai
    sudo rm -f /etc/logrotate.d/ideai
"

# === Step 6: Postgres locale (stop, NON purge) ================================
log "Stop Postgres locale (pacchetto preservato)"
remote_exec "$PROXY_HOST" "
    if systemctl list-unit-files | grep -q '^postgresql.service'; then
        sudo systemctl disable --now postgresql 2>/dev/null || true
        echo '  postgresql disabled (pacchetto preservato per altri usi)'
    fi
"

# === Conferma cleanup =========================================================
print_header "Cleanup completato su $PROXY_HOST"
info "Verifica nginx + TLS ancora attivi (l'unico ruolo residuo di $PROXY_HOST):"
remote_exec "$PROXY_HOST" "
    systemctl is-active nginx
    curl -fsSI https://localhost/health -k 2>/dev/null | head -1 || true
"
info "Test esterno: curl -fsSI $PUBLIC_URL/health"
curl -fsSI --max-time 10 "$PUBLIC_URL/health" | head -1 \
    && log "Endpoint pubblico OK -- cleanup riuscito"
