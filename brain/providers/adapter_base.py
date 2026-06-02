"""Helper capability-driven per gli adapter provider (M3/M4/M5 del piano).

Funzioni pure (testabili senza chiamate API) che centralizzano le decisioni che
prima erano hardcoded nei singoli provider, leggendole dalle capability DB:

- resolve_max_tokens   (M5): budget risposta entro il tetto hard del modello.
- resolve_tool_choice  (M5): spec tool_choice nel dialetto del provider.
- translate_tools_for  (M1): payload tool + doc block per il dialetto del modello.
- is_soft_failure      (M4): rileva il turno "vuoto" (end_turn senza tool e con
                              poco contenuto) che va trattato come fallimento
                              soft e instradato al fallback chain.

Regola G: ogni soglia/scelta viene dalla ProviderCapability (DB), mai hardcoded.
I provider esistenti possono adottare queste funzioni in modo incrementale senza
cambiare la firma pubblica di generate_agent_turn.
"""
from __future__ import annotations

from typing import Any

from ._models import CanonicalTool, ProviderCapability
from ._schema_utils import is_first_agent_turn
from .tool_translator import dialect_for_capability

# stop_reason che indicano una chiusura "naturale" del turno (non un errore).
_NATURAL_STOPS = {"end_turn", "stop", "", None}


def resolve_max_tokens(cap: ProviderCapability, requested: int = 0) -> int:
    """max_tokens effettivi: il richiesto limitato al tetto hard del modello,
    oppure il default del modello se non richiesto. Niente costanti hardcoded."""
    return cap.clamp_max_output_tokens(requested)


def resolve_tool_choice(
    cap: ProviderCapability,
    messages: list[dict],
    *,
    weak_models: tuple[str, ...] = (),
) -> Any:
    """Spec tool_choice nel dialetto del provider, in base a cap.tool_choice_style.

    Forza l'uso di un tool al primo turno (se cap.tool_choice_first_turn_force)
    per evitare il pattern narrate-without-act; ai turni successivi lascia auto.
    Ritorna None per i provider tool-mute (style 'none').
    """
    style = cap.tool_choice_style
    if style == "none":
        return None

    weak = bool(weak_models) and any(t in cap.model.lower() for t in weak_models)
    force_first = cap.tool_choice_first_turn_force and not weak and is_first_agent_turn(messages)

    if style == "anthropic_any":
        return {"type": "any"} if force_first else {"type": "auto"}
    if style == "openai_required":
        return "required" if force_first else "auto"
    if style == "openai_auto":
        return "auto"
    if style == "google_function_calling_any":
        mode = "ANY" if force_first else "AUTO"
        return {"function_calling_config": {"mode": mode}}
    # Stile sconosciuto: nessuna forzatura (degradazione esplicita, non crash).
    return "auto"


def translate_tools_for(
    cap: ProviderCapability, tools: list[CanonicalTool]
) -> tuple[list[dict] | None, str]:
    """(payload_tool, doc_block) per il dialetto del modello.

    payload_tool e None per i provider tool-mute; in quel caso doc_block contiene
    la descrizione testuale dei tool da iniettare nel system prompt.
    """
    dialect = dialect_for_capability(cap)
    payload = dialect.translate_tools(tools, cap)
    doc = dialect.documentation_block(tools) if payload is None else ""
    return payload, doc


def is_soft_failure(
    metadata: dict[str, Any] | None,
    content: str,
    cap: ProviderCapability,
    iteration: int | None = None,
    first_turn: bool = True,
) -> bool:
    """True se il turno e un fallimento soft: chiusura naturale (end_turn/stop)
    SENZA tool-call e con contenuto sotto soglia, MENTRE il modello non ha ancora
    agito. Indica un provider che "molla" all'inizio invece di usare i tool ->
    il chiamante puo instradare al fallback (M4).

    `first_turn`: il soft-failure si applica SOLO se siamo ancora al primo turno
    agente (nessun tool_result nella history). Se il modello ha gia eseguito tool
    nei turni precedenti, una chiusura naturale e legittima (fine task) e NON va
    trattata come fallimento -> evita il falso positivo del fallback a fine run.

    `iteration` e opzionale: se fornita, ulteriore guardia sulle prime iterazioni
    (cap.soft_failure_iter_threshold).
    """
    meta = metadata or {}
    stop = meta.get("stop_reason")
    if stop not in _NATURAL_STOPS:
        return False
    blocks = meta.get("tool_use_blocks") or []
    if blocks:
        return False
    # Il modello ha gia lavorato (turni con tool alle spalle): chiusura legittima.
    if not first_turn:
        return False
    if iteration is not None and iteration >= cap.soft_failure_iter_threshold:
        return False
    return len(content or "") < cap.soft_failure_content_threshold
