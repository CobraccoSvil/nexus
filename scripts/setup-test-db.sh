#!/usr/bin/env bash
set -uo pipefail

ROOT=/home/administrator/projects/test-metasteps

cat > "$ROOT/.env" <<EOF
DATABASE_URL=postgresql://nexus:nexus@localhost:5434/test_metasteps_dev
POSTGRES_USER=nexus
POSTGRES_PASSWORD=nexus
POSTGRES_DB=test_metasteps_dev
EOF
echo "1. .env aggiornato a :5434"

cat > "$ROOT/docker-compose.yml" <<EOF
services:
  db:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: nexus
      POSTGRES_PASSWORD: nexus
      POSTGRES_DB: test_metasteps_dev
    ports:
      - "5434:5432"
EOF
echo "2. docker-compose.yml aggiornato"

docker exec ideai-postgres-app-1 psql -U nexus -c "CREATE DATABASE test_metasteps_dev" 2>&1 | tail -2
echo "3. CREATE DATABASE eseguito"

docker exec ideai-postgres-app-1 psql -U nexus -d test_metasteps_dev -c "SELECT current_database()" 2>&1 | tail -3
