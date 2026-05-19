#!/usr/bin/env python3
"""Classifica le unwrap/expect PROD per categoria.

Cat REGEX: linea contiene Regex::new(...).unwrap() - ammessa §F.
Cat MUTEX: linea contiene .lock().unwrap() - ammessa §F.
Cat STATIC: linea contiene parse o from_str su literal - ammessa §F.
Cat OTHER: tutto il resto - da fixare o annotare.

Per ogni file PROD con almeno 1 unwrap/expect:
- elenca occorrenze classificate
- count totale per categoria
"""
import re
import sys
from pathlib import Path

CFG_TEST = re.compile(r'#\[cfg\(test\)\]|^\s*mod\s+\w*test')


def find_test_start(lines):
    for i, line in enumerate(lines):
        if CFG_TEST.search(line) and not line.lstrip().startswith('//'):
            return i
    return len(lines)


def classify(line: str) -> str:
    if 'Regex::new' in line or 'regex!(' in line:
        return 'REGEX'
    if '.lock()' in line and '.unwrap()' in line:
        return 'MUTEX'
    if ('parse::<' in line or 'from_str' in line) and ('.unwrap()' in line or '.expect(' in line):
        # static literal? Hard to tell - flag as MAYBE
        return 'PARSE'
    if 'env!' in line or 'include_str!' in line or 'include_bytes!' in line:
        return 'COMPILE_TIME'
    return 'OTHER'


def main() -> int:
    root = Path('/home/administrator/ideai')
    crates_dir = root / 'crates'
    if not crates_dir.exists():
        print('crates dir non trovata')
        return 1

    summary = {'REGEX': 0, 'MUTEX': 0, 'PARSE': 0, 'COMPILE_TIME': 0, 'OTHER': 0}
    other_by_file = {}

    for crate in sorted(d.name for d in crates_dir.iterdir() if d.is_dir()):
        src = crates_dir / crate / 'src'
        if not src.exists():
            continue
        for f in sorted(src.rglob('*.rs')):
            text = f.read_text(encoding='utf-8', errors='replace')
            lines = text.splitlines()
            test_start = find_test_start(lines)
            for i in range(test_start):
                line = lines[i]
                if line.lstrip().startswith('//'):
                    continue
                if '.unwrap(' not in line and '.expect(' not in line:
                    continue
                cat = classify(line)
                summary[cat] += line.count('.unwrap(') + line.count('.expect(')
                if cat == 'OTHER':
                    rel = f.relative_to(root).as_posix()
                    other_by_file.setdefault(rel, []).append((i + 1, line.strip()[:140]))

    print('=== SUMMARY PROD ===')
    for k, v in summary.items():
        print(f'  {k}: {v}')

    print('\n=== Cat OTHER per file ===')
    sorted_files = sorted(other_by_file.items(), key=lambda kv: -len(kv[1]))
    for rel, items in sorted_files:
        print(f'\n{rel} ({len(items)})')
        for ln, t in items[:5]:
            print(f'  :{ln}: {t}')
        if len(items) > 5:
            print(f'  ... +{len(items) - 5}')

    return 0


if __name__ == '__main__':
    sys.exit(main())
