#!/usr/bin/env bash
LOG=/tmp/backlog-closure/verify.log

echo "=== Esiti finali (== exit) ==="
grep "=== exit" "$LOG"

echo ""
echo "=== Tasks turbo / counts ==="
grep -E "Tasks:|Cached:" "$LOG" | tail -5

echo ""
echo "=== cargo lines ==="
grep -i 'cargo' "$LOG" | head -10

echo ""
echo "=== clippy lines ==="
grep -i 'clippy' "$LOG" | head -10

echo ""
echo "=== errori cargo (E0... + warning -D) ==="
grep -E 'error\[E[0-9]+\]|warning: unused|-D warnings' "$LOG" | head -20

echo ""
echo "=== package che ha generato error vero ==="
grep -E ':typecheck:.+error|:lint:.+error\b' "$LOG" | head -20
