#!/usr/bin/env bash
echo "=== ToolRunner log (mcp-core) ==="
grep -E 'ToolRunner|tool_runner' /tmp/nexus-mcp-core.log | tail -25

echo ""
echo "=== Processo tool-runner-server ==="
pgrep -af 'tool-runner\|tool_runner\|nexus-tool' | head -5
echo ""
echo "=== Listen porte runner (4070 default? Cerco) ==="
ss -ltnp 2>/dev/null | grep -E ":(4070|4071|4072|5050|5051)" || echo "(nessuna porta sospetta)"
echo ""
echo "=== TOOL_RUNNER_ADDR in env ==="
grep -E 'TOOL_RUNNER_ADDR|AGENT_ROUTER_ADDR' /home/administrator/ideai/.env 2>/dev/null
