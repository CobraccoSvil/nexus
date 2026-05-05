"""Smoke runner per la qualita' dei prompt agente (Fase 4 - skeleton).

Esegue verifica statica e dinamica dei prompt nel DB:

1. Connette al DB Nexus (DATABASE_URL) e carica i prompt agente.
2. Per ogni case YAML in `evals/prompts/cases/`:
   - Risolve il prompt del profilo richiesto via prompt_renderer.
   - Verifica la rubrica: no placeholder residui, presenza dei tag XML,
     assenza di stringhe vietate, contenuti obbligatori nel protocollo.
3. Persiste un report breve su stdout. Exit code != 0 se almeno una rubrica fallisce.

Uso:
  pnpm eval:prompts:smoke
  python3 evals/prompts/smoke_runner.py [--case <name>]

Questo runner e' lo skeleton iniziale (Fase 1.7 / 4.1 del piano). La versione
completa (Settimana 4) lancera' i case contro il brain reale e raccogliera'
metriche dinamiche (tool_call_accuracy, iterations, latency).
"""
from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path
from typing import Any

# Permetti l'import di brain.* anche quando il runner e' lanciato dalla root.
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

try:
    import yaml  # type: ignore[import-untyped]
except ImportError:
    print("ERRORE: PyYAML non installato. pip install pyyaml", file=sys.stderr)
    sys.exit(2)

try:
    import psycopg2  # type: ignore[import-untyped]
except ImportError:
    print("ERRORE: psycopg2 non installato.", file=sys.stderr)
    sys.exit(2)

from brain.agents import prompt_renderer, profile_loader  # noqa: E402

CASES_DIR = Path(__file__).parent / "cases"
PLACEHOLDER_RE = re.compile(r"\{\{[^}]+\}\}")


def load_prompts_from_db(database_url: str) -> dict[str, str]:
    """Carica tutti i prompt agente dal DB."""
    conn = psycopg2.connect(database_url)
    try:
        with conn.cursor() as cur:
            cur.execute(
                "SELECT key, content FROM nexus_prompt_templates "
                "WHERE key LIKE 'agent.%%'"
            )
            rows = cur.fetchall()
    finally:
        conn.close()
    return {k: v for k, v in rows if v}


def load_cases() -> list[dict[str, Any]]:
    """Carica tutti i case YAML nella directory cases/."""
    out: list[dict[str, Any]] = []
    for path in sorted(CASES_DIR.glob("*.yaml")):
        with path.open(encoding="utf-8") as fh:
            data = yaml.safe_load(fh)
        if not isinstance(data, dict):
            print(f"WARN: case {path.name} non e' un dict, skip", file=sys.stderr)
            continue
        data["_path"] = str(path)
        out.append(data)
    return out


def resolve_prompt_for_case(case: dict[str, Any], prompts: dict[str, str]) -> str | None:
    """Risolve il prompt per un case: trova il profilo, recupera la chiave,
    applica il renderer con lo state del case."""
    agent_name = case.get("agent_type")
    if not agent_name:
        return None
    profile = profile_loader.get_profile(agent_name)
    if profile is None:
        return None
    raw = prompts.get(profile.prompt_key)
    if not raw:
        return None
    state = case.get("state") or {}
    intent = case.get("intent") or "chat"
    return prompt_renderer.render(raw, state, intent=intent)


def evaluate_rubric(case: dict[str, Any], rendered: str) -> list[str]:
    """Valuta la rubrica del case sul prompt renderizzato.

    Ritorna lista di violazioni (vuota = case ok).
    """
    rubric = case.get("rubric") or {}
    violations: list[str] = []

    if rubric.get("must_render_without_residual_placeholder", False):
        if PLACEHOLDER_RE.search(rendered):
            violations.append(
                f"placeholder residuo: {PLACEHOLDER_RE.findall(rendered)[:3]}"
            )

    for tag in rubric.get("must_contain_xml_tags", []) or []:
        if tag not in rendered:
            violations.append(f"tag XML obbligatorio mancante: {tag}")

    for forbidden in rubric.get("forbidden_strings", []) or []:
        if forbidden in rendered:
            violations.append(f"stringa vietata presente: {forbidden}")

    must_in_protocol = rubric.get("must_contain_in_protocol", []) or []
    if must_in_protocol:
        # Estrae il blocco <protocollo>...</protocollo>
        match = re.search(r"<protocollo>(.*?)</protocollo>", rendered, re.DOTALL)
        protocol = match.group(1) if match else ""
        for needle in must_in_protocol:
            if needle not in protocol:
                violations.append(f"contenuto mancante nel <protocollo>: '{needle}'")

    return violations


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke runner prompt eval")
    parser.add_argument("--case", help="esegui solo il case con questo nome")
    args = parser.parse_args()

    database_url = os.environ.get("DATABASE_URL")
    if not database_url:
        print("ERRORE: DATABASE_URL non impostato", file=sys.stderr)
        return 2

    prompts = load_prompts_from_db(database_url)
    print(f"[smoke] caricati {len(prompts)} prompt agente da DB")

    cases = load_cases()
    if args.case:
        cases = [c for c in cases if c.get("name") == args.case]
        if not cases:
            print(f"ERRORE: case '{args.case}' non trovato", file=sys.stderr)
            return 2

    print(f"[smoke] esecuzione {len(cases)} case")
    failures: list[tuple[str, list[str]]] = []

    for case in cases:
        name = case.get("name", "?")
        rendered = resolve_prompt_for_case(case, prompts)
        if rendered is None:
            print(f"  [FAIL] {name}: profilo o prompt mancante per agent_type={case.get('agent_type')}")
            failures.append((name, ["profilo o prompt non trovato"]))
            continue
        violations = evaluate_rubric(case, rendered)
        if violations:
            print(f"  [FAIL] {name}:")
            for v in violations:
                print(f"     - {v}")
            failures.append((name, violations))
        else:
            print(f"  [OK]   {name}")

    print(f"\n[smoke] {len(cases) - len(failures)}/{len(cases)} case passati")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
