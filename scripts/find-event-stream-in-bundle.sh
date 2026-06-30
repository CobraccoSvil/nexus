#!/usr/bin/env bash
cd /home/administrator/ideai/apps/web-ide/.next/static/chunks
echo "=== File con event-stream ==="
grep -l "event-stream" app/ide/*.js *.js 2>/dev/null
echo ""
echo "=== Contesto event-stream (snippet ±50 char) ==="
for f in $(grep -l "event-stream" app/ide/*.js *.js 2>/dev/null); do
  echo "--- $f ---"
  grep -oP '.{50}event-stream.{50}' "$f" | head -3
done
