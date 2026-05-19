#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai

OUT=/tmp/backlog-closure
echo "=== TOP FILE Rust hardcoding ==="
cut -d: -f1 "$OUT/hardcoding-rs.txt" | sort | uniq -c | sort -rn | head -20

echo ""
echo "=== TOP FILE TS hardcoding ==="
cut -d: -f1 "$OUT/hardcoding-ts.txt" | sort | uniq -c | sort -rn | head -20

echo ""
echo "=== Categorizzazione euristica Rust ==="
total_rs=$(wc -l < "$OUT/hardcoding-rs.txt")
seed_sql=$(grep -cE "INSERT INTO|UPDATE.*SET|VALUES.*\(" "$OUT/hardcoding-rs.txt" || echo 0)
regex_pat=$(grep -cE "Regex::new|regex!|r#?\"" "$OUT/hardcoding-rs.txt" || echo 0)
alias=$(grep -cE "model_alias|aliases|ALIAS_" "$OUT/hardcoding-rs.txt" || echo 0)
comment=$(grep -cE "^\s*//|\.rs:[0-9]+:\s*//" "$OUT/hardcoding-rs.txt" || echo 0)
echo "totale=$total_rs  seed_sql_like=$seed_sql  regex_like=$regex_pat  alias_like=$alias  commento=$comment"

echo ""
echo "=== Sample 30 righe non-seed Rust ==="
grep -vE "INSERT INTO|UPDATE.*SET|VALUES.*\(|Regex::new|regex!|model_alias|aliases|ALIAS_" "$OUT/hardcoding-rs.txt" | head -30

echo ""
echo "=== Categorizzazione euristica TS ==="
total_ts=$(wc -l < "$OUT/hardcoding-ts.txt")
seed=$(grep -cE "INSERT INTO|UPDATE.*SET" "$OUT/hardcoding-ts.txt" || echo 0)
alias=$(grep -cE "alias|ALIAS_|mapping" "$OUT/hardcoding-ts.txt" || echo 0)
comment=$(grep -cE "\.ts:[0-9]+:\s*//|\.ts:[0-9]+:\s*\*" "$OUT/hardcoding-ts.txt" || echo 0)
echo "totale=$total_ts  seed_like=$seed  alias_like=$alias  commento=$comment"

echo ""
echo "=== Sample 30 righe non-seed TS ==="
grep -vE "INSERT INTO|UPDATE.*SET|alias|ALIAS_|mapping" "$OUT/hardcoding-ts.txt" | head -30
