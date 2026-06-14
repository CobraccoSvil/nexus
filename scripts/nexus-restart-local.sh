#!/usr/bin/env bash
# Riavvio locale stack Nexus (senza full build workspace). Uso: bash scripts/nexus-restart-local.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

ENV_FILE="${ROOT}/.env"
if [ -f "$ENV_FILE" ]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE" 2>/dev/null || true
  set +a
fi

DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}"
POSTGRES_URL="${POSTGRES_URL:-$DATABASE_URL}"
export DATABASE_URL POSTGRES_URL

stop() { pkill -f "$1" 2>/dev/null && sleep 1 || true; }

log() { echo "==> $*"; }

log "Docker infra"
docker compose -f docker-compose.local.yml up -d 2>/dev/null || true

log "Stop processi Nexus"
# Migrazione gateway a Rust: il gateway e' il binario `nexus-gateway`
# (target/debug). Il vecchio server Node (apps/nexus-gateway/dist/server.js)
# e' stato eliminato; manteniamo solo il pattern Rust nello stop.
for x in mcp-core admin-service chat-service billing-service doc-service plugin-service browser-bridge-mcp \
  brain.grpc_server.main target/debug/nexus-gateway target/release/nexus-gateway "pnpm.*web-ide"; do
  stop "$x"
done
pkill -f "next-server" 2>/dev/null || true
pkill -f "next start" 2>/dev/null || true
sleep 2

log "Build mcp-core release"
cargo build --release -p mcp-core

log "Build nexus-gateway release (Rust)"
cargo build --release -p nexus-gateway --bin nexus-gateway

log "Neural Core"
setsid nohup env DATABASE_URL="$DATABASE_URL" NEXUS_BRAIN_BILLING="${NEXUS_BRAIN_BILLING:-off}" \
  python3 -m brain.grpc_server.main --rest > /tmp/nexus-neural.log 2>&1 < /dev/null &
sleep 5

GW_PORT="${NEXUS_GATEWAY_PORT:-4060}"
log "Nexus Gateway :${GW_PORT} (Rust)"
# Migrazione Fase 6: il gateway e' il binario Rust del crate nexus-gateway. Le
# config (policy/alias/chiavi) sono risolte dal DB all'avvio (regola G), quindi
# non servono piu' le env *_FILE del vecchio server Node. cwd = ROOT cosi' il
# bootstrap trova eventuali file config relativi.
( cd "${ROOT}" && setsid nohup env DATABASE_URL="$DATABASE_URL" POSTGRES_URL="$POSTGRES_URL" \
  NEXUS_GATEWAY_PORT="$GW_PORT" \
  "${ROOT}/target/release/nexus-gateway" > /tmp/nexus-gateway.log 2>&1 < /dev/null & )
sleep 2

RELEASE="${ROOT}/target/release"
log "mcp-core + microservizi"
setsid nohup env ENABLE_TOOL_RUNNER=1 DATABASE_URL="$DATABASE_URL" POSTGRES_URL="$POSTGRES_URL" \
  "${RELEASE}/mcp-core" > /tmp/nexus-mcp-core.log 2>&1 < /dev/null &
sleep 3
for svc in admin-service chat-service billing-service doc-service plugin-service; do
  setsid nohup env DATABASE_URL="$DATABASE_URL" POSTGRES_URL="$POSTGRES_URL" \
    "${RELEASE}/${svc}" > "/tmp/nexus-${svc}.log" 2>&1 < /dev/null &
done
sleep 2
setsid nohup env BROWSER_BRIDGE_PORT="${BROWSER_BRIDGE_PORT:-4055}" DATABASE_URL="$DATABASE_URL" \
  "${RELEASE}/browser-bridge-mcp" > /tmp/nexus-browser-bridge-mcp.log 2>&1 < /dev/null &

log "web-ide production"
if [ ! -f "${ROOT}/apps/web-ide/.next/BUILD_ID" ]; then
  log "next build web-ide (.next mancante)"
  NODE_ENV=production pnpm --filter @ai-orchestrator/web-ide build
fi
setsid nohup env NODE_ENV=production DATABASE_URL="$DATABASE_URL" POSTGRES_URL="$POSTGRES_URL" \
  pnpm --filter @ai-orchestrator/web-ide start > /tmp/nexus-webide.log 2>&1 < /dev/null &
sleep 4

log "Porte in ascolto"
ss -tlnp 2>/dev/null | grep -E ':3000|:4000|:4010|:4020|:4030|:4040|:4060|:8001|:50051|:4055' || true

log "Fine. Log: /tmp/nexus-*.log"
