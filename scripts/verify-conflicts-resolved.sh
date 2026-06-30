#!/usr/bin/env bash
cd /home/administrator/ideai

echo "=== Conflict markers residui (deve essere 0) ==="
grep -rcE '<<<<<<<|=======|>>>>>>>' apps/web-ide/components/ 2>/dev/null | grep -v ':0$' | head -10 || echo "none"
echo ""
echo "=== Git status ==="
git add apps/web-ide/components/chat/message-list.tsx 2>&1
git status --short | head -30
echo ""
echo "=== Typecheck ==="
pnpm --filter @ai-orchestrator/web-ide typecheck 2>&1 | tail -10
