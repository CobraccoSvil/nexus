#!/usr/bin/env bash
cd /home/administrator/ideai
OUT=/tmp/backlog-closure/clippy-dead.log
echo "=== $(date -Iseconds) clippy dead_code ===" > "$OUT"
cargo clippy --workspace --all-targets -- -W dead_code -W unused_imports 2>&1 >> "$OUT"
echo "=== exit=$? ===" >> "$OUT"
grep -E "^warning|^error|src/" "$OUT" | grep -iE "dead_code|unused" | head -40
echo "--- COUNT ---"
grep -ciE "dead_code|unused_(import|variable)" "$OUT"
