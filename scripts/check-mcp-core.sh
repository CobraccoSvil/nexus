#!/usr/bin/env bash
pgrep -af "target/release/mcp-core" | head -3
echo "---"
curl -sS -m 5 -o /dev/null -w "mcp-core /health code=%{http_code} t=%{time_total}s\n" http://localhost:4000/health
curl -sS -m 5 -o /dev/null -w "mcp-core /        code=%{http_code} t=%{time_total}s\n" http://localhost:4000/
ss -ltnp 2>/dev/null | grep :4000
