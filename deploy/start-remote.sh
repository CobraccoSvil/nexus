#!/bin/bash
export PATH=/home/administrator/.local/bin:/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export HOME=/home/administrator
cd /opt/ai-orchestrator
source .env
export DATABASE_URL REDIS_URL QDRANT_URL RUST_LOG

# Kill existing
pkill -f "mcp-core" 2>/dev/null || true
pkill -f "next" 2>/dev/null || true
sleep 1

# MCP Core Rust (:4000)
nohup cargo run -p mcp-core > /tmp/mcp.log 2>&1 &
echo "MCP Core PID: $!"
sleep 3

# Web IDE (:3000)
nohup pnpm --filter @ai-orchestrator/web-ide dev > /tmp/webide.log 2>&1 &
echo "Web IDE PID: $!"

sleep 5
echo "=== Checking ports ==="
ss -tlnp | grep -E '3000|4000' || echo "waiting for ports..."
echo "DONE"
