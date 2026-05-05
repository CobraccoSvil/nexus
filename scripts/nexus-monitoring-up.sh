#!/usr/bin/env bash
# nexus-monitoring-up.sh — Avvia lo stack di monitoraggio Nexus su server-prod.
#
# Porta up (via `docker compose`) SOLO i servizi prometheus + grafana definiti
# in deploy/docker-compose.yml, senza toccare qdrant/redis che già girano come
# bare container. I servizi postgres/shadow-db/qdrant/redis del compose NON
# vengono avviati qui per evitare collisioni.
#
# Prerequisiti remoti:
#   - sorgenti sincronizzate in /opt/ai-orchestrator (deploy-nexus.sh --rust-only
#     oppure sync manuale)
#   - docker + docker compose v2 installati su server-prod
#   - porte 9090 (prometheus) e 3001 (grafana) libere
#
# Uso:
#   scripts/nexus-monitoring-up.sh              # up
#   scripts/nexus-monitoring-up.sh down         # stop
#   scripts/nexus-monitoring-up.sh status       # stato
#   scripts/nexus-monitoring-up.sh logs         # tail logs

set -euo pipefail

REMOTE="${DEPLOY_HOST:-administrator@nexus-prod}"
DEPLOY_DIR="/opt/ai-orchestrator"
COMPOSE_FILE="$DEPLOY_DIR/deploy/docker-compose.yml"
SERVICES=("prometheus" "grafana")
ACTION="${1:-up}"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[monitoring]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
err()  { echo -e "${RED}[error]${NC} $*" >&2; exit 1; }
info() { echo -e "${BLUE}[info]${NC} $*"; }

remote() { ssh "$REMOTE" "$@"; }

ensure_compose_available() {
    remote "docker compose version >/dev/null 2>&1" \
        || err "docker compose non disponibile su $REMOTE — installa docker-compose-plugin"
    remote "test -f '$COMPOSE_FILE'" \
        || err "$COMPOSE_FILE non trovato — esegui prima scripts/deploy-nexus.sh per sincronizzare"
}

check_port_collision() {
    local port="$1"
    local service="$2"
    if remote "ss -tln 'sport = :$port' 2>/dev/null | tail -n +2 | grep -q ."; then
        warn "porta $port già in uso su $REMOTE (servizio $service)"
        warn "  → eventuali bind collideranno. Controlla con: ssh $REMOTE 'ss -tlnp | grep :$port'"
    fi
}

up() {
    ensure_compose_available
    check_port_collision 9090 prometheus
    check_port_collision 3001 grafana

    info "Avvio ${SERVICES[*]} via docker compose su $REMOTE..."
    remote "cd '$DEPLOY_DIR/deploy' && docker compose up -d ${SERVICES[*]}"

    log "Attendo readiness..."
    local prom_ok=0 graf_ok=0
    for i in $(seq 1 20); do
        if remote "curl -fsS http://localhost:9090/-/healthy >/dev/null 2>&1"; then
            prom_ok=1
        fi
        if remote "curl -fsS http://localhost:3001/api/health >/dev/null 2>&1"; then
            graf_ok=1
        fi
        [ "$prom_ok" = 1 ] && [ "$graf_ok" = 1 ] && break
        sleep 1
    done

    if [ "$prom_ok" = 1 ]; then
        log "✓ Prometheus   — http://${DEPLOY_HOST_IP:-localhost}:9090"
    else
        warn "⚠ Prometheus non risponde dopo 20s — controlla: $0 logs"
    fi

    if [ "$graf_ok" = 1 ]; then
        log "✓ Grafana      — http://${DEPLOY_HOST_IP:-localhost}:3001  (admin/admin)"
        info "  Dashboard: Nexus → Nexus Orchestrator"
    else
        warn "⚠ Grafana non risponde dopo 20s — controlla: $0 logs"
    fi

    echo
    info "Prometheus scraping job: nexus-mcp-core → http://host.docker.internal:4000/nexus/metrics"
    info "Verifica scraping in Grafana: Connections → Data sources → Prometheus → Test"
}

down() {
    info "Stop di ${SERVICES[*]}..."
    remote "cd '$DEPLOY_DIR/deploy' && docker compose stop ${SERVICES[*]}"
    log "Stack monitoring fermo (container preservati, riavviabili con: $0 up)"
}

status() {
    info "Stato container monitoring su $REMOTE:"
    remote "cd '$DEPLOY_DIR/deploy' && docker compose ps ${SERVICES[*]}"
}

logs() {
    info "Tail logs ${SERVICES[*]} (CTRL-C per uscire):"
    remote "cd '$DEPLOY_DIR/deploy' && docker compose logs --tail=100 -f ${SERVICES[*]}"
}

case "$ACTION" in
    up)     up ;;
    down)   down ;;
    status) status ;;
    logs)   logs ;;
    *)
        echo "Uso: $0 [up|down|status|logs]"
        exit 1
        ;;
esac
