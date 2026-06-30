#!/usr/bin/env bash
cd /home/administrator/ideai
LOG=/tmp/backlog-closure/verify.log

echo "=== ERROR/FAIL/exit lines ==="
grep -iE 'error|fail|exit|elifecycle|✖' "$LOG" | grep -v 'WARNING command' | grep -v '@ai-orchestrator/web-ide:lint:' | head -40

echo ""
echo "=== Final lines ==="
tail -10 "$LOG"

echo ""
echo "=== Counts ==="
echo "warnings totali web-ide lint: $(grep -c 'warning' "$LOG")"
echo "errors:                       $(grep -cE '✖.*errors' "$LOG")"
echo "elifecycle exits:             $(grep -c 'ELIFECYCLE' "$LOG")"

echo ""
echo "=== Componenti che falliscono ==="
grep -E 'command finished with error' "$LOG" || echo "(nessun command finished with error)"
