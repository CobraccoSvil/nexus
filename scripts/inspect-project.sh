#!/usr/bin/env bash
PROJECT_ID="${1:-18543611-8f62-4ef4-9f23-7a5236e52f85}"

ROOT=$(docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -A -c \
  "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', '') FROM projects p LEFT JOIN repositories r ON r.project_id=p.id WHERE p.id='$PROJECT_ID'" 2>&1)
echo "=== root_path: $ROOT ==="

if [ -d "$ROOT" ]; then
  echo ""
  echo "=== ls -la (top-level) ==="
  ls -la "$ROOT" | head -20
  echo ""
  echo "=== File pattern DB rilevanti ==="
  find "$ROOT" -maxdepth 3 -type f \
    \( -name "*.env*" -o -name "docker-compose*.yml" -o -name "package.json" \
       -o -name "Cargo.toml" -o -name "pyproject.toml" -o -name "requirements.txt" \
       -o -name "schema.prisma" -o -name "alembic.ini" -o -name "knexfile*" \) \
    2>/dev/null | head -15
  echo ""
  echo "=== Cartelle migration ==="
  find "$ROOT" -maxdepth 3 -type d -name "migrations" 2>/dev/null | head -5
fi
