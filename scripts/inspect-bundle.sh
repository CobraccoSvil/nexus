#!/usr/bin/env bash
cd /home/administrator/ideai/apps/web-ide/.next/static/chunks

echo "=== File che contengono 'openStream' o 'event-stream' ==="
grep -lE 'openStream|connectDispatcher|/event-stream' *.js 2>/dev/null | head -10

echo ""
echo "=== Sample contesto openStream ==="
for f in $(grep -lE 'openStream' *.js 2>/dev/null | head -3); do
  echo "--- $f ---"
  grep -oE '[a-zA-Z_$]{1,30}openStream[a-zA-Z_$0-9]{0,30}' "$f" 2>/dev/null | sort -u | head -10
done

echo ""
echo "=== Sample contesto event-stream URL ==="
grep -hoE '/api/projects/[^"]{0,80}event-stream[^"]{0,40}' *.js 2>/dev/null | sort -u | head -5

echo ""
echo "=== Path snapshot ==="
grep -hoE '/api/projects/[^"]{0,80}snapshot[^"]{0,40}' *.js 2>/dev/null | sort -u | head -5
