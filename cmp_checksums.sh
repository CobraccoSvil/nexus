#!/usr/bin/env bash
# Confronta sha384 dei file migrazione con i checksum registrati in _sqlx_migrations.
set -euo pipefail
cd /home/administrator/ideai

sha384sum db/migrations/*.sql > /tmp/sums.txt

awk '{
  n = split($2, a, "/")
  fn = a[n]
  sub(/_.*/, "", fn)
  rows[NR] = sprintf("(%d,'\''%s'\'')", fn + 0, $1)
}
END {
  printf "WITH fsum(version,filesum) AS (VALUES "
  for (i = 1; i <= NR; i++) printf "%s%s", (i > 1 ? "," : ""), rows[i]
  printf ") SELECT m.version AS ver, encode(m.checksum,'\''hex'\'') AS db_sum, f.filesum AS file_sum "
  printf "FROM _sqlx_migrations m JOIN fsum f USING(version) "
  printf "WHERE encode(m.checksum,'\''hex'\'') <> f.filesum ORDER BY m.version;\n"
}' /tmp/sums.txt > /tmp/cmp.sql

echo "=== migrazioni con file != DB ==="
docker exec -i ideai-postgres-nexus-1 psql -U nexus -d nexus -P pager=off < /tmp/cmp.sql

echo "=== migrazioni nel DB ma SENZA file ==="
awk '{ n=split($2,a,"/"); fn=a[n]; sub(/_.*/,"",fn); print fn+0 }' /tmp/sums.txt | sort -n > /tmp/file_versions.txt
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -tAc "SELECT version FROM _sqlx_migrations ORDER BY version" > /tmp/db_versions.txt
echo "presenti nel DB ma non come file:"
comm -23 /tmp/db_versions.txt /tmp/file_versions.txt
