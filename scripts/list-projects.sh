#!/usr/bin/env bash
echo "=== Tutti i progetti in nexus DB ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT id, name, status, created_at::date FROM projects ORDER BY created_at DESC"

echo ""
echo "=== Project roots ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT p.name, COALESCE(r.root_path, p.analysis_json->>'rootPath', '(none)') AS root FROM projects p LEFT JOIN repositories r ON r.project_id=p.id ORDER BY p.created_at DESC"

echo ""
echo "=== Tabella users ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -c "SELECT id, email, role FROM users LIMIT 5"
