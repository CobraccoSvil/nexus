#!/usr/bin/env bash
echo "=== Health endpoint ==="
for p in 3000 4000 4010 4020 4030 4040 4050 4055 4060 8001; do
  code=$(curl -sS -o /dev/null -m 5 -w "%{http_code}" "http://localhost:$p/health" 2>/dev/null || echo 000)
  printf "  :%-5s /health  %s\n" "$p" "$code"
done
echo ""
echo "=== Root web-ide ==="
curl -sS -o /dev/null -m 5 -w "  :3000 / -> %{http_code}\n" http://localhost:3000/ 2>/dev/null || echo "  :3000 / -> timeout"
echo ""
echo "=== Processi ==="
pgrep -af "target/release/mcp-core" | head -1
pgrep -af "next-server" | head -1
pgrep -af "nexus-gateway" | head -1
