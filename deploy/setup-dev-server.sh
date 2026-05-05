#!/usr/bin/env bash
# =============================================================================
# AI-Orchestrator v2 — Setup ambiente di sviluppo completo
# Target: Ubuntu/Debian sul server di produzione
# Eseguire come root: bash setup-dev-server.sh
# =============================================================================
set -euo pipefail

echo "============================================"
echo " AI-Orchestrator v2 — Dev Environment Setup"
echo "============================================"

export DEBIAN_FRONTEND=noninteractive

# --- 1. Aggiornamento sistema ---
echo "[1/9] Aggiornamento sistema..."
apt-get update -qq
apt-get upgrade -y -qq

# --- 2. Pacchetti base ---
echo "[2/9] Installazione pacchetti base..."
apt-get install -y -qq \
  build-essential pkg-config libssl-dev libpq-dev \
  curl wget git unzip jq \
  ca-certificates gnupg lsb-release \
  protobuf-compiler

# --- 3. Docker ---
echo "[3/9] Installazione Docker..."
if ! command -v docker &>/dev/null; then
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] \
    https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update -qq
  apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-compose-plugin
  systemctl enable docker --now
  echo "  Docker installato: $(docker --version)"
else
  echo "  Docker gia' presente: $(docker --version)"
fi

# --- 4. Redis e Qdrant via Docker ---
echo "[4/9] Avvio Redis e Qdrant..."
docker pull redis:7-alpine -q
docker pull qdrant/qdrant:v1.13.4 -q

# Redis
if ! docker ps --format '{{.Names}}' | grep -q '^redis$'; then
  docker run -d --name redis --restart unless-stopped \
    -p 6379:6379 redis:7-alpine
  echo "  Redis avviato su :6379"
else
  echo "  Redis gia' in esecuzione"
fi

# Qdrant
if ! docker ps --format '{{.Names}}' | grep -q '^qdrant$'; then
  docker run -d --name qdrant --restart unless-stopped \
    -p 6333:6333 -p 6334:6334 \
    -v qdrant_storage:/qdrant/storage \
    qdrant/qdrant:v1.13.4
  echo "  Qdrant avviato su :6333"
else
  echo "  Qdrant gia' in esecuzione"
fi

# --- 5. Configurazione PostgreSQL ---
echo "[5/9] Configurazione PostgreSQL..."
if command -v psql &>/dev/null; then
  # Crea database e utente se non esistono
  sudo -u postgres psql -tc "SELECT 1 FROM pg_database WHERE datname = 'ai_orchestrator'" \
    | grep -q 1 || sudo -u postgres psql -c "CREATE DATABASE ai_orchestrator;"
  sudo -u postgres psql -tc "SELECT 1 FROM pg_roles WHERE rolname = 'orchestrator'" \
    | grep -q 1 || sudo -u postgres psql -c "CREATE USER orchestrator WITH PASSWORD 'orchestrator_dev_2024';"
  sudo -u postgres psql -c "GRANT ALL PRIVILEGES ON DATABASE ai_orchestrator TO orchestrator;"
  sudo -u postgres psql -d ai_orchestrator -c "GRANT ALL ON SCHEMA public TO orchestrator;"

  # Abilita connessioni dalla rete locale
  PG_CONF=$(find /etc/postgresql -name postgresql.conf 2>/dev/null | head -1)
  PG_HBA=$(find /etc/postgresql -name pg_hba.conf 2>/dev/null | head -1)
  if [ -n "$PG_CONF" ]; then
    grep -q "listen_addresses = '\*'" "$PG_CONF" || {
      sed -i "s/#listen_addresses = 'localhost'/listen_addresses = '*'/" "$PG_CONF"
      echo "  PostgreSQL: listen_addresses impostato a *"
    }
  fi
  if [ -n "$PG_HBA" ]; then
    grep -q "192.168.0.0/24" "$PG_HBA" || {
      echo "host all all 192.168.0.0/24 scram-sha-256" >> "$PG_HBA"
      echo "  PostgreSQL: accesso rete locale abilitato"
    }
  fi
  systemctl restart postgresql
  echo "  PostgreSQL configurato (db: ai_orchestrator, user: orchestrator)"
else
  echo "  ATTENZIONE: psql non trovato, PostgreSQL potrebbe non essere installato"
fi

# --- 6. Rust ---
echo "[6/9] Installazione Rust..."
if ! command -v rustup &>/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  source "$HOME/.cargo/env"
  echo "  Rust installato: $(rustc --version)"
else
  rustup update stable -q
  source "$HOME/.cargo/env"
  echo "  Rust gia' presente: $(rustc --version)"
fi

# --- 7. Python 3.12+ ---
echo "[7/9] Installazione Python..."
if ! command -v python3.12 &>/dev/null; then
  apt-get install -y -qq software-properties-common
  add-apt-repository -y ppa:deadsnakes/ppa
  apt-get update -qq
  apt-get install -y -qq python3.12 python3.12-venv python3.12-dev python3-pip
fi
# Alias python
update-alternatives --install /usr/bin/python python /usr/bin/python3.12 1 2>/dev/null || true
echo "  Python: $(python3.12 --version)"

# --- 8. Node.js 22 + pnpm ---
echo "[8/9] Installazione Node.js e pnpm..."
if ! command -v node &>/dev/null; then
  curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
  apt-get install -y -qq nodejs
fi
npm install -g pnpm@latest 2>/dev/null || true
echo "  Node: $(node --version), pnpm: $(pnpm --version)"

# --- 9. Claude Code ---
echo "[9/9] Installazione Claude Code..."
if ! command -v claude &>/dev/null; then
  npm install -g @anthropic-ai/claude-code
  echo "  Claude Code installato: $(claude --version 2>/dev/null || echo 'ok')"
else
  echo "  Claude Code gia' presente"
fi

# --- Creazione file .env ---
PROJECT_DIR="/opt/ai-orchestrator"
echo ""
echo "============================================"
echo " Creazione directory progetto: $PROJECT_DIR"
echo "============================================"
mkdir -p "$PROJECT_DIR"

cat > "$PROJECT_DIR/.env" << 'ENVEOF'
# AI-Orchestrator v2 — Environment
DATABASE_URL=postgres://orchestrator:orchestrator_dev_2024@localhost:5432/ai_orchestrator
REDIS_URL=redis://localhost:6379
NEURAL_CORE_URL=http://localhost:50051
QDRANT_URL=http://localhost:6333
RUST_LOG=info

# LLM API Keys (inserire le proprie chiavi)
# OPENAI_API_KEY=sk-...
# ANTHROPIC_API_KEY=sk-ant-...
# GOOGLE_API_KEY=...
ENVEOF

# --- Script di avvio rapido ---
cat > "$PROJECT_DIR/start-all.sh" << 'STARTEOF'
#!/usr/bin/env bash
# Avvia tutti i servizi AI-Orchestrator v2
set -e
cd "$(dirname "$0")"
source .env
export DATABASE_URL REDIS_URL NEURAL_CORE_URL QDRANT_URL RUST_LOG

echo "Avvio Neural Core (Python gRPC + REST)..."
python3.12 -m brain.grpc_server.main --rest &
NEURAL_PID=$!
sleep 2

echo "Avvio MCP Server Core (Rust :4000)..."
cargo run -p mcp-core --release &
RUST_PID=$!
sleep 2

echo "Avvio Web IDE (Next.js :3000)..."
pnpm --filter @ai-orchestrator/web-ide dev &
WEB_PID=$!

echo ""
echo "=== Servizi avviati ==="
echo "  Web IDE:      http://localhost:3000"
echo "  MCP Core:     http://localhost:4000/api/health"
echo "  Neural REST:  http://localhost:8001/health"
echo "  Neural gRPC:  localhost:50051"
echo ""
echo "PID: Neural=$NEURAL_PID Rust=$RUST_PID Web=$WEB_PID"
echo "Per fermare: kill $NEURAL_PID $RUST_PID $WEB_PID"
wait
STARTEOF
chmod +x "$PROJECT_DIR/start-all.sh"

# --- Riepilogo ---
echo ""
echo "============================================"
echo " SETUP COMPLETATO!"
echo "============================================"
echo ""
echo " Prossimi passi:"
echo ""
echo " 1. Clona il repo nella directory del progetto:"
echo "    cd $PROJECT_DIR"
echo "    git clone <repo-url> ."
echo ""
echo " 2. Installa dipendenze Python:"
echo "    pip install sentence-transformers qdrant-client openai anthropic google-generativeai grpcio grpcio-tools protobuf fastapi uvicorn httpx numpy tiktoken"
echo ""
echo " 3. Installa dipendenze Node:"
echo "    pnpm install"
echo ""
echo " 4. Compila il progetto Rust:"
echo "    source ~/.cargo/env"
echo "    cargo build --workspace"
echo ""
echo " 5. Esegui le migrazioni DB:"
echo "    cargo run -p mcp-core  (le esegue automaticamente al primo avvio)"
echo ""
echo " 6. Configura le API key in $PROJECT_DIR/.env"
echo ""
echo " 7. Avvia tutto:"
echo "    cd $PROJECT_DIR && ./start-all.sh"
echo ""
echo " 8. Avvia Claude Code:"
echo "    cd $PROJECT_DIR && claude"
echo ""
echo " Servizi disponibili:"
echo "   PostgreSQL:  localhost:5432 (db: ai_orchestrator, user: orchestrator)"
echo "   Redis:       localhost:6379"
echo "   Qdrant:      localhost:6333"
echo "============================================"
