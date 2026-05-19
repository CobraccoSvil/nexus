#!/usr/bin/env bash
echo "=== Listening :5432 e :5433 ==="
ss -ltnp 2>/dev/null | grep -E ":(5432|5433) "

echo ""
echo "=== Container Postgres ==="
docker ps --format '{{.Names}} {{.Ports}}' | grep -i postgres

echo ""
echo "=== TCP reach :5432 e :5433 (timeout 2s) ==="
timeout 2 bash -c '</dev/tcp/localhost/5432' 2>/dev/null && echo ":5432 reachable" || echo ":5432 NOT reachable"
timeout 2 bash -c '</dev/tcp/localhost/5433' 2>/dev/null && echo ":5433 reachable" || echo ":5433 NOT reachable"
