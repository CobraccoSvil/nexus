"""Rubrica statica per il nodo di self-reflection (Fase 2).

La rubrica definisce le dimensioni di valutazione e il meta-prompt inviato
al modello LLM per ottenere un punteggio JSON strutturato dell'output agente.

Uso:
    from brain.agents.reflection_rubric import build_reflection_prompt, parse_reflection_response

Struttura risposta attesa:
    {
      "score": 0.0-1.0,
      "dimensions": {
        "correctness": 0.0-1.0,
        "completeness": 0.0-1.0,
        "efficiency": 0.0-1.0,
        "safety": 0.0-1.0
      },
      "weaknesses": ["...", "..."],
      "suggestions": ["...", "..."]
    }
"""
from __future__ import annotations

import json
import logging
import re
from typing import Any

logger = logging.getLogger(__name__)

# Dimensioni della rubrica con peso relativo (usato per il punteggio aggregato).
# I pesi sommano a 1.0.
DIMENSIONI: dict[str, tuple[str, float]] = {
    "correctness": (
        "L'output risolve correttamente e completamente il problema richiesto?",
        0.40,
    ),
    "completeness": (
        "L'output copre tutti gli aspetti del task senza lasciare parti irrisolte o incomplete?",
        0.30,
    ),
    "efficiency": (
        "L'agente ha usato il numero minimo necessario di iterazioni e tool, senza ridondanze?",
        0.15,
    ),
    "safety": (
        "L'agente ha evitato azioni distruttive o irreversibili non esplicitamente richieste?",
        0.15,
    ),
}

# Prompt di sistema per il valutatore (strettamente read-only, max 400 token output)
_SYSTEM_RUBRIC = """\
Sei un valutatore critico e imparziale di output di agenti AI specializzati in sviluppo software.
Il tuo unico compito e' analizzare l'output dell'agente e produrre una valutazione JSON strutturata.
Non devi generare codice, correggere bug o svolgere il task originale: solo valutare.
Rispondi ESCLUSIVAMENTE con JSON valido, senza testo aggiuntivo, markdown o delimitatori.
"""

_TEMPLATE_UTENTE = """\
<task_originale>
{task}
</task_originale>

<output_agente>
{output}
</output_agente>

<rubrica>
Valuta ciascuna dimensione con un punteggio da 0.0 (pessimo) a 1.0 (eccellente):

{rubrica_dettaglio}
</rubrica>

Istruzioni:
1. Assegna un punteggio per ciascuna dimensione.
2. Calcola il punteggio finale come media ponderata (pesi: correctness=0.40, completeness=0.30, efficiency=0.15, safety=0.15).
3. Elenca al massimo 3 punti deboli specifici e concreti (non generici).
4. Suggerisci al massimo 3 miglioramenti concreti e applicabili al prompt dell'agente.

Rispondi SOLO con questo JSON (nessun altro testo):
{{
  "score": <float 0.0-1.0>,
  "dimensions": {{
    "correctness": <float>,
    "completeness": <float>,
    "efficiency": <float>,
    "safety": <float>
  }},
  "weaknesses": ["<stringa>", "..."],
  "suggestions": ["<stringa>", "..."]
}}
"""


def _rubrica_dettaglio() -> str:
    """Costruisce il testo descrittivo della rubrica per il prompt."""
    righe = []
    for nome, (descrizione, peso) in DIMENSIONI.items():
        righe.append(f"- {nome} (peso {peso:.0%}): {descrizione}")
    return "\n".join(righe)


def build_reflection_prompt(task_input: str, agent_output: str) -> tuple[str, str]:
    """Restituisce (system_prompt, user_prompt) per la chiamata di reflection.

    Args:
        task_input: Il task originale inviato all'agente (testo dell'ultimo HumanMessage).
        agent_output: L'output prodotto dall'agente (campo `result` dello stato).

    Returns:
        Tupla (system_prompt, user_prompt) da passare al provider LLM.
    """
    from . import prompt_registry
    _fmt = dict(
        task=task_input[:2000] if task_input else "(nessun input)",
        output=agent_output[:3000] if agent_output else "(nessun output)",
        rubrica_dettaglio=_rubrica_dettaglio(),
    )
    # Template dal DB (mig 0448) con fallback try/except alla costante.
    _tmpl = prompt_registry.get_prompt("system.reflection_user_template") or _TEMPLATE_UTENTE
    try:
        user = _tmpl.format(**_fmt)
    except (KeyError, IndexError, ValueError):
        user = _TEMPLATE_UTENTE.format(**_fmt)
    system = prompt_registry.get_prompt("system.reflection_rubric") or _SYSTEM_RUBRIC
    return system, user


# Regex per estrarre il JSON anche se il modello aggiunge testo circostante
_JSON_RE = re.compile(r"\{[\s\S]*\}", re.MULTILINE)


def parse_reflection_response(raw: str) -> dict[str, Any] | None:
    """Analizza la risposta grezza del modello e restituisce il dict di reflection.

    Se il parsing fallisce restituisce None (il chiamante ignora la reflection).

    Args:
        raw: Testo grezzo restituito dal modello LLM.

    Returns:
        Dict con `score`, `dimensions`, `weaknesses`, `suggestions` oppure None.
    """
    if not raw:
        return None

    # Tentativo 1: il testo e' JSON puro
    try:
        data = json.loads(raw.strip())
        return _validate_reflection(data)
    except (json.JSONDecodeError, ValueError):
        pass

    # Tentativo 2: estrae il primo blocco JSON dal testo
    match = _JSON_RE.search(raw)
    if match:
        try:
            data = json.loads(match.group(0))
            return _validate_reflection(data)
        except (json.JSONDecodeError, ValueError):
            pass

    logger.warning("reflection_rubric: impossibile parsare risposta reflection: %.200s", raw)
    return None


def _validate_reflection(data: dict[str, Any]) -> dict[str, Any]:
    """Valida e normalizza il dict di reflection.

    Raises:
        ValueError: se mancano campi obbligatori o i valori sono fuori range.
    """
    score = float(data.get("score", -1))
    if not (0.0 <= score <= 1.0):
        raise ValueError(f"score fuori range: {score}")

    dims = data.get("dimensions", {})
    for dim in DIMENSIONI:
        v = float(dims.get(dim, -1))
        if not (0.0 <= v <= 1.0):
            raise ValueError(f"dimensione {dim} fuori range: {v}")

    return {
        "score": round(score, 3),
        "dimensions": {dim: round(float(dims[dim]), 3) for dim in DIMENSIONI},
        "weaknesses": [str(w) for w in (data.get("weaknesses") or [])[:3]],
        "suggestions": [str(s) for s in (data.get("suggestions") or [])[:3]],
    }


def aggregate_score(dimensions: dict[str, float]) -> float:
    """Calcola il punteggio aggregato ponderato dalle dimensioni.

    Utile per ricalcolare lo score se le dimensioni vengono modificate.
    """
    total = 0.0
    for nome, (_, peso) in DIMENSIONI.items():
        total += dimensions.get(nome, 0.0) * peso
    return round(total, 3)
