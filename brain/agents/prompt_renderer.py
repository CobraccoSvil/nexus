"""Rendering runtime dei placeholder nei prompt agente.

I prompt nello schema XML (vedi migrazione 0086) contengono placeholder
del tipo `{{lang_hint}}`, `{{type_hint}}`, `{{repo_summary}}`. Questo modulo
li sostituisce con i valori derivati dallo stato corrente del run (intent,
metadati del repo, ecc.).

Regola fondamentale: MAI lasciare `{{...}}` letterale nel prompt finale.
Se il valore non e' disponibile, la sostituzione produce stringa vuota.

Estensibile: nuovi placeholder vengono registrati nel dict `_RESOLVERS` con
una funzione `resolve(state, intent) -> str`. Il modulo e' deliberatamente
puro (no DB call) per non aggiungere latenza al router_node.
"""
from __future__ import annotations

import logging
import re
from typing import Any, Callable

logger = logging.getLogger(__name__)

# Pattern che cattura placeholder {{nome}} con eventuali spazi interni.
_PLACEHOLDER_RE = re.compile(r"\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\}\}")


# ── Mappa intent -> hint sul tipo di output atteso dall'agente ──────────────
# Usata per risolvere {{type_hint}}. Tradotto in italiano per rispettare la
# regola LINGUA dei prompt.
_INTENT_TYPE_HINT: dict[str, str] = {
    "code_generation": "moduli e funzioni produzione-ready",
    "code_modification": "modifiche chirurgiche al codice esistente",
    "bug_fix": "fix mirata + test di regressione",
    "refactoring": "refactor a parita' di comportamento",
    "test_generation": "test unitari indipendenti",
    "code_review": "report di code review strutturato",
    "documentation": "documentazione tecnica concisa",
    "architecture": "design architetturale e contratti",
    "performance": "ottimizzazione misurata before/after",
    "security": "audit di sicurezza con remediation",
    "database": "schema, query e migrazioni idempotenti",
    "infrastructure": "configurazione infrastrutturale riproducibile",
    "deployment": "pipeline di deployment automatizzata",
    "chat": "risposta concisa e accionabile",
}


def _resolve_lang_hint(state: dict[str, Any], _intent: str) -> str:
    """Estrae l'hint sul linguaggio dominante dal repo.

    Cerca in ordine: state["repo_lang"], state["lang"],
    state["project_metadata"]["lang"]. Se assente, ritorna stringa vuota.

    Output: ", linguaggio Rust" / ", linguaggio TypeScript" / "" (mai placeholder).
    """
    lang = (
        state.get("repo_lang")
        or state.get("lang")
        or (state.get("project_metadata") or {}).get("lang")
    )
    if not lang or not isinstance(lang, str):
        return ""
    lang_clean = lang.strip()
    if not lang_clean:
        return ""
    return f", linguaggio {lang_clean}"


def _resolve_type_hint(_state: dict[str, Any], intent: str) -> str:
    """Risolve {{type_hint}} dal task intent corrente.

    Default: "task generico" se l'intent non ha mapping (mai placeholder).
    """
    return _INTENT_TYPE_HINT.get(intent, "task generico")


def _resolve_repo_summary(state: dict[str, Any], _intent: str) -> str:
    """Estrae il riassunto del repository dal state.

    Cerca in ordine: state["repo_summary"], state["repo_card"]["summary"],
    state["project_metadata"]["summary"]. Default: stringa generica neutra.
    """
    summary = (
        state.get("repo_summary")
        or (state.get("repo_card") or {}).get("summary")
        or (state.get("project_metadata") or {}).get("summary")
    )
    if isinstance(summary, str) and summary.strip():
        return summary.strip()
    return "repository utente (metadati non disponibili)"


# Registry estensibile dei resolver per placeholder noti.
_RESOLVERS: dict[str, Callable[[dict[str, Any], str], str]] = {
    "lang_hint": _resolve_lang_hint,
    "type_hint": _resolve_type_hint,
    "repo_summary": _resolve_repo_summary,
}


def render(template: str, state: dict[str, Any], intent: str = "chat") -> str:
    """Sostituisce tutti i placeholder noti nel template.

    - Placeholder con resolver registrato: sostituito col valore calcolato.
    - Placeholder sconosciuto: sostituito con stringa vuota + warning log
      (cosi' il prompt non contiene mai `{{...}}` letterale).

    Args:
        template: testo del prompt potenzialmente con placeholder.
        state: stato corrente del run agente (chiavi best-effort).
        intent: intent classificato dal router (es. "bug_fix").

    Returns:
        Il testo con tutti i placeholder risolti.
    """
    if not template:
        return ""

    def _sub(match: re.Match[str]) -> str:
        name = match.group(1)
        resolver = _RESOLVERS.get(name)
        if resolver is None:
            logger.warning(
                "prompt_renderer: placeholder sconosciuto '{{%s}}', sostituito con stringa vuota",
                name,
            )
            return ""
        try:
            return resolver(state, intent) or ""
        except Exception as exc:  # noqa: BLE001
            logger.error(
                "prompt_renderer: resolver '%s' ha sollevato %s, fallback a stringa vuota",
                name, exc,
            )
            return ""

    return _PLACEHOLDER_RE.sub(_sub, template)


def register_resolver(name: str, resolver: Callable[[dict[str, Any], str], str]) -> None:
    """Permette ai test o a moduli esterni di registrare nuovi resolver."""
    _RESOLVERS[name] = resolver
