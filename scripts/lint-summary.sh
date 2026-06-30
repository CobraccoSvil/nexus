#!/usr/bin/env bash
cd /home/administrator/ideai
LOG=/tmp/backlog-closure/verify.log

echo "=== Top 20 file per numero di warning lint ==="
grep -oE '/home/administrator/ideai/apps/web-ide/[^ ]+\.tsx?' "$LOG" 2>/dev/null \
  | sed 's|/home/administrator/ideai/||g' \
  | sort | uniq -c | sort -rn | head -20

echo ""
echo "=== Tipi di warning ==="
grep -oE '@typescript-eslint/[a-z-]+|react-hooks/[a-z-]+' "$LOG" \
  | sort | uniq -c | sort -rn
