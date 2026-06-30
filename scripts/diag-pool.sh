#!/usr/bin/env bash
echo "=== mcp-core log: pool / timed out / test-connection (ultimi 30) ==="
grep -iE 'pool|timed out|test.connection' /tmp/nexus-mcp-core.log | tail -30

echo ""
echo "=== mcp-core ERROR/WARN ultimi 5 min ==="
tail -500 /tmp/nexus-mcp-core.log | grep -iE 'ERROR|WARN' | tail -10

echo ""
echo "=== DATABASE_URL / max connections ==="
grep -E 'DATABASE_URL|MAX_CONNECT|MCP_DB' /home/administrator/ideai/.env 2>/dev/null | head -5

echo ""
echo "=== Postgres connessioni attive ==="
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -A -c "SELECT count(*) FROM pg_stat_activity WHERE state IS NOT NULL"
docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -A -c "SELECT max_conn, used FROM (SELECT count(*) used FROM pg_stat_activity) t, (SELECT setting::int max_conn FROM pg_settings WHERE name='max_connections') s"
