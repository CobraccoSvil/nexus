#!/usr/bin/env bash
echo "=== Schema tabella projects ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "\d projects" 2>&1 | head -25

echo ""
echo "=== Owner / user dei progetti ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT p.name, p.user_id, p.is_active, u.email FROM projects p LEFT JOIN users u ON u.id = p.user_id ORDER BY p.created_at DESC"

echo ""
echo "=== Verifica /api/projects/mine usa quale filtro ==="
grep -rn "fn list_my_projects\|fn list_projects\|fn projects_mine\|api/projects/mine\|projects_mine" /home/administrator/ideai/crates/mcp-core/src/ 2>/dev/null | head -5
