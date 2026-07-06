#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

OUT=/tmp/backlog-closure
mkdir -p "$OUT"

echo "=== BASELINE START $(date -Iseconds) ===" | tee "$OUT/summary.log"

echo "--- unwrap/expect per crate ---" | tee -a "$OUT/summary.log"
: > "$OUT/unwrap-baseline.txt"
for d in crates/*/; do
  c=$(basename "$d")
  uw=$(grep -rn "unwrap(" "$d"src/ 2>/dev/null | grep -v -E "#\[cfg\(test\)\]|// safety:|// ok:|// allow:" | grep -v -E "/tests/" | wc -l)
  ex=$(grep -rn "expect(" "$d"src/ 2>/dev/null | grep -v -E "#\[cfg\(test\)\]|// safety:|// ok:|// allow:" | grep -v -E "/tests/" | wc -l)
  printf "%-40s unwrap=%4d expect=%4d\n" "$c" "$uw" "$ex" >> "$OUT/unwrap-baseline.txt"
done
cat "$OUT/unwrap-baseline.txt" | tee -a "$OUT/summary.log"

echo "--- hardcoding modelli Rust (fuori test) ---" | tee -a "$OUT/summary.log"
grep -rnE '"(claude-(haiku|sonnet|opus)-[0-9]|mistral-(small|medium|large)-|gemini-[0-9]+\.[0-9]+|gpt-(4|3\.5)|deepseek-(chat|coder))' crates/ --include="*.rs" 2>/dev/null \
  | grep -v -E "/tests/|_test\.rs|#\[cfg\(test\)\]" \
  > "$OUT/hardcoding-rs.txt" || true
wc -l "$OUT/hardcoding-rs.txt" | tee -a "$OUT/summary.log"

echo "--- hardcoding modelli TS (fuori test) ---" | tee -a "$OUT/summary.log"
grep -rnE '"(claude-(haiku|sonnet|opus)-[0-9]|mistral-(small|medium|large)-|gemini-[0-9]+\.[0-9]+|gpt-(4|3\.5)|deepseek-(chat|coder))' apps/ packages/ --include="*.ts" --include="*.tsx" 2>/dev/null \
  | grep -v -E "/tests?/|\.test\.|\.spec\." \
  > "$OUT/hardcoding-ts.txt" || true
wc -l "$OUT/hardcoding-ts.txt" | tee -a "$OUT/summary.log"

echo "--- migrazioni esistenti (ultime 10) ---" | tee -a "$OUT/summary.log"
ls db/migrations/ | tail -10 | tee -a "$OUT/summary.log"

echo "=== BASELINE END $(date -Iseconds) ===" | tee -a "$OUT/summary.log"
