#!/usr/bin/env bash
set +e
cd /home/administrator/ideai
OUT=/tmp/backlog-closure
mkdir -p "$OUT"
echo "=== $(date -Iseconds) running pnpm verify (CI=1) ===" > "$OUT/verify.log"
CI=1 pnpm verify >> "$OUT/verify.log" 2>&1
RC=$?
echo "=== exit=$RC ===" >> "$OUT/verify.log"
tail -50 "$OUT/verify.log"
