#!/usr/bin/env bash
set -euo pipefail

echo "Starting AI-Orchestrator v2 installation"
docker compose -f deploy/docker-compose.yml up -d
echo "Infrastructure is up. Install language toolchains and application dependencies next."

