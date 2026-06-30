#!/usr/bin/env bash
set +e
cd /home/administrator/ideai
OUT=/tmp/backlog-closure
mkdir -p "$OUT"
echo "=== $(date -Iseconds) running pnpm verify ===" > "$OUT/verify.log"
pnpm verify >> "$OUT/verify.log" 2>&1
echo "=== exit=$? ===" >> "$OUT/verify.log"
tail -100 "$OUT/verify.log"
