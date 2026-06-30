#!/bin/bash
# Restart rapido di mcp-core (senza build). Usa il binario gia' compilato in target/release.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

if [ -f "$ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$ROOT/.env" 2>/dev/null || true
    set +a
fi

DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}"
POSTGRES_URL="${POSTGRES_URL:-$DATABASE_URL}"

echo "Fermando mcp-core..."
pkill -f "target/release/mcp-core" 2>/dev/null || true
sleep 2

BIN="$ROOT/target/release/mcp-core"
if [ ! -f "$BIN" ]; then
    echo "ERRORE: $BIN non trovato" >&2
    exit 1
fi

echo "Avviando mcp-core..."
setsid nohup env \
    ENABLE_TOOL_RUNNER=1 \
    DATABASE_URL="$DATABASE_URL" \
    POSTGRES_URL="$POSTGRES_URL" \
    "$BIN" > /tmp/nexus-mcp-core.log 2>&1 < /dev/null &

PID=$!
disown "$PID" 2>/dev/null || true
echo "mcp-core avviato PID=$PID"
echo "Log: /tmp/nexus-mcp-core.log"
sleep 5
ss -tlnp 2>/dev/null | grep ':4000' && echo "OK: porta 4000 in ascolto" || echo "WARN: porta 4000 non ancora in ascolto (potrebbe impiegare qualche secondo)"
