#!/usr/bin/env bash
# Applica tutte le migrazioni db/migrations/ al database nexus su server-remoto.
# Da eseguire una sola volta (o dopo nuove migrazioni).
set -euo pipefail

DEV101_USER="administrator"
REMOTE_HOST="${REMOTE_DB_HOST:-}"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o BatchMode=yes"
PGUSER="admin"
PGPASSWORD="N3tm3d42dc2"
PGDB="nexus"
MIGRATIONS_DIR="$(cd "$(dirname "$0")/../db/migrations" && pwd)"

ERRORS=0
for f in "$MIGRATIONS_DIR"/00[0-9][0-9]_*.sql; do
  name=$(basename "$f")
  result=$(cat "$f" | ssh $SSH_OPTS "${DEV101_USER}@${DEV101_HOST}" \
    "PGPASSWORD='$PGPASSWORD' psql -h localhost -p 5432 -U '$PGUSER' -d '$PGDB' -q" 2>&1)
  if echo "$result" | grep -qi "^ERROR"; then
    echo "ERR $name: $(echo "$result" | grep -im1 "ERROR")"
    ERRORS=$((ERRORS+1))
  else
    echo "OK  $name"
  fi
done

echo ""
echo "Completato. Errori: $ERRORS"
