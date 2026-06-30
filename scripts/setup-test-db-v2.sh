#!/usr/bin/env bash
set -uo pipefail

# postgres-app usa user=nexus_admin (env del container)
# Aggiorno .env con utente corretto
ROOT=/home/administrator/projects/test-metasteps

cat > "$ROOT/.env" <<EOF
DATABASE_URL=postgresql://nexus_admin:nexus_admin_secret@localhost:5434/test_metasteps_dev
POSTGRES_USER=nexus_admin
POSTGRES_PASSWORD=nexus_admin_secret
POSTGRES_DB=test_metasteps_dev
EOF
echo "1. .env aggiornato a nexus_admin@:5434"

cat > "$ROOT/docker-compose.yml" <<EOF
services:
  db:
    image: postgres:17-alpine
    environment:
      POSTGRES_USER: nexus_admin
      POSTGRES_PASSWORD: nexus_admin_secret
      POSTGRES_DB: test_metasteps_dev
    ports:
      - "5434:5432"
EOF
echo "2. docker-compose.yml aggiornato"

docker exec -e PGPASSWORD=nexus_admin_secret ideai-postgres-app-1 \
  psql -U nexus_admin -d postgres -c "CREATE DATABASE test_metasteps_dev" 2>&1 | tail -2
echo "3. CREATE DATABASE eseguito"

docker exec -e PGPASSWORD=nexus_admin_secret ideai-postgres-app-1 \
  psql -U nexus_admin -d test_metasteps_dev -c "SELECT current_database()" 2>&1 | tail -3
