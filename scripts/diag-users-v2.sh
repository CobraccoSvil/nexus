#!/usr/bin/env bash
echo "=== TUTTI gli utenti (schema reale: display_name + github_username) ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "
SELECT id, email, display_name, COALESCE(github_username,'-') AS gh, role, deleted_at IS NULL AS active, created_at::date
FROM users
ORDER BY created_at"

echo ""
echo "=== Cerca 'cobracco' ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "
SELECT id, email, display_name, github_username, role
FROM users
WHERE email ILIKE '%cobracco%' OR display_name ILIKE '%cobracco%' OR github_username ILIKE '%cobracco%'"

echo ""
echo "=== github_connections ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\d github_connections" 2>&1 | head -15
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT user_id, github_login, created_at::date FROM github_connections" 2>&1 | head -10

echo ""
echo "=== Quale utente è loggato (sessions) ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\dt *session*" 2>&1 | head -5
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT user_id, COUNT(*) FROM sessions WHERE expires_at > now() GROUP BY user_id" 2>&1 | head -5
