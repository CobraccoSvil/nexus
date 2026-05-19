#!/usr/bin/env bash
cd /home/administrator/ideai
python3 - <<'PY'
import re
from pathlib import Path

root = Path('apps/web-ide')
rows = []
re_style = re.compile(r'style=\{\{')
for f in root.rglob('*.tsx'):
    if '/node_modules/' in str(f) or '/.next/' in str(f):
        continue
    try:
        text = f.read_text(encoding='utf-8', errors='replace')
    except Exception:
        continue
    count = len(re_style.findall(text))
    if count > 0:
        rows.append((count, str(f)))

rows.sort(reverse=True)
print(f"{'count':>5} file")
print('-' * 60)
total = 0
for count, name in rows[:30]:
    total += count
    print(f'{count:>5} {name}')
print('-' * 60)
print(f'Top 30 sum: {total}')
print(f'All files:  {sum(c for c, _ in rows)} inline styles in {len(rows)} files')
PY
