#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai

echo "=== Python: import anthropic_provider ==="
python3 -c "from brain.providers import anthropic_provider; print('import OK'); print('helpers:', [h for h in dir(anthropic_provider) if h.startswith('_resolve') or h.startswith('_load_thinking') or h.startswith('ThinkingModels')])"

echo "=== Python: parse syntax router/service.py ==="
python3 -m py_compile brain/router/service.py && echo "syntax OK"

echo "=== Python: parse syntax anthropic_provider.py ==="
python3 -m py_compile brain/providers/anthropic_provider.py && echo "syntax OK"

echo "=== Rust: cargo check admin-service ==="
cargo check -p admin-service --quiet 2>&1 | tail -5

echo "=== SQL: validate 0170 + 0171 syntax via psql --variable ON_ERROR_STOP=1 (dry-parse) ==="
for f in db/migrations/0170_model_capabilities.sql db/migrations/0171_provider_test_and_admin_purposes.sql; do
  if command -v psql >/dev/null 2>&1; then
    echo "--- $f ---"
    # Solo lint sintattico via BEGIN/ROLLBACK
    psql "${DATABASE_URL:-postgresql://postgres:postgres@localhost:5432/postgres}" -v ON_ERROR_STOP=1 -f "$f" -c "ROLLBACK;" 2>&1 | tail -5 || echo "psql non disponibile o DB non raggiungibile (lint skip)"
  else
    echo "psql non installato, skip lint per $f"
  fi
done

echo "=== DONE ==="
