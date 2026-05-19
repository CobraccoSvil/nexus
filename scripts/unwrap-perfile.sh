#!/usr/bin/env bash
# Conta unwrap/expect per FILE dentro un crate, escludendo cfg(test) e mod tests.
# Uso: scripts/unwrap-perfile.sh <crate-name>
set -uo pipefail
CRATE="${1:-mcp-core}"
DIR="/home/administrator/ideai/crates/$CRATE/src"
if [[ ! -d "$DIR" ]]; then
    echo "Directory non trovata: $DIR"
    exit 1
fi

echo "=== unwrap/expect per file in $CRATE (esclusi cfg(test) e mod tests) ==="
echo "FILE: unwrap  expect"
echo "----"

python3 - "$DIR" <<'PY'
import sys, re
from pathlib import Path

root = Path(sys.argv[1])
cfg_test = re.compile(r'#\[cfg\(test\)\]')

def is_in_test_block(lines, idx):
    """Returns True if line idx is inside a #[cfg(test)] item (fn/mod).
    Greedy heuristic: backtrack to last #[cfg(test)] attribute that is
    followed by a fn/mod/impl, then count braces until current idx."""
    depth = 0
    in_test = False
    test_brace = -1
    for i, line in enumerate(lines[:idx+1]):
        if cfg_test.search(line) and not line.lstrip().startswith('//'):
            # next non-empty line should start a fn/mod/impl
            for j in range(i+1, min(i+4, len(lines))):
                nl = lines[j].lstrip()
                if not nl:
                    continue
                if nl.startswith(('pub ', 'fn ', 'mod ', 'impl ', 'async ')) or 'fn ' in nl[:30]:
                    in_test = True
                    test_brace = 0
                break
        if in_test:
            test_brace += line.count('{') - line.count('}')
            if test_brace <= 0 and '{' in line:
                # apparently closed on same line - rare
                in_test = False
                test_brace = -1
            elif test_brace == 0 and i > 0:
                in_test = False
                test_brace = -1
    return in_test

rows = []
for f in sorted(root.rglob('*.rs')):
    text = f.read_text(encoding='utf-8', errors='replace')
    lines = text.splitlines()
    uw = 0
    ex = 0
    for i, line in enumerate(lines):
        if 'unwrap(' in line or 'expect(' in line:
            if line.lstrip().startswith('//'):
                continue
            if is_in_test_block(lines, i):
                continue
            if 'unwrap(' in line:
                uw += line.count('unwrap(')
            if 'expect(' in line:
                ex += line.count('expect(')
    if uw or ex:
        rel = f.relative_to(root).as_posix()
        rows.append((uw + ex, uw, ex, rel))

rows.sort(reverse=True)
for tot, uw, ex, rel in rows:
    print(f'{rel}: unwrap={uw} expect={ex} tot={tot}')
print(f'\nTOTAL: unwrap={sum(r[1] for r in rows)} expect={sum(r[2] for r in rows)} files={len(rows)}')
PY
