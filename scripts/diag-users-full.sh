#!/usr/bin/env bash
echo "=== Schema tabella users ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\d users" 2>&1 | head -20

echo ""
echo "=== TUTTI gli utenti ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT id, email, COALESCE(username, '-') AS username, role, created_at::date FROM users ORDER BY created_at"

echo ""
echo "=== Tabella github_accounts / auth_providers ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\dt *github*" 2>&1 | head -10
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\dt *auth*" 2>&1 | head -10

echo ""
echo "=== Cerca 'cobraccosvil' o 'cobracco' o 'svil' ovunque ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "
SELECT 'users' AS tbl, id::text, email::text
FROM users
WHERE email ILIKE '%cobracco%' OR email ILIKE '%svil%' OR username ILIKE '%cobracco%'"
