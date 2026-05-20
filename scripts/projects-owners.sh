#!/usr/bin/env bash
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "
SELECT p.name, p.owner_user_id, u.email, p.team_id
FROM projects p
LEFT JOIN users u ON u.id = p.owner_user_id
ORDER BY p.created_at DESC"

echo ""
echo "=== Members del team (chi vede cosa) ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\dt *member*" 2>&1 | head -10
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\dt *team*" 2>&1 | head -10
