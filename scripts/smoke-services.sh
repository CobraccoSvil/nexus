#!/usr/bin/env bash
# scripts/smoke-services.sh — Smoke test dei servizi chiave.
#
# Avvia brevemente nexus-gateway, admin-service e web-ide in background,
# verifica che le porte attese siano in ascolto, poi smonta tutto.
#
# Porte attese (modificabili via env):
#   NEXUS_GATEWAY_PORT (default 8080)
#   ADMIN_SERVICE_PORT (default 8081)
#   WEB_IDE_PORT       (default 3000)

set -u

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

NEXUS_GATEWAY_PORT="${NEXUS_GATEWAY_PORT:-8080}"
ADMIN_SERVICE_PORT="${ADMIN_SERVICE_PORT:-8081}"
WEB_IDE_PORT="${WEB_IDE_PORT:-3000}"
WAIT_SECS="${WAIT_SECS:-20}"

PIDS=()
cleanup() {
    for pid in "${PIDS[@]:-}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT

check_port() {
    local port="$1"
    local name="$2"
    for _ in $(seq 1 "$WAIT_SECS"); do
        if ss -tlnp 2>/dev/null | grep -q ":${port}\b"; then
            echo "OK smoke: ${name} in ascolto su :${port}"
            return 0
        fi
        sleep 1
    done
    echo "!! smoke: ${name} NON in ascolto su :${port} dopo ${WAIT_SECS}s" >&2
    return 1
}

echo "==> smoke: avvio nexus-gateway (porta ${NEXUS_GATEWAY_PORT})"
(cd apps/nexus-gateway && PORT="$NEXUS_GATEWAY_PORT" pnpm exec tsx src/server.ts >/tmp/smoke-nexus.log 2>&1) &
PIDS+=("$!")

echo "==> smoke: avvio admin-service (porta ${ADMIN_SERVICE_PORT})"
(PORT="$ADMIN_SERVICE_PORT" cargo run -p admin-service >/tmp/smoke-admin.log 2>&1) &
PIDS+=("$!")

echo "==> smoke: avvio web-ide (porta ${WEB_IDE_PORT})"
(cd apps/web-ide && PORT="$WEB_IDE_PORT" pnpm exec next dev >/tmp/smoke-web.log 2>&1) &
PIDS+=("$!")

FAIL=0
check_port "$NEXUS_GATEWAY_PORT" "nexus-gateway" || FAIL=1
check_port "$ADMIN_SERVICE_PORT" "admin-service" || FAIL=1
check_port "$WEB_IDE_PORT" "web-ide" || FAIL=1

if [[ "$FAIL" -ne 0 ]]; then
    echo "-- smoke: estratti log (tail) --"
    for f in /tmp/smoke-nexus.log /tmp/smoke-admin.log /tmp/smoke-web.log; do
        echo "### ${f}"
        tail -n 20 "$f" 2>/dev/null || true
    done
    exit 1
fi

echo "OK smoke: tutti i servizi hanno aperto le porte attese"
