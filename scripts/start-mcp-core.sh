#!/usr/bin/env bash
# Avvio mcp-core in background con log a /tmp/nexus-mcp-core.log.
# Usato dopo recovery migrazioni 0168/0169 da stash.
set -uo pipefail

cd /home/administrator/ideai

# Carica env
if [ -f .env ]; then
  set -a; source .env 2>/dev/null || true; set +a
fi

# Kill istanze esistenti
pkill -9 -f "target/release/mcp-core" 2>/dev/null || true
sleep 1

LOG=/tmp/nexus-mcp-core.log
: > "$LOG"

nohup ./target/release/mcp-core >> "$LOG" 2>&1 &
PID=$!
echo "mcp-core started PID=$PID, log=$LOG"

# Attendi up a 30s
for i in $(seq 1 30); do
  sleep 1
  if ss -ltn '( sport = :4000 )' 2>/dev/null | grep -q ':4000'; then
    echo "[ OK ] mcp-core listening on :4000 (after ${i}s)"
    exit 0
  fi
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "[FAIL] processo $PID terminato"
    tail -20 "$LOG"
    exit 1
  fi
done

echo "[TIMEOUT] non in listen dopo 30s"
tail -30 "$LOG"
exit 1
