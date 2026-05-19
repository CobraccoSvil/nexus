#!/usr/bin/env python3
"""Conta unwrap/expect per file. Detector cfg(test) robusto.

Per ogni file Rust:
 - trova la posizione del primo `#[cfg(test)]` (o `mod tests {`)
 - separa le occorrenze in PROD (prima) vs TEST (dopo)

Argomento: crate o "all" per workspace.
"""
import re
import sys
from pathlib import Path

CFG_TEST = re.compile(r'#\[cfg\(test\)\]|^\s*mod\s+\w*test')


def analyze(file_path: Path) -> tuple[int, int, int, int]:
    """Ritorna (prod_unwrap, prod_expect, test_unwrap, test_expect)."""
    try:
        text = file_path.read_text(encoding='utf-8', errors='replace')
    except Exception:
        return (0, 0, 0, 0)
    lines = text.splitlines()
    # Trova la prima riga di mod tests o cfg(test)
    test_start = len(lines)  # default: tutto in prod
    for i, line in enumerate(lines):
        if CFG_TEST.search(line) and not line.lstrip().startswith('//'):
            test_start = i
            break
    prod_uw = prod_ex = test_uw = test_ex = 0
    for i, line in enumerate(lines):
        s = line.lstrip()
        if s.startswith('//'):
            continue
        u = line.count('.unwrap(')
        e = line.count('.expect(')
        if not (u or e):
            continue
        if i >= test_start:
            test_uw += u
            test_ex += e
        else:
            prod_uw += u
            prod_ex += e
    return (prod_uw, prod_ex, test_uw, test_ex)


def main() -> int:
    root = Path('/home/administrator/ideai')
    target = sys.argv[1] if len(sys.argv) > 1 else 'mcp-core'

    if target == 'all':
        crates_dir = root / 'crates'
        crates = sorted(d.name for d in crates_dir.iterdir() if d.is_dir())
    else:
        crates = [target]

    grand_prod_uw = grand_prod_ex = grand_test_uw = grand_test_ex = 0
    print(f"{'file':<60} {'prod_uw':>8} {'prod_ex':>8} {'test_uw':>8} {'test_ex':>8}")
    print('-' * 100)
    for crate in crates:
        src = root / 'crates' / crate / 'src'
        if not src.exists():
            continue
        for f in sorted(src.rglob('*.rs')):
            p_uw, p_ex, t_uw, t_ex = analyze(f)
            grand_prod_uw += p_uw
            grand_prod_ex += p_ex
            grand_test_uw += t_uw
            grand_test_ex += t_ex
            if p_uw or p_ex:
                rel = f.relative_to(root).as_posix()
                print(f'{rel[:60]:<60} {p_uw:>8} {p_ex:>8} {t_uw:>8} {t_ex:>8}')
    print('-' * 100)
    print(f'{"TOTAL":<60} {grand_prod_uw:>8} {grand_prod_ex:>8} {grand_test_uw:>8} {grand_test_ex:>8}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
