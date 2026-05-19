#!/usr/bin/env python3
"""Classificazione precisa hardcoding modelli AI: esclude contesto cfg(test)."""
from __future__ import annotations
import re
import sys
from pathlib import Path

MODELS_RE = re.compile(
    r'"(claude-(haiku|sonnet|opus)-[0-9]'
    r'|mistral-(small|medium|large)-'
    r'|gemini-[0-9]+\.[0-9]+'
    r'|gpt-(4|3\.5|4o)'
    r'|deepseek-(chat|coder))[a-z0-9.\-]*"'
)

CFG_TEST_RE = re.compile(r'#\[cfg\(test\)\]')
MOD_TEST_RE = re.compile(r'^\s*mod\s+\w*test')


def in_cfg_test(lines: list[str], idx: int) -> bool:
    """True se la riga idx (0-based) e' dentro un blocco cfg(test) o mod tests."""
    depth = 0
    in_test = False
    test_brace = 0
    for i, line in enumerate(lines[:idx + 1]):
        if CFG_TEST_RE.search(line) and not line.lstrip().startswith('//'):
            # cerca l'apertura graffa del blocco seguente
            in_test = True
            test_brace = 0
            continue
        if in_test:
            test_brace += line.count('{') - line.count('}')
            if test_brace > 0 or '{' in line:
                pass
            else:
                continue
            if test_brace <= 0 and i > idx - 1:
                # chiuso prima della riga
                in_test = False
    return in_test


def is_comment(line: str) -> bool:
    s = line.lstrip()
    return s.startswith('//') or s.startswith('*') or s.startswith('#')


def main() -> int:
    root = Path('/home/administrator/ideai')
    targets: list[tuple[str, list[str]]] = [
        ('Rust', ['crates']),
        ('TS', ['apps', 'packages']),
        ('Python', ['brain']),
    ]
    exts_for_lang = {'Rust': '.rs', 'TS': ('.ts', '.tsx'), 'Python': '.py'}

    out_lines: dict[str, list[str]] = {'A': [], 'B': [], 'C': []}

    for lang, paths in targets:
        ext = exts_for_lang[lang]
        for base in paths:
            base_path = root / base
            if not base_path.exists():
                continue
            for f in base_path.rglob('*'):
                if not f.is_file():
                    continue
                rel = f.relative_to(root).as_posix()
                if isinstance(ext, tuple):
                    if not any(rel.endswith(e) for e in ext):
                        continue
                else:
                    if not rel.endswith(ext):
                        continue
                # skip test dirs
                if '/tests/' in rel or '/__pycache__/' in rel:
                    continue
                if '.test.' in rel or '.spec.' in rel or rel.endswith('_test.py'):
                    continue
                try:
                    text = f.read_text(encoding='utf-8', errors='replace')
                except Exception:
                    continue
                if not MODELS_RE.search(text):
                    continue
                lines = text.splitlines()
                for i, line in enumerate(lines):
                    if not MODELS_RE.search(line):
                        continue
                    if is_comment(line):
                        out_lines['C'].append(f'{rel}:{i + 1}: [comment] {line.strip()[:120]}')
                        continue
                    if lang == 'Rust' and in_cfg_test(lines, i):
                        out_lines['C'].append(f'{rel}:{i + 1}: [cfg(test)] {line.strip()[:120]}')
                        continue
                    # Regex / parser
                    if 'Regex::new' in line or 'regex!' in line or 'r#"' in line[:30] or 'r"' in line[:30]:
                        out_lines['C'].append(f'{rel}:{i + 1}: [regex] {line.strip()[:120]}')
                        continue
                    # assert!
                    if 'assert!' in line or 'assert_eq!' in line or 'assert(' in line:
                        out_lines['C'].append(f'{rel}:{i + 1}: [assert in non-test fn?] {line.strip()[:120]}')
                        continue
                    # heuristic: dropdown options / price tables (UI side) = B
                    if any(k in line for k in [': {', 'input:', 'output:', 'cacheRead:']):
                        out_lines['B'].append(f'{rel}:{i + 1}: [price-table] {line.strip()[:120]}')
                        continue
                    if any(k in line for k in ['[', 'Array<', 'Set(', 'frozenset', 'list[']):
                        out_lines['B'].append(f'{rel}:{i + 1}: [set/list] {line.strip()[:120]}')
                        continue
                    out_lines['A'].append(f'{rel}:{i + 1}: {lang}  {line.strip()[:120]}')

    for cat in ['A', 'B', 'C']:
        print(f'\n=== Cat {cat}: {len(out_lines[cat])} ===')
        for line in out_lines[cat][:50]:
            print(f'  {line}')
        if len(out_lines[cat]) > 50:
            print(f'  ... +{len(out_lines[cat]) - 50} altri')

    return 0


if __name__ == '__main__':
    sys.exit(main())
