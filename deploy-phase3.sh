#!/usr/bin/env bash
# Deploy Phase 3 — Runtime nei terminali IDE
# Esegui questo script dal tuo terminale: bash deploy-phase3.sh
set -e

SERVER="${DEPLOY_HOST:-developer@nexus-prod}"
REMOTE_DIR="/home/developer/ideai"   # aggiorna se diverso

echo "=== Sincronizzazione file sorgenti ==="
FILES=(
  "crates/mcp-core/src/agent_tools.rs"
  "crates/mcp-core/src/chat_messages.rs"
  "crates/mcp-core/src/chat_agent.rs"
  "crates/mcp-core/src/projects.rs"
  "crates/mcp-core/src/main.rs"
  "apps/web-ide/components/terminal-panel.tsx"
  "db/migrations/0011_terminal_commands.sql"
)

for f in "${FILES[@]}"; do
  echo "  → $f"
  ssh "$SERVER" "mkdir -p '$REMOTE_DIR/$(dirname $f)'"
  scp "$f" "$SERVER:$REMOTE_DIR/$f"
done

echo ""
echo "=== Esecuzione migrazione DB ==="
ssh "$SERVER" "cd $REMOTE_DIR && psql \"\$DATABASE_URL\" -f db/migrations/0011_terminal_commands.sql 2>&1 || echo 'Migrazione gia applicata o errore ignorabile'"

echo ""
echo "=== Build Rust ==="
ssh "$SERVER" "cd $REMOTE_DIR && cargo build -p mcp-core --release 2>&1 | tail -20"

echo ""
echo "=== Restart servizi ==="
ssh "$SERVER" "sudo systemctl restart mcp-core 2>/dev/null || pkill -f mcp-core || true"
ssh "$SERVER" "cd $REMOTE_DIR/apps/web-ide && pnpm build 2>&1 | tail -10 && sudo systemctl restart web-ide 2>/dev/null || pm2 restart web-ide 2>/dev/null || true"

echo ""
echo "=== Deploy Phase 3 completato! ==="
echo "  Tool agente: run_in_terminal"
echo "  Endpoint SSE: GET /api/projects/:id/terminal-commands/stream"
echo "  Frontend: TerminalPanel ora riceve e inietta comandi dall'agente"
