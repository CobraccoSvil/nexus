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

import logging

from ._models import CanonicalTool, ProviderCapability
from ._schema_utils import is_first_agent_turn
from .tool_translator import dialect_for_capability

logger = logging.getLogger(__name__)

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
    force_override: bool | None = None,
) -> Any:
    """Spec tool_choice nel dialetto del provider, in base a cap.tool_choice_style.

    Forza l'uso di un tool al primo turno (se cap.tool_choice_first_turn_force)
    per evitare il pattern narrate-without-act; ai turni successivi lascia auto.
    Ritorna None per i provider tool-mute (style 'none').

    `force_override` (ADR 0018 leva 2): se valorizzato, fa OVERRIDE della
    decisione interna `force_first`. True = forza la tool call; False =
    disattiva la forzatura per questo turno (usato dal retry-senza-forcing dopo
    un errore provider). None = comportamento storico (first_turn_force da cap).
    Il guard `thinking`/`weak`/`style none` resta sempre prioritario: non si
    forza mai un modello che l'API rifiuterebbe (es. DeepSeek thinking mode).
    """
    style = cap.tool_choice_style
    if style == "none":
        return None

    weak = bool(weak_models) and any(t in cap.model.lower() for t in weak_models)
    # I modelli in thinking/reasoning mode non accettano un tool_choice forzato:
    # DeepSeek V4 risponde HTTP 400 "Thinking mode does not support this
    # tool_choice", e lo stesso vincolo vale per Anthropic con extended thinking
    # e per i reasoning OpenAI (o1/o3, gia' style 'none'). In quel caso si degrada
    # ad "auto": il modello decide da se' quando invocare i tool, senza la
    # forzatura anti-narration del primo turno. Guard cross-provider, valido per
    # qualunque modello thinking presente o futuro (la verita' resta cap.thinking).
    if force_override is False:
        # Override esplicito: il chiamante chiede di NON forzare (retry dopo
        # errore di forcing). Si degrada ad auto per questo turno.
        force_first = False
    else:
        # `force_override is True` rafforza la forzatura anche oltre il primo
        # turno (turni d'azione iniziali, ADR 0018 (b)), ma sempre rispettando
        # i guard hard (weak/thinking) che renderebbero il forcing un errore API.
        _base_force = (
            cap.tool_choice_first_turn_force and is_first_agent_turn(messages)
            if force_override is None
            else True
        )
        force_first = _base_force and not weak and not cap.thinking

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
