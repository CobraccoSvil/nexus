#!/bin/bash
# Restart all IDEAI microservices
# Usage: ./restart-microservices.sh [service_name]
#   No args: restart all
#   With arg: restart only that service (e.g., ./restart-microservices.sh chat-service)

set -e
export PATH=/home/administrator/.local/bin:/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export HOME=/home/administrator
cd /opt/ai-orchestrator
source .env
export DATABASE_URL REDIS_URL NEURAL_CORE_URL QDRANT_URL RUST_LOG FRONTEND_URL

SERVICE="${1:-all}"

stop_service() {
    local name="$1"
    pkill -f "$name" 2>/dev/null || true
}

RELEASE_DIR="/opt/ai-orchestrator/target/release"

start_service() {
    local name="$1"
    local log="/tmp/${name}.log"
    local bin="${RELEASE_DIR}/${name}"
    if [ ! -f "$bin" ]; then
        echo "  WARNING: $bin not found, using cargo run..."
        nohup cargo run -p "$name" --release > "$log" 2>&1 &
    else
        echo "Starting $name..."
        nohup "$bin" > "$log" 2>&1 &
    fi
    echo "  PID: $! -> $log"
}

if [ "$SERVICE" = "all" ]; then
    echo "=== Stopping all services ==="
    stop_service "brain.grpc_server"
    stop_service "mcp-core"
    stop_service "admin-service"
    stop_service "chat-service"
    stop_service "doc-service"
    stop_service "billing-service"
    stop_service "plugin-service"
    stop_service "next-server"
    sleep 2

    echo "=== Starting Neural Core ==="
    nohup python3 -m brain.grpc_server.main --rest > /tmp/neural.log 2>&1 &
    echo "  PID: $!"
    sleep 5

    echo "=== Starting Core Platform ==="
    start_service "mcp-core"
    sleep 3

    echo "=== Starting Microservices ==="
    start_service "admin-service"
    start_service "billing-service"
    start_service "chat-service"
    start_service "doc-service"
    start_service "plugin-service"
    sleep 3

    echo "=== Starting Web IDE ==="
    nohup pnpm --filter @ai-orchestrator/web-ide dev > /tmp/webide.log 2>&1 &
    echo "  PID: $!"
    sleep 5
else
    echo "=== Restarting $SERVICE ==="
    stop_service "$SERVICE"
    sleep 1
    start_service "$SERVICE"
    sleep 2
fi

echo ""
echo "=== Active Ports ==="
ss -tlnp | grep -E '3000|4000|4010|4020|4030|4040|4050|8001|50051' || true
echo ""
echo "=== Health Checks ==="
for port in 4000 4010 4020 4030 4040 4050; do
    status=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${port}/health" 2>/dev/null || echo "down")
    echo "  :${port} -> ${status}"
done
echo ""
echo "DONE"
