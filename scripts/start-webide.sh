#!/usr/bin/env bash
# Avvio web-ide Next.js in background.
set -uo pipefail

cd /home/administrator/ideai/apps/web-ide

if [ -f /home/administrator/ideai/.env ]; then
  set -a; source /home/administrator/ideai/.env 2>/dev/null || true; set +a
fi

pkill -9 -f "next-server" 2>/dev/null || true
pkill -9 -f "next start" 2>/dev/null || true
sleep 1

LOG=/tmp/nexus-webide.log
: > "$LOG"

# setsid distacca il processo dal gruppo bash, sopravvive a SIGHUP/SIGTERM
# dello script padre. `nohup` da solo non basta perche' `pnpm exec` apre
# subprocess in gruppo.
setsid bash -c "cd /home/administrator/ideai/apps/web-ide && exec pnpm exec next start -p 3000" \
  >> "$LOG" 2>&1 < /dev/null &
PID=$!
disown 2>/dev/null || true
echo "web-ide started PID=$PID, log=$LOG"

for i in $(seq 1 40); do
  sleep 1
  if ss -ltn '( sport = :3000 )' 2>/dev/null | grep -q ':3000'; then
    echo "[ OK ] web-ide listening on :3000 (after ${i}s)"
    exit 0
  fi
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "[FAIL] processo $PID terminato"
    tail -30 "$LOG"
    exit 1
  fi
done

echo "[TIMEOUT] non in listen dopo 40s"
tail -30 "$LOG"
exit 1
