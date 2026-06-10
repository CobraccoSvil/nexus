#!/usr/bin/env bash
cd /home/administrator/ideai
for f in packages/*/package.json apps/*/package.json; do
  v=$(jq -r '.scripts.test // empty' "$f" 2>/dev/null)
  if [ "$v" = "vitest" ]; then
    echo "$f"
  fi
done
