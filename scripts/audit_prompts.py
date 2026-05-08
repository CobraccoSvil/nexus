#!/usr/bin/env python3
"""Audit dei system prompt in nexus_prompt_templates (BP14 piano riduzione token).

Misura il token count di ogni template attivo via tiktoken (cl100k_base) e
genera un report Markdown ordinato per peso. Identifica:
- Top 10 template piu' lunghi (candidati al refactor)
- Template con tag XML ridondanti o esempi prolissi
- Template che superano una soglia di "warning" (default 2000 token)

Uso:
    python3 scripts/audit_prompts.py
    python3 scripts/audit_prompts.py --output docs/audit-prompts.md

Connessione DB: prende DATABASE_URL dall'env o usa default locale.
"""
from __future__ import annotations

import argparse
import os
import re
import sys
from datetime import datetime
from pathlib import Path

DEFAULT_DSN = "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable"
WARNING_TOKENS = 2000  # template oltre questa soglia sono segnalati
LARGE_TOKENS = 1000    # template oltre questa soglia entrano nel "top heavy"


def count_tokens(text: str) -> int:
    """Conta i token cl100k_base (compatibile con GPT-4 / Claude tokenizer)."""
    try:
        import tiktoken  # type: ignore[import-untyped]
    except ImportError:
        sys.stderr.write("ERROR: tiktoken non installato. pip install tiktoken\n")
        sys.exit(1)
    enc = tiktoken.get_encoding("cl100k_base")
    return len(enc.encode(text))


def detect_redundancy(content: str) -> list[str]:
    """Heuristics per individuare ridondanze comuni."""
    issues: list[str] = []

    # 1. Esempi multipli (>3 blocchi <example>)
    n_examples = len(re.findall(r"<example", content, re.IGNORECASE))
    if n_examples > 3:
        issues.append(f"{n_examples} blocchi <example> (consigliato max 3)")

    # 2. Frasi ripetute (la stessa istruzione comparsa >2 volte)
    sentences = re.split(r"[.!?]\s+", content)
    seen: dict[str, int] = {}
    for s in sentences:
        key = s.strip().lower()
        if len(key) > 30:  # ignora frasi troppo corte
            seen[key] = seen.get(key, 0) + 1
    duplicates = [s for s, c in seen.items() if c > 1]
    if duplicates:
        issues.append(f"{len(duplicates)} frasi ripetute >1 volta")

    # 3. Bullet point molto lunghi (potrebbero essere compressi)
    long_bullets = re.findall(r"^[\s]*[-*]\s+(.{200,})$", content, re.MULTILINE)
    if long_bullets:
        issues.append(f"{len(long_bullets)} bullet >200 char (compattabili)")

    # 4. Sezione di greeting/preambolo (raramente utile per LLM)
    if re.search(r"(ciao|hello|hi),?\s+(sei|you are)", content[:200], re.IGNORECASE):
        issues.append("preambolo conversazionale rimovibile")

    return issues


def main() -> int:
    parser = argparse.ArgumentParser(description="Audit system prompt token weights.")
    parser.add_argument("--output", default="docs/audit-prompts.md",
                        help="Path del report Markdown (default: docs/audit-prompts.md)")
    parser.add_argument("--dsn", default=os.getenv("DATABASE_URL", DEFAULT_DSN),
                        help="DSN Postgres (default: $DATABASE_URL o locale)")
    parser.add_argument("--top", type=int, default=10,
                        help="Numero di template nel top heavy (default 10)")
    args = parser.parse_args()

    try:
        import psycopg2  # type: ignore[import-untyped]
    except ImportError:
        sys.stderr.write("ERROR: psycopg2 non installato. pip install psycopg2-binary\n")
        return 1

    # Normalizza DSN per psycopg2 (vuole 'postgresql://', non 'postgres://')
    dsn = args.dsn.replace("postgres://", "postgresql://", 1)
    # Rimuove ?sslmode=disable se presente (psycopg2 lo legge come kwarg)
    sslmode = "disable" if "sslmode=disable" in dsn else None
    dsn = re.sub(r"\?sslmode=[^&]+", "", dsn)

    conn_kwargs = {}
    if sslmode:
        conn_kwargs["sslmode"] = sslmode

    try:
        conn = psycopg2.connect(dsn, **conn_kwargs)
    except Exception as exc:
        sys.stderr.write(f"ERROR: connessione DB fallita: {exc}\n")
        return 1

    cur = conn.cursor()
    cur.execute("""
        SELECT key, category, title, content, schema_type, version
        FROM nexus_prompt_templates
        WHERE is_active = true
        ORDER BY key
    """)
    rows = cur.fetchall()
    cur.close()
    conn.close()

    if not rows:
        sys.stderr.write("WARNING: nessun template attivo trovato\n")
        return 1

    # Calcola metriche per ogni template
    audit_data = []
    for key, category, title, content, schema_type, version in rows:
        tokens = count_tokens(content)
        chars = len(content)
        issues = detect_redundancy(content)
        audit_data.append({
            "key": key, "category": category, "title": title,
            "tokens": tokens, "chars": chars, "schema": schema_type,
            "version": version, "issues": issues,
        })

    # Statistiche aggregate
    total_tokens = sum(d["tokens"] for d in audit_data)
    avg_tokens = total_tokens / len(audit_data) if audit_data else 0
    over_warning = [d for d in audit_data if d["tokens"] >= WARNING_TOKENS]
    over_large = sorted(
        [d for d in audit_data if d["tokens"] >= LARGE_TOKENS],
        key=lambda d: d["tokens"], reverse=True,
    )[: args.top]

    # Genera report Markdown
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    lines: list[str] = []
    lines.append(f"# Audit system prompt -- token weights\n")
    lines.append(f"Generato: {datetime.now().isoformat(timespec='seconds')}\n")
    lines.append(f"## Sintesi\n")
    lines.append(f"- Template attivi: **{len(audit_data)}**")
    lines.append(f"- Token totali: **{total_tokens:,}**")
    lines.append(f"- Token medio per template: **{avg_tokens:.0f}**")
    lines.append(f"- Template oltre soglia warning ({WARNING_TOKENS} tok): **{len(over_warning)}**")
    lines.append("")

    if over_warning:
        lines.append(f"## Sopra soglia warning ({WARNING_TOKENS} tok)\n")
        lines.append("| Key | Categoria | Tokens | Issues |")
        lines.append("|-----|-----------|-------:|--------|")
        for d in sorted(over_warning, key=lambda x: x["tokens"], reverse=True):
            issues = "; ".join(d["issues"]) if d["issues"] else "-"
            lines.append(f"| `{d['key']}` | {d['category']} | {d['tokens']:,} | {issues} |")
        lines.append("")

    lines.append(f"## Top {args.top} template piu' pesanti (>= {LARGE_TOKENS} tok)\n")
    lines.append("| Key | Categoria | Schema | Tokens | Char | Issues |")
    lines.append("|-----|-----------|--------|-------:|-----:|--------|")
    for d in over_large:
        issues = "; ".join(d["issues"]) if d["issues"] else "-"
        lines.append(
            f"| `{d['key']}` | {d['category']} | {d['schema']} | "
            f"{d['tokens']:,} | {d['chars']:,} | {issues} |"
        )
    lines.append("")

    lines.append("## Tutti i template (ordine alfabetico)\n")
    lines.append("| Key | Tokens | Char |")
    lines.append("|-----|-------:|-----:|")
    for d in audit_data:
        lines.append(f"| `{d['key']}` | {d['tokens']:,} | {d['chars']:,} |")
    lines.append("")

    lines.append("## Raccomandazioni\n")
    lines.append("Per ogni template segnalato:")
    lines.append("1. Rimuovere preamboli conversazionali ('Sei un agente...')")
    lines.append("2. Sostituire 2+ esempi con 1 solo paradigmatico")
    lines.append("3. Compattare bullet point >200 char in frasi singole")
    lines.append("4. Verificare coerenza con `<safety_progetto>` (mig 0096)")
    lines.append("5. Riferimento target: < 1000 token per template specializzato")
    lines.append("")

    out_path.write_text("\n".join(lines), encoding="utf-8")
    print(f"Report scritto in: {out_path}")
    print(f"Template totali: {len(audit_data)}, token totali: {total_tokens:,}")
    print(f"Sopra soglia warning: {len(over_warning)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
