#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="${PROJECT_DIR:-/opt/ai-orchestrator}"
LOG_DIR="${LOG_DIR:-/tmp}"
MCP_BIN="$PROJECT_DIR/target/release/mcp-core"

usage() {
  cat <<'EOF'
Usage: bash scripts/dev-server-101.sh <command>

Commands:
  setup            [root] Install system dependencies (one-time, run as root)
  status           Show processes, listening ports, and Docker infra containers
  build            Install JS deps if needed and rebuild the Rust binary
  restart          Restart mcp-core and web IDE
  restart-backend  Restart mcp-core only
  restart-web      Restart the web IDE only
  deploy-web       Build web IDE and restart (use after deploying files)
  stop             Stop mcp-core and web IDE
  health           Query local health endpoints
  logs [service]   Tail logs for one service: mcp | web
EOF
}

setup() {
  # Deve essere eseguito come root (sudo bash scripts/dev-server-101.sh setup)
  if [[ "$EUID" -ne 0 ]]; then
    echo "ERROR: 'setup' richiede privilegi root. Esegui:"
    echo "  sudo bash scripts/dev-server-101.sh setup"
    exit 1
  fi

  echo "=== Aggiornamento lista pacchetti ==="
  apt-get update -qq

  echo "=== Dipendenze sistema per Playwright (Chromium headless) ==="
  apt-get install -y --no-install-recommends \
    libatk1.0-0 \
    libatk-bridge2.0-0 \
    libatspi2.0-0 \
    libxcomposite1 \
    libxdamage1 \
    libxfixes3 \
    libxrandr2 \
    libgbm1 \
    libasound2 \
    libpango-1.0-0 \
    libcairo2 \
    libcups2

  echo "=== Installazione browser Playwright (come utente administrator) ==="
  # Scarica i browser Playwright per tutti i progetti che hanno @playwright/test
  local PLAYWRIGHT_HOME="/home/administrator"
  local projects_dir="$PROJECT_DIR/projects"

  find "$projects_dir" -maxdepth 3 -name "package.json" ! -path "*/node_modules/*" | while read -r pkg; do
    local dir
    dir=$(dirname "$pkg")
    if grep -q '"@playwright/test"' "$pkg" 2>/dev/null; then
      echo "→ Installazione browser per: $dir"
      sudo -u administrator bash -c "cd '$dir' && HOME='$PLAYWRIGHT_HOME' pnpm exec playwright install chromium 2>&1 | tail -3" || true
    fi
  done

  echo ""
  echo "✓ Setup completato. Playwright è pronto per i progetti Nexus."
}

load_env() {
  if [[ -f "$PROJECT_DIR/.env" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$PROJECT_DIR/.env"
    set +a
  fi

  export DATABASE_URL="${DATABASE_URL:-}"
  export REDIS_URL="${REDIS_URL:-}"
  export QDRANT_URL="${QDRANT_URL:-http://localhost:6333}"
  export RUST_LOG="${RUST_LOG:-info}"
  export FRONTEND_URL="${FRONTEND_URL:-http://localhost:3000}"
  export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
}

ensure_infra() {
  docker start redis qdrant >/dev/null 2>&1 || true
}

stop_backend() {
  pkill -f "$MCP_BIN" 2>/dev/null || true
  pkill -f "./target/release/mcp-core" 2>/dev/null || true
  pkill -f "target/release/mcp-core" 2>/dev/null || true
  pkill -x "mcp-core" 2>/dev/null || true
  pkill -f "cargo run -p mcp-core" 2>/dev/null || true
}

stop_web() {
  fuser -k 3000/tcp 2>/dev/null || true
  fuser -k 3001/tcp 2>/dev/null || true
  pkill -f "next dev -H 0.0.0.0" 2>/dev/null || true
  pkill -f "next start -H 0.0.0.0 -p 3000" 2>/dev/null || true
  pkill -f "pnpm --filter @ai-orchestrator/web-ide dev" 2>/dev/null || true
  pkill -f "pnpm --filter @ai-orchestrator/web-ide exec next start -H 0.0.0.0 -p 3000" 2>/dev/null || true
  pkill -f "next start" 2>/dev/null || true
}

restart_backend() {
  load_env
  ensure_infra
  stop_backend
  sleep 2

  if [[ ! -x "$MCP_BIN" ]]; then
    echo "mcp-core binary missing, building it first..."
    build
  fi

  cd "$PROJECT_DIR"
  nohup "$MCP_BIN" > "$LOG_DIR/mcp.log" 2>&1 &
  sleep 3
  verify_deploy
}


deploy_web() {
  load_env
  stop_web
  sleep 1
  cd "$PROJECT_DIR"
  echo "Building web-ide..."
  pnpm --filter @ai-orchestrator/web-ide build
  echo "Starting web-ide..."
  nohup pnpm --filter @ai-orchestrator/web-ide exec next start -H 0.0.0.0 -p 3000 > "$LOG_DIR/webide.log" 2>&1 &
  # Attende fino a 30s che next-server sia vivo
  for i in $(seq 1 15); do
    sleep 2
    if pgrep -f "next-server" > /dev/null 2>&1; then
      echo "Web IDE ready."
      break
    fi
    if [ "$i" -eq 15 ]; then
      echo "WARNING: next-server non ancora attivo dopo 30s, controlla /tmp/webide.log"
    fi
  done
}

restart_web() {
  load_env
  stop_web
  sleep 2
  cd "$PROJECT_DIR"
  if [[ ! -f "$PROJECT_DIR/apps/web-ide/.next/BUILD_ID" ]]; then
    echo "ATTENZIONE: .next/BUILD_ID non trovato. Esegui prima 'deploy-web' per buildare il frontend."
    echo "Avvio comunque (potrebbe fallire)..."
  fi
  nohup pnpm --filter @ai-orchestrator/web-ide exec next start -H 0.0.0.0 -p 3000 > "$LOG_DIR/webide.log" 2>&1 &

  # Attendi fino a 40s che il server risponda all'endpoint di versione
  local build_id=""
  echo -n "Attendo frontend pronto (max 40s)"
  for i in $(seq 1 40); do
    sleep 1
    build_id=$(curl -fsS http://127.0.0.1:3000/nexus/version 2>/dev/null | grep -o '"buildId":"[^"]*"' | cut -d'"' -f4 || true)
    if [ -n "$build_id" ]; then
      echo " ✓ OK (buildId=$build_id)"
      return 0
    fi
    echo -n "."
  done
  echo " ✗ Timeout: next-server non risponde dopo 40s"
  echo "   → Controlla i log: tail -30 $LOG_DIR/webide.log"
}

status() {
  echo "== Processes =="
  ps -ef | grep -E "mcp-core|next dev -H 0.0.0.0|next start -H 0.0.0.0 -p 3000|next-server" | grep -v grep || true
  echo
  echo "== Ports =="
  ss -tlnp | grep -E "3000|4000|50051|5432|6379|6333|6334" || true
  echo
  echo "== Docker =="
  docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}' | sed -n '1,20p'
}

build() {
  load_env
  cd "$PROJECT_DIR"

  if [[ -f "pnpm-lock.yaml" ]]; then
    pnpm install --frozen-lockfile
    pnpm --filter @ai-orchestrator/web-ide build
  fi

  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  # Salva il timestamp di build per il verify_deploy
  local build_start
  build_start=$(date +%s)
  echo "$build_start" > "$PROJECT_DIR/.last_build_ts"

  cargo build -p mcp-core --release
}

health() {
  echo "== MCP Core =="
  curl -fsS http://127.0.0.1:4000/api/health 2>/dev/null || echo "non risponde"
  echo
  echo "== Web IDE =="
  local version_json
  version_json=$(curl -fsS http://127.0.0.1:3000/nexus/version 2>/dev/null || true)
  if [ -n "$version_json" ]; then
    local build_id build_date
    build_id=$(echo "$version_json" | grep -o '"buildId":"[^"]*"' | cut -d'"' -f4 || echo "?")
    build_date=$(echo "$version_json" | grep -o '"buildDate":"[^"]*"' | cut -d'"' -f4 || echo "?")
    echo "  buildId  = $build_id"
    echo "  built at = $build_date"
    echo "  uptime   = $(echo "$version_json" | grep -o '"uptime":[0-9.]*' | cut -d: -f2 || echo "?")s"
  else
    echo "  non risponde (porta 3000)"
  fi
}

# Verifica che il binario in esecuzione sia quello appena compilato.
# Confronta il build_time esposto da /api/health con il timestamp del binario su disco.
verify_deploy() {
  local max_wait=15
  local i=0
  echo -n "Verifica deploy mcp-core"
  while [ "$i" -lt "$max_wait" ]; do
    local running_build
    running_build=$(curl -fsS http://127.0.0.1:4000/api/health 2>/dev/null | grep -o '"build_time":"[^"]*"' | cut -d'"' -f4)
    if [ -n "$running_build" ]; then
      # Il BUILD_TIMESTAMP è il momento di inizio compilazione.
      # Il binario su disco viene scritto DOPO la compilazione (può essere 2-3 min dopo).
      # Verifichiamo che build_time del processo attivo sia >= quello salvato nell'ultimo build.
      local expected_build
      expected_build=$(cat "$PROJECT_DIR/.last_build_ts" 2>/dev/null || echo "0")
      if [ "$running_build" -ge "$expected_build" ] 2>/dev/null; then
        echo " ✓ OK (build=$running_build)"
        return 0
      else
        echo " ⚠ Binario vecchio in esecuzione! Attivo: build=$running_build, Atteso: >=$expected_build"
        echo "   → Riavvio forzato..."
        pkill -f "$MCP_BIN" 2>/dev/null || true
        sleep 1
        nohup "$MCP_BIN" > "$LOG_DIR/mcp.log" 2>&1 &
        sleep 3
        local new_build
        new_build=$(curl -fsS http://127.0.0.1:4000/api/health 2>/dev/null | grep -o '"build_time":"[^"]*"' | cut -d'"' -f4)
        echo " ✓ Riavviato (build=$new_build)"
        return 0
      fi
    fi
    echo -n "."
    sleep 1
    i=$((i + 1))
  done
  echo " ✗ Timeout: mcp-core non risponde dopo ${max_wait}s"
  return 1
}

logs() {
  local service="${1:-}"
  local lines="${LINES:-120}"
  case "$service" in
    mcp) tail -n "$lines" "$LOG_DIR/mcp.log" ;;
    web) tail -n "$lines" "$LOG_DIR/webide.log" ;;
    *)
      echo "Choose one service: mcp | web" >&2
      exit 1
      ;;
  esac
}

command="${1:-status}"

case "$command" in
  setup)
    setup
    ;;
  status)
    status
    ;;
  build)
    build
    ;;
  restart)
    restart_backend
    restart_web
    status
    ;;
  restart-backend)
    restart_backend
    status
    ;;
  restart-web)
    restart_web
    status
    ;;
  deploy-web)
    deploy_web
    status
    ;;
  stop)
    stop_web
    stop_backend
    status
    ;;
  health)
    health
    ;;
  logs)
    shift || true
    logs "${1:-}"
    ;;
  *)
    usage
    exit 1
    ;;
esac
