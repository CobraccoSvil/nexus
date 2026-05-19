#!/usr/bin/env bash
set -uo pipefail
exec > /tmp/nexus-rebuild.log 2>&1

echo "=== START $(date -Iseconds) ==="
pkill -9 -f next-server 2>/dev/null || true
pkill -9 -f "next start" 2>/dev/null || true
sleep 2

cd /home/administrator/ideai/apps/web-ide
rm -rf .next
echo "=== rm .next OK ==="

pnpm build
RC=$?
echo "=== pnpm build EXIT=$RC ==="

if [ $RC -ne 0 ]; then
  echo "build failed"
  exit $RC
fi

# Avvia next start in detached
nohup pnpm exec next start -p 3000 > /tmp/nexus-webide.log 2>&1 &
disown
echo "=== next start lanciato PID=$! ==="

# Attendi listen
for i in $(seq 1 30); do
  sleep 1
  if ss -ltn '( sport = :3000 )' 2>/dev/null | grep -q ':3000'; then
    echo "=== :3000 listening (after ${i}s) ==="
    exit 0
  fi
done
echo "=== TIMEOUT :3000 ==="
exit 1
