#!/usr/bin/env bash
echo "=== /tmp/nexus-webide.log ==="
ls -la /tmp/nexus-webide.log 2>&1
tail -40 /tmp/nexus-webide.log

echo ""
echo "=== Processi pnpm/node ==="
pgrep -af pnpm | head -10
echo "---"
pgrep -af node | head -10

echo ""
echo "=== ss listen 3000 ==="
ss -ltnp 2>/dev/null | grep -E ":(3000|3001|3002)"
