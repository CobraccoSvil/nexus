#!/usr/bin/env bash
# Pre-flight check per la migrazione on-prem.
# Valida che il sistema host abbia i requisiti minimi PRIMA di scaricare
# il modello vLLM (~60GB) e avviare lo stack completo.
#
# Uso: ./scripts/onprem-preflight.sh
# Exit: 0 se tutti i check critici passano, 1 altrimenti.

set -uo pipefail

PASS=0
WARN=0
FAIL=0

ok()   { echo "[ OK ]   $*"; PASS=$((PASS + 1)); }
warn() { echo "[WARN]  $*"; WARN=$((WARN + 1)); }
fail() { echo "[FAIL]  $*"; FAIL=$((FAIL + 1)); }

echo ""
echo "═══════════════════════════════════════════════════"
echo "  Nexus on-prem migration — pre-flight checks"
echo "═══════════════════════════════════════════════════"
echo ""

# ── 1. CLI tool richiesti dallo smoke test ─────────────────────────────────
# node sostituisce python3: onprem-smoke.sh parsa il JSON con node (jq non e'
# garantito sull'ambiente), e nel repo non esiste piu' codice Python.
for tool in docker curl node pg_isready; do
  if command -v "$tool" &>/dev/null; then
    ok "$tool disponibile ($(command -v $tool))"
  else
    fail "$tool mancante — il runbook on-prem lo richiede"
  fi
done

# ── 2. Docker daemon ───────────────────────────────────────────────────────
if docker info &>/dev/null; then
  ok "docker daemon raggiungibile"
else
  fail "docker daemon non raggiungibile — verificare che il servizio sia attivo"
fi

# ── 3. Docker Compose v2 ───────────────────────────────────────────────────
if docker compose version &>/dev/null; then
  ok "docker compose v2 disponibile ($(docker compose version --short 2>/dev/null || echo unknown))"
else
  fail "docker compose v2 mancante — installare Compose Plugin"
fi

# ── 4. GPU NVIDIA + nvidia-container-toolkit ───────────────────────────────
if command -v nvidia-smi &>/dev/null; then
  if nvidia-smi -L &>/dev/null; then
    GPU=$(nvidia-smi --query-gpu=name --format=csv,noheader | head -1)
    VRAM=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits | head -1 || echo 0)
    if [ "$VRAM" -ge 40000 ] 2>/dev/null; then
      ok "GPU rilevata: $GPU (${VRAM}MB VRAM, OK per Qwen2.5-Coder-32B)"
    else
      warn "GPU rilevata: $GPU (${VRAM}MB VRAM) — meno di 40GB raccomandati; usa profilo cpu-test o modello piu' piccolo"
    fi
  else
    warn "nvidia-smi presente ma nessuna GPU enumerata"
  fi
else
  warn "nvidia-smi mancante — usa profilo cpu-test (vllm-cpu) per validare senza GPU"
fi

# Test nvidia-container-toolkit (solo se GPU presente)
if command -v nvidia-smi &>/dev/null && docker info &>/dev/null; then
  if docker run --rm --gpus all nvidia/cuda:12.0.0-base-ubuntu22.04 nvidia-smi -L &>/dev/null; then
    ok "nvidia-container-toolkit configurato (docker --gpus all funzionante)"
  else
    warn "docker --gpus all non riesce — installare nvidia-container-toolkit e riavviare docker"
  fi
fi

# ── 5. Risorse host (RAM/disco) ────────────────────────────────────────────
RAM_GB=$(awk '/MemTotal/ {printf "%.0f", $2/1024/1024}' /proc/meminfo 2>/dev/null || echo 0)
if [ "$RAM_GB" -ge 64 ] 2>/dev/null; then
  ok "RAM: ${RAM_GB}GB (OK, raccomandato ≥ 64GB)"
elif [ "$RAM_GB" -ge 32 ]; then
  warn "RAM: ${RAM_GB}GB (sotto la raccomandazione di 64GB)"
else
  fail "RAM: ${RAM_GB}GB — insufficiente per vLLM 32B"
fi

DISK_GB=$(df -BG /var/lib/docker 2>/dev/null | awk 'NR==2 {gsub("G","",$4); print $4}' || echo 0)
if [ "$DISK_GB" -ge 100 ] 2>/dev/null; then
  ok "Disco /var/lib/docker: ${DISK_GB}GB liberi (OK per cache vLLM)"
elif [ "$DISK_GB" -ge 60 ]; then
  warn "Disco /var/lib/docker: ${DISK_GB}GB liberi (limite per Qwen 32B ~60GB)"
else
  fail "Disco /var/lib/docker: ${DISK_GB}GB liberi — insufficiente"
fi

# ── 6. File richiesti dal runbook ──────────────────────────────────────────
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for f in \
  "$ROOT/.env.example" \
  "$ROOT/infra/docker/docker-compose.onprem.yml" \
  "$ROOT/infra/sql/init-schemas.sql" \
  "$ROOT/infra/sql/rls-policies.sql" \
  "$ROOT/scripts/onprem-smoke.sh"
do
  if [ -r "$f" ]; then
    ok "file presente: ${f#$ROOT/}"
  else
    fail "file mancante: ${f#$ROOT/}"
  fi
done

# ── 7. Validazione sintattica compose ──────────────────────────────────────
if docker compose -f "$ROOT/infra/docker/docker-compose.onprem.yml" config --quiet &>/dev/null; then
  ok "docker-compose.onprem.yml sintatticamente valido"
else
  fail "docker-compose.onprem.yml ha errori di sintassi"
fi

# ── 8. .env.onprem (se presente, suggerimenti) ─────────────────────────────
if [ -r "$ROOT/.env.onprem" ]; then
  ok ".env.onprem presente"
  for key in NEXUS_PROFILE VLLM_MODEL POSTGRES_PASSWORD LANGFUSE_SECRET JWT_SECRET; do
    if grep -qE "^${key}=" "$ROOT/.env.onprem" 2>/dev/null; then
      ok "  .env.onprem contiene $key"
    else
      warn "  .env.onprem manca $key (vedi docs/migration-to-onprem.md §1.2)"
    fi
  done
else
  warn ".env.onprem non trovato — creare da .env.example prima del deploy"
fi

# ── 9. Riepilogo ───────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  Pre-flight: $PASS OK, $WARN warning, $FAIL fail"
echo "═══════════════════════════════════════════════════"
if [ $FAIL -eq 0 ]; then
  echo "Sistema pronto per ./docker compose -f infra/docker/docker-compose.onprem.yml up -d"
  exit 0
else
  echo "Risolvere i FAIL prima di procedere con il deploy."
  exit 1
fi
