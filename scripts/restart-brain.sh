#!/bin/bash
# Restart rapido del brain Python (nexus-neural) senza build.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

if [ -f "$ROOT/.env" ]; then
    set -a
    source "$ROOT/.env" 2>/dev/null || true
    set +a
fi

DATABASE_URL="${DATABASE_URL:-postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable}"

echo "Fermando brain Python..."
pkill -f "brain.grpc_server.main" 2>/dev/null || true
pkill -f "brain/grpc_server" 2>/dev/null || true
sleep 2

echo "Avviando brain Python..."
setsid nohup env \
    DATABASE_URL="$DATABASE_URL" \
    NEXUS_BRAIN_BILLING="${NEXUS_BRAIN_BILLING:-off}" \
    python3 -m brain.grpc_server.main --rest \
    > /tmp/nexus-neural.log 2>&1 < /dev/null &

PID=$!
disown "$PID" 2>/dev/null || true
echo "Brain Python avviato PID=$PID"
echo "Log: /tmp/nexus-neural.log"
sleep 8
tail -5 /tmp/nexus-neural.log 2>/dev/null || echo "log non disponibile ancora"
