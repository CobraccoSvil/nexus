"""Rubrica di conformita' dei prompt alle direttive di prompt engineering.

Punto unico (regola L) per la valutazione e l'eventuale revisione di un template
prompt rispetto a un insieme di direttive (best practice Anthropic + regole
interne sezione D). Modellato su `brain.agents.reflection_rubric`.

Uso:
    from brain.agents.prompt_conformance_rubric import (
        build_revise_prompt, parse_revise_response, aggregate_score,
    )

Due modalita':
    - "evaluate": solo punteggio + dimensioni + issues (output breve, economico)
    - "evaluate_and_revise": aggiunge `revised_template` + `rationale`

Struttura risposta attesa (evaluate):
    {
      "overall_score": 0.0-1.0,
      "dimensions": {
        "alignment": 0.0-1.0,
        "structure": 0.0-1.0,
        "clarity": 0.0-1.0,
        "safety_preservation": 0.0-1.0
      },
      "issues": [{"practice_key": "...", "severity": "must|should|nice", "detail": "..."}]
    }
In modalita' "evaluate_and_revise" si aggiungono:
      "revised_template": "<prompt riallineato completo>",
      "rationale": "<spiegazione sintetica delle modifiche>"
"""
from __future__ import annotations

import logging
from typing import Any

from brain.utils.json_extract import extract_json_block

logger = logging.getLogger(__name__)

# Dimensioni della rubrica con peso relativo. I pesi sommano a 1.0.
DIMENSIONI: dict[str, tuple[str, float]] = {
    "alignment": (
        "Il prompt rispetta le direttive attive (severita' 'must' su tutte, 'should' sulla maggior parte)?",
        0.40,
    ),
    "structure": (
        "Il prompt e' strutturato e completo (tag/sezioni previsti, formato di output esplicito)?",
        0.30,
    ),
    "clarity": (
        "Le istruzioni sono chiare, univoche e in italiano, senza ambiguita' o ridondanze?",
        0.20,
    ),
    "safety_preservation": (
        "Il prompt preserva i vincoli di sicurezza e non introduce istruzioni rischiose?",
        0.10,
    ),
}

_SYSTEM_RUBRIC = """\
Sei un esperto di prompt engineering per agenti AI specializzati in sviluppo software.
Valuti la conformita' di un template di prompt a un insieme di direttive (best practice Anthropic e regole interne del progetto).
Rispondi ESCLUSIVAMENTE con JSON valido, senza testo aggiuntivo, markdown o delimitatori.
"""

_TEMPLATE_EVALUATE = """\
<prompt_da_valutare>
{template}
</prompt_da_valutare>

<direttive_attive>
{guidelines}
</direttive_attive>
{signals}
<rubrica>
Valuta ciascuna dimensione con un punteggio da 0.0 (pessimo) a 1.0 (eccellente):

{rubrica_dettaglio}
</rubrica>

Istruzioni:
1. Assegna un punteggio per ciascuna dimensione, confrontando il prompt con OGNI direttiva attiva (usa il relativo criterio).
2. Calcola overall_score come media ponderata (pesi: alignment=0.40, structure=0.30, clarity=0.20, safety_preservation=0.10).
3. Elenca le violazioni specifiche in `issues` (max 8), una per direttiva non rispettata, indicando practice_key, severity e un dettaglio concreto.

Rispondi SOLO con questo JSON (nessun altro testo):
{{
  "overall_score": <float 0.0-1.0>,
  "dimensions": {{
    "alignment": <float>,
    "structure": <float>,
    "clarity": <float>,
    "safety_preservation": <float>
  }},
  "issues": [{{"practice_key": "<stringa>", "severity": "must|should|nice", "detail": "<stringa>"}}]
}}
"""

_TEMPLATE_REVISE = """\
<prompt_da_valutare>
{template}
</prompt_da_valutare>

<direttive_attive>
{guidelines}
</direttive_attive>
{signals}
<rubrica>
Valuta ciascuna dimensione con un punteggio da 0.0 (pessimo) a 1.0 (eccellente):

{rubrica_dettaglio}
</rubrica>

Istruzioni:
1. Assegna un punteggio per ciascuna dimensione confrontando il prompt con OGNI direttiva attiva.
2. Calcola overall_score come media ponderata (pesi: alignment=0.40, structure=0.30, clarity=0.20, safety_preservation=0.10).
3. Elenca le violazioni in `issues` (max 8).
4. Produci `revised_template`: il prompt RISCRITTO in modo da rispettare tutte le direttive 'must' e il maggior numero possibile di 'should', in italiano, senza emoji. PRESERVA ogni tag, sezione o vincolo di sicurezza gia' presente (non rimuovere nulla di critico). Mantieni i placeholder esistenti (es. {{{{lang_hint}}}}, {{{{repo_summary}}}}) invariati.
5. Produci `rationale`: spiegazione sintetica delle modifiche.

Rispondi SOLO con questo JSON (nessun altro testo):
{{
  "overall_score": <float 0.0-1.0>,
  "dimensions": {{
    "alignment": <float>,
    "structure": <float>,
    "clarity": <float>,
    "safety_preservation": <float>
  }},
  "issues": [{{"practice_key": "<stringa>", "severity": "must|should|nice", "detail": "<stringa>"}}],
  "revised_template": "<prompt riallineato completo>",
  "rationale": "<spiegazione sintetica>"
}}
"""


def _rubrica_dettaglio() -> str:
    """Testo descrittivo della rubrica per il prompt."""
    righe = []
    for nome, (descrizione, peso) in DIMENSIONI.items():
        righe.append(f"- {nome} (peso {peso:.0%}): {descrizione}")
    return "\n".join(righe)


def _guidelines_block(guidelines: list[dict[str, Any]]) -> str:
    """Serializza l'elenco di direttive attive in testo per il prompt."""
    if not guidelines:
        return "(nessuna direttiva attiva: valuta solo struttura, chiarezza e sicurezza generali)"
    righe = []
    for g in guidelines:
        practice = str(g.get("practice_key", "?"))
        severity = str(g.get("severity", "should"))
        desc = str(g.get("description", "")).strip()
        hint = str(g.get("check_hint", "")).strip()
        righe.append(f"- [{severity}] {practice}: {desc}\n  Criterio: {hint}")
    return "\n".join(righe)


def _signals_block(signals: dict[str, Any] | None) -> str:
    """Blocco opzionale con segnali aggiuntivi (es. weaknesses da reflection)."""
    if not signals:
        return ""
    weaknesses = signals.get("weaknesses") or []
    if not weaknesses:
        return ""
    elenco = "\n".join(f"- {str(w)}" for w in weaknesses[:5])
    return f"\n<debolezze_osservate>\n{elenco}\n</debolezze_osservate>\n"


def build_revise_prompt(
    current_template: str,
    guidelines: list[dict[str, Any]],
    mode: str = "evaluate",
    signals: dict[str, Any] | None = None,
) -> tuple[str, str]:
    """Restituisce (system_prompt, user_prompt) per la valutazione/revisione.

    Args:
        current_template: il contenuto del template da valutare.
        guidelines: direttive attive [{practice_key, description, check_hint, severity}].
        mode: "evaluate" (solo punteggio) o "evaluate_and_revise" (anche revised_template).
        signals: segnali opzionali (es. {"weaknesses": [...]}).
    """
    tmpl = _TEMPLATE_REVISE if mode == "evaluate_and_revise" else _TEMPLATE_EVALUATE
    user = tmpl.format(
        template=(current_template or "(prompt vuoto)")[:12000],
        guidelines=_guidelines_block(guidelines),
        signals=_signals_block(signals),
        rubrica_dettaglio=_rubrica_dettaglio(),
    )
    return _SYSTEM_RUBRIC, user


def parse_revise_response(raw: str, mode: str = "evaluate") -> dict[str, Any] | None:
    """Analizza la risposta grezza del modello.

    Restituisce None se il parsing fallisce (il chiamante decide come gestire).
    """
    if not raw:
        return None
    data = extract_json_block(raw)
    if data is None:
        logger.warning("prompt_conformance: risposta non parsabile: %.200s", raw)
        return None
    try:
        return _validate(data, mode)
    except (ValueError, TypeError) as e:
        logger.warning("prompt_conformance: validazione fallita (%s): %.200s", e, raw)
        return None


def _validate(data: dict[str, Any], mode: str) -> dict[str, Any]:
    """Valida e normalizza il dict di conformita'."""
    score = float(data.get("overall_score", -1))
    if not (0.0 <= score <= 1.0):
        raise ValueError(f"overall_score fuori range: {score}")

    dims_in = data.get("dimensions", {}) or {}
    dims_out: dict[str, float] = {}
    for dim in DIMENSIONI:
        v = float(dims_in.get(dim, -1))
        if not (0.0 <= v <= 1.0):
            raise ValueError(f"dimensione {dim} fuori range: {v}")
        dims_out[dim] = round(v, 3)

    issues_in = data.get("issues") or []
    issues_out = []
    for it in issues_in[:8]:
        if not isinstance(it, dict):
            continue
        issues_out.append({
            "practice_key": str(it.get("practice_key", "")),
            "severity": str(it.get("severity", "should")),
            "detail": str(it.get("detail", ""))[:500],
        })

    result: dict[str, Any] = {
        "overall_score": round(score, 3),
        "dimensions": dims_out,
        "issues": issues_out,
    }

    if mode == "evaluate_and_revise":
        revised = data.get("revised_template")
        if not revised or not str(revised).strip():
            raise ValueError("revised_template assente in modalita' evaluate_and_revise")
        result["revised_template"] = str(revised)
        result["rationale"] = str(data.get("rationale", ""))[:1000]

    return result


def aggregate_score(dimensions: dict[str, float]) -> float:
    """Punteggio aggregato ponderato dalle dimensioni."""
    total = 0.0
    for nome, (_, peso) in DIMENSIONI.items():
        total += float(dimensions.get(nome, 0.0)) * peso
    return round(total, 3)
