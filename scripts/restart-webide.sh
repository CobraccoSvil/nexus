#!/bin/bash
# Restart rapido del web-ide Next.js (production mode, senza rebuild).
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

if [ -f "$ROOT/.env" ]; then
    set -a
    source "$ROOT/.env" 2>/dev/null || true
    set +a
fi

DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}"
POSTGRES_URL="${POSTGRES_URL:-$DATABASE_URL}"

echo "Fermando web-ide..."
pkill -f "next-server" 2>/dev/null || true
pkill -f "next start" 2>/dev/null || true
pkill -f "node.*server.js.*web-ide" 2>/dev/null || true
sleep 2

echo "Avviando web-ide (production)..."
setsid nohup env \
    NODE_ENV=production \
    DATABASE_URL="$DATABASE_URL" \
    POSTGRES_URL="$POSTGRES_URL" \
    pnpm --filter @ai-orchestrator/web-ide start \
    > /tmp/nexus-webide.log 2>&1 < /dev/null &

PID=$!
disown "$PID" 2>/dev/null || true
echo "web-ide avviato PID=$PID"
echo "Log: /tmp/nexus-webide.log"
sleep 8
tail -5 /tmp/nexus-webide.log 2>/dev/null || echo "log non disponibile ancora"
ss -tlnp 2>/dev/null | grep ':3000' && echo "OK: porta 3000 in ascolto" || echo "WARN: porta 3000 non ancora in ascolto"
