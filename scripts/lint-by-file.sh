#!/usr/bin/env bash
cd /home/administrator/ideai
LOG=/tmp/backlog-closure/verify.log

python3 - <<'PY'
import re
from collections import defaultdict
path_re = re.compile(r'/home/administrator/ideai/(apps/web-ide/[^ \n]+\.tsx?)$')
warn_re = re.compile(r'^\s*@ai-orchestrator/web-ide:lint:\s+(\d+:\d+)\s+(warning|error)\s+(.+?)\s+(@?[\w-]+/[\w-]+)\s*$')

current = None
warnings_by_file = defaultdict(list)
with open('/tmp/backlog-closure/verify.log') as f:
    for line in f:
        line = line.rstrip('\n')
        # detect file header: tail after lint:
        m = re.search(r'@ai-orchestrator/web-ide:lint:\s*(/home/administrator/ideai/apps/web-ide/\S+\.tsx?)$', line)
        if m:
            current = m.group(1).replace('/home/administrator/ideai/', '')
            continue
        if current and ': warning' in line.lower() or (current and 'warning' in line):
            mw = re.search(r'warning\s+(.+?)\s+(@?[\w-]+/[\w-]+)\s*$', line)
            if mw:
                warnings_by_file[current].append((mw.group(2), mw.group(1).strip()))

ranked = sorted(warnings_by_file.items(), key=lambda kv: -len(kv[1]))
total = 0
for f, ws in ranked:
    total += len(ws)
    print(f'{f}: {len(ws)} warnings')
print(f'\nTOTALE: {total}')
PY
