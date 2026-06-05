"""Estrazione di un blocco JSON da output LLM (punto unico, regola L / ADR 0026).

Prima questa logica era duplicata (router/agentic_classifier.py::_extract_json,
grpc_server/routes/agent.py::_extract_json_block, e altri nodi). Gestisce: fence
markdown ``` / ```json, testo prima/dopo l'oggetto, e oggetti annidati a N
livelli tramite brace-matching che rispetta stringhe ed escape.
"""
from __future__ import annotations

import json
import re
from typing import Optional


def extract_json_block(text: str) -> Optional[dict]:
    """Estrae il primo oggetto JSON valido da ``text``.

    Strategia, dalla piu' alla meno robusta:
      1. strip dei code fence markdown, poi parse diretto;
      2. brace-matching counter: dal primo ``{`` alla ``}`` bilanciata a
         qualunque profondita' di annidamento (rispetta stringhe ed escape);
      3. fallback regex single-level (legacy).

    Ritorna ``None`` se non trova un oggetto JSON valido. I valori non-oggetto
    (es. una lista JSON top-level) ritornano ``None``: i call site si aspettano
    un dict.
    """
    if not text:
        return None
    content = re.sub(r"^```(?:json)?\s*", "", text.strip())
    content = re.sub(r"\s*```\s*$", "", content)

    # 1. Parse diretto del contenuto pulito.
    try:
        parsed = json.loads(content)
        return parsed if isinstance(parsed, dict) else None
    except json.JSONDecodeError:
        pass

    # 2. Brace-matching counter (gestisce N livelli annidati).
    start = content.find("{")
    if start >= 0:
        depth = 0
        in_string = False
        escape = False
        for i in range(start, len(content)):
            ch = content[i]
            if escape:
                escape = False
                continue
            if ch == "\\":
                escape = True
                continue
            if ch == '"':
                in_string = not in_string
                continue
            if in_string:
                continue
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    candidate = content[start : i + 1]
                    try:
                        parsed = json.loads(candidate)
                        return parsed if isinstance(parsed, dict) else None
                    except json.JSONDecodeError:
                        break  # cade nel fallback regex

    # 3. Fallback legacy regex (annidamento single-level).
    match = re.search(r"\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}", content, re.DOTALL)
    if match:
        try:
            parsed = json.loads(match.group(0))
            return parsed if isinstance(parsed, dict) else None
        except json.JSONDecodeError:
            return None
    return None
