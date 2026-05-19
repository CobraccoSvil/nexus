#!/usr/bin/env bash
cd /home/administrator/ideai
exec > /tmp/post-pop-check.log 2>&1

echo "=== Cargo check workspace ==="
cargo check --workspace --quiet 2>&1 | tail -15
echo "RC=$?"

echo ""
echo "=== Python syntax ==="
python3 -m py_compile \
  brain/grpc_server/main.py \
  brain/agents/graph.py \
  brain/agents/nodes.py \
  brain/agents/planner_node.py \
  brain/agents/state.py \
  brain/agents/clarify_or_expand_node.py \
  brain/agents/meta_steps.py \
  brain/redaction/client.py \
  brain/providers/vllm_provider.py \
  brain/providers/anthropic_provider.py \
  brain/router/service.py \
  2>&1
echo "RC=$?"

echo ""
echo "=== ESLint web-ide (warnings count) ==="
pnpm --filter @ai-orchestrator/web-ide exec eslint . 2>&1 | tail -8
