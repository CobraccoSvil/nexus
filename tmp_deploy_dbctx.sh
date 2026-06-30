#!/bin/bash
set -e
SRC="/mnt/d/Sviluppo/ideai/fervent-cohen-fad855/crates/mcp-core/src/chat_messages.rs"
DST="/home/administrator/ideai/crates/mcp-core/src/chat_messages.rs"
SRC2="/mnt/d/Sviluppo/ideai/fervent-cohen-fad855/crates/mcp-core/src/agent_types.rs"
DST2="/home/administrator/ideai/crates/mcp-core/src/agent_types.rs"
cp "$SRC" "$DST"
cp "$SRC2" "$DST2"
echo "Copiati chat_messages.rs e agent_types.rs"

cd /home/administrator/ideai
echo "Build mcp-core..."
cargo build --release -p mcp-core 2>&1 | tail -5
echo "Riavvio mcp-core..."
pkill -f "target/release/mcp-core" 2>/dev/null || true
sleep 1
setsid nohup ./target/release/mcp-core > /tmp/nexus-mcp-core.log 2>&1 < /dev/null &
echo "mcp-core riavviato (PID: $!)"
sleep 2
curl -s -o /dev/null -w "mcp-core HTTP: %{http_code}\n" http://localhost:4000/health 2>/dev/null || echo "mcp-core non raggiungibile (potrebbe usare altra porta)"
echo "Fatto."
