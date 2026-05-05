#!/bin/bash
export PATH=/home/administrator/.local/bin:/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export HOME=/home/administrator
cd /opt/ai-orchestrator
source .env
export DATABASE_URL REDIS_URL NEURAL_CORE_URL QDRANT_URL RUST_LOG

# Kill existing
pkill -f "brain.grpc_server" 2>/dev/null || true
pkill -f "mcp-core" 2>/dev/null || true
pkill -f "next" 2>/dev/null || true
sleep 1

# Neural Core (REST su 0.0.0.0:8001 + gRPC su 50051)
nohup python3 -m brain.grpc_server.main --rest > /tmp/neural.log 2>&1 &
echo "Neural Core PID: $!"
sleep 5

# MCP Core Rust (:4000)
nohup cargo run -p mcp-core > /tmp/mcp.log 2>&1 &
echo "MCP Core PID: $!"
sleep 3

# Web IDE (:3000)
nohup pnpm --filter @ai-orchestrator/web-ide dev > /tmp/webide.log 2>&1 &
echo "Web IDE PID: $!"

sleep 5
echo "=== Checking ports ==="
ss -tlnp | grep -E '3000|4000|8001|50051' || echo "waiting for ports..."
echo "DONE"
