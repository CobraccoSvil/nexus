#!/usr/bin/env bash
set -uo pipefail
exec > /tmp/nexus-rebuild2.log 2>&1

echo "=== START $(date -Iseconds) FORCE REBUILD ==="
pkill -9 -f next-server 2>/dev/null || true
fuser -k -9 3000/tcp 2>/dev/null || true
sleep 2

cd /home/administrator/ideai/apps/web-ide
rm -rf .next .turbo
echo "=== .next + .turbo eliminati ==="

# Verifica sorgenti
echo ""
echo "=== Verifica file ide-shell.tsx attuale ==="
grep -n "useProjectDispatcher" components/ide-shell.tsx | head -3
echo ""

# Verifica modulo dispatcher esiste
ls -la lib/project-dispatcher/
echo ""

# Build fresh
pnpm build
RC=$?
echo "=== pnpm build EXIT=$RC ==="

if [ $RC -ne 0 ]; then exit $RC; fi

# Verifica bundle generato
echo ""
echo "=== Bundle verifica useProjectDispatcher (mangled) ==="
for kw in "/event-stream" "event-stream" "/snapshot?topics" "JobCreated" "FileChanged"; do
  count=$(grep -c "$kw" .next/static/chunks/*.js .next/static/chunks/app/**/*.js 2>/dev/null | awk -F: '{s+=$2} END {print s+0}')
  echo "  '$kw': $count occorrenze totali"
done

# Restart Next
nohup pnpm exec next start -p 3000 > /tmp/nexus-webide.log 2>&1 &
disown
echo ""
echo "=== next start PID=$! ==="
for i in $(seq 1 20); do
  sleep 1
  if ss -ltn '( sport = :3000 )' 2>/dev/null | grep -q ':3000'; then
    echo "=== :3000 listening ${i}s ==="
    break
  fi
done
