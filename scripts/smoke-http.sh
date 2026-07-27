#!/usr/bin/env bash
set -euo pipefail
chk() {
  local url=$1
  local name=$2
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' "$url" || echo "ERR")
  echo "$name $url -> HTTP $code"
}
chk "http://127.0.0.1:3000/" "web-ide-home"
chk "http://127.0.0.1:4000/health" "mcp-core"
chk "http://127.0.0.1:4010/health" "admin-service"
chk "http://127.0.0.1:4060/health" "nexus-gateway"
