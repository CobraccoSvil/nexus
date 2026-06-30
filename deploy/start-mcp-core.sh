#!/bin/bash
# Avvia mcp-core in modo permanente (usato da deploy e CI).
# Può essere chiamato da PowerShell/WSL senza perdere il processo al termine della sessione.
set -a
source /home/administrator/ideai/.env 2>/dev/null || true
set +a
export PATH=/home/administrator/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export HOME=/home/administrator

pkill -f "mcp-core" 2>/dev/null || true
sleep 1

BIN="/home/administrator/ideai/target/release/mcp-core"
LOG="/tmp/nexus-mcp-core.log"

exec setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo "mcp-core avviato (PID=$!), log: $LOG"
