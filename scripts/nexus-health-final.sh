#!/usr/bin/env bash
LOG=/tmp/nexus-mcp-core.log
echo "=== Migrazioni applicate da mcp-core (ultime righe) ==="
grep -iE 'migration|sqlx_migrations|listening|started' "$LOG" | tail -10

echo ""
echo "=== Health endpoint per porta ==="
for p in 3000 4000 4010 4020 4030 4040 4050 4055 4060; do
  code=$(curl -s -o /dev/null -w "%{http_code}" "http://localhost:$p/health" 2>/dev/null || echo 000)
  printf "  :%-5s %s\n" "$p" "$code"
done

echo ""
echo "=== Verifica migrazioni nel DB ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -c \
  "SELECT version FROM _sqlx_migrations WHERE version >= 168 ORDER BY version" 2>/dev/null || \
  docker exec ideai-postgres-nexus-1 psql -U postgres -d nexus -t -c \
  "SELECT version FROM _sqlx_migrations WHERE version >= 168 ORDER BY version" 2>/dev/null || \
  echo "(impossibile leggere migrations dal DB)"

echo ""
echo "=== Verifica capability thinking popolata (mig 0170) ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -c \
  "SELECT model, capabilities FROM ai_price_catalog WHERE (capabilities->>'thinking')::boolean = true" 2>/dev/null || \
  docker exec ideai-postgres-nexus-1 psql -U postgres -d nexus -t -c \
  "SELECT model, capabilities FROM ai_price_catalog WHERE (capabilities->>'thinking')::boolean = true" 2>/dev/null || \
  echo "(impossibile leggere ai_price_catalog)"

echo ""
echo "=== Verifica purpose model (mig 0171) ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -c \
  "SELECT purpose, provider, model_id FROM nexus_purpose_model WHERE purpose IN ('provider_test_connection.anthropic', 'admin.tool_selection')" 2>/dev/null || \
  docker exec ideai-postgres-nexus-1 psql -U postgres -d nexus -t -c \
  "SELECT purpose, provider, model_id FROM nexus_purpose_model WHERE purpose IN ('provider_test_connection.anthropic', 'admin.tool_selection')" 2>/dev/null || \
  echo "(impossibile leggere nexus_purpose_model)"
