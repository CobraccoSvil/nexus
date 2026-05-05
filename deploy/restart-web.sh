#!/bin/bash
export PATH=/home/administrator/.local/bin:/usr/local/bin:/usr/bin:/bin
export HOME=/home/administrator
pkill -f "next-server" 2>/dev/null || true
pkill -f "next dev" 2>/dev/null || true
sleep 2
cd /opt/ai-orchestrator
nohup pnpm --filter @ai-orchestrator/web-ide dev > /tmp/webide.log 2>&1 &
echo "Web IDE PID: $!"
sleep 6
ss -tlnp | grep 3000
echo "RESTART_OK"
