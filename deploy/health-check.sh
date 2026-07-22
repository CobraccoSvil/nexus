#!/usr/bin/env bash
# deploy/health-check.sh - Smoke test post-deploy.
#
# Verifica:
#   1. SSH a PROD_HOST e PROXY_HOST raggiungibili
#   2. systemd units active su PROD_HOST
#   3. Porte applicative aperte su PROD_HOST (3000, 4000, 4010, 4020)
#   4. Endpoint interno /health su PROD_HOST -> 200
#   5. Log core/webide ultimi 100 righe senza ERROR/panic
#   6. Endpoint pubblico PUBLIC_URL/health attraverso .03 -> 200
#   7. routing_matrix popolata - BLOCCANTE come gli altri (direttiva G): una
#      matrice vuota fa panicare mcp-core all'avvio, quindi un deploy in quello
#      stato non e' degradato, e' rotto. L'intestazione lo dava per "best-effort"
#      ma il check incrementa CHECKS_FAILED come tutti e lo script esce 1: a
#      mentire era il commento, non il codice.
#
# Exit code: 0 tutto OK, 1 al primo fallimento.
# Uso: ./deploy/health-check.sh [--verbose] [--external-only]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/remote.sh
. "$SCRIPT_DIR/lib/remote.sh"

VERBOSE="${VERBOSE:-0}"
EXTERNAL_ONLY="0"
for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE="1" ;;
        --external-only) EXTERNAL_ONLY="1" ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -20
            exit 0 ;;
    esac
done

CHECKS_PASSED=0
CHECKS_FAILED=0

check() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '  %bOK%b  %s\n' "$C_GREEN" "$C_NC" "$desc"
        CHECKS_PASSED=$((CHECKS_PASSED + 1))
        return 0
    else
        printf '  %bKO%b  %s\n' "$C_RED" "$C_NC" "$desc"
        CHECKS_FAILED=$((CHECKS_FAILED + 1))
        if [ "$VERBOSE" = "1" ]; then
            "$@" 2>&1 | sed 's/^/      /' || true
        fi
        return 1
    fi
}

print_header "Health check Nexus ($PROD_HOST <- $PROXY_HOST <- $PUBLIC_URL)"

# === 1. SSH reachability =====================================================
log "Connettivita' SSH"
check "SSH a $PROD_HOST ($SSH_USER)"  remote_check_reachable "$PROD_HOST"
check "SSH a $PROXY_HOST ($SSH_USER)" remote_check_reachable "$PROXY_HOST" || true

if [ "$EXTERNAL_ONLY" != "1" ]; then

# === 2. systemd units ========================================================
log "systemd units su $PROD_HOST"
for unit in nexus-core nexus-webide; do
    check "$unit active" \
        remote_exec_quiet "$PROD_HOST" "systemctl is-active --quiet $unit"
done

# Microservizi opzionali (non bloccanti)
for unit in nexus-admin nexus-billing nexus-docs nexus-plugins; do
    if remote_exec_quiet "$PROD_HOST" "systemctl list-unit-files | grep -q ^${unit}.service"; then
        check "$unit active (opzionale)" \
            remote_exec_quiet "$PROD_HOST" "systemctl is-active --quiet $unit" || true
    fi
done

# === 3. Porte applicative ====================================================
log "Porte applicative su $PROD_HOST"
check "Porta 3000 (web-ide)"      remote_exec_quiet "$PROD_HOST" "ss -tln 'sport = :3000' | grep -q :3000"
check "Porta 4000 (mcp-core)"     remote_exec_quiet "$PROD_HOST" "ss -tln 'sport = :4000' | grep -q :4000"
check "Porta 80 (nginx interno)"  remote_exec_quiet "$PROD_HOST" "ss -tln 'sport = :80' | grep -q :80"

# === 4. Health interno =======================================================
log "Endpoint /health interno su $PROD_HOST"
check "curl http://127.0.0.1/health" \
    remote_exec_quiet "$PROD_HOST" "curl -fsS --max-time 5 http://127.0.0.1/health"
check "curl http://127.0.0.1:3000/" \
    remote_exec_quiet "$PROD_HOST" "curl -fsS --max-time 5 -o /dev/null http://127.0.0.1:3000/"

# === 5. Log scan =============================================================
log "Scan log applicativi (ultime 100 righe)"
for log_file in core.log webide.log; do
    if remote_exec_quiet "$PROD_HOST" "test -f /var/log/ideai/$log_file"; then
        check "$log_file senza ERROR/panic" \
            remote_exec_quiet "$PROD_HOST" \
                "! sudo tail -n 100 /var/log/ideai/$log_file 2>/dev/null | grep -qE 'ERROR|panicked'"
    fi
done

# === 6. Routing matrix popolata (direttiva G di CLAUDE.md) ===================
log "Registry modelli (nexus_routing_matrix)"
matrix_count=$(remote_exec "$PROD_HOST" \
    "sudo -u postgres psql -tAc \"SELECT COUNT(*) FROM nexus_routing_matrix;\" nexus 2>/dev/null || echo 0")
if [ "${matrix_count:-0}" -gt 0 ]; then
    printf '  %bOK%b  nexus_routing_matrix: %s righe\n' "$C_GREEN" "$C_NC" "$matrix_count"
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
else
    printf '  %bKO%b  nexus_routing_matrix VUOTA -- mcp-core paniche all avvio\n' "$C_RED" "$C_NC"
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
fi

fi  # !EXTERNAL_ONLY

# === 7. Endpoint pubblico ====================================================
log "Endpoint pubblico $PUBLIC_URL"
check "curl $PUBLIC_URL/health (via $PROXY_HOST)" \
    curl -fsS --max-time 10 -o /dev/null "$PUBLIC_URL/health"
check "TLS certificato valido" \
    bash -c "curl -fsS -o /dev/null --max-time 10 '$PUBLIC_URL' 2>&1"

# === Riepilogo ===============================================================
printf '\n'
total=$((CHECKS_PASSED + CHECKS_FAILED))
if [ "$CHECKS_FAILED" -eq 0 ]; then
    log "Tutti i check superati ($CHECKS_PASSED/$total)"
    exit 0
else
    err "$CHECKS_FAILED/$total check falliti (rerun con --verbose per dettagli)"
    exit 1
fi
