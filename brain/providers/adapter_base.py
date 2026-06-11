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


def anthropic_tool_to_openai(tool: dict) -> dict:
    """Converte un tool definition Anthropic nel formato OpenAI function.
    Applica anche la compressione dello schema (BP6 piano riduzione token).

    Punto unico (regola L / ADR 0026, S80): prima viveva in
    `openai_provider._anthropic_tool_to_openai`, ma quel collocamento creava
    una dipendenza inversa (adapter_base layer-basso ↔ openai_provider
    layer-alto). Ora vive qui dove appartiene.
    """
    from ._schema_utils import compress_schema, _truncate_text, DEFAULT_TOOL_DESCR_MAX

    raw_schema = tool.get("input_schema", {"type": "object", "properties": {}})
    return {
        "type": "function",
        "function": {
            "name": tool["name"],
            "description": _truncate_text(tool.get("description", ""), DEFAULT_TOOL_DESCR_MAX),
            "parameters": compress_schema(raw_schema),
        },
    }


def convert_messages_to_openai(messages: list[dict]) -> list[dict]:
    """Converte messaggi in formato Anthropic (con tool_use/tool_result) in
    formato OpenAI. Punto unico (regola L / ADR 0026, S80) usato da tutti gli
    adapter OpenAI-compatible (openai, deepseek, mistral)."""
    import json as _json

    result: list[dict] = []
    for msg in messages:
        role = msg.get("role", "user")
        content = msg.get("content", "")

        if isinstance(content, str):
            result.append({"role": role, "content": content})
        elif isinstance(content, list):
            text_parts: list[str] = []
            reasoning_parts: list[str] = []
            tool_calls: list[dict] = []
            tool_results: list[dict] = []

            for block in content:
                btype = block.get("type")
                if btype == "text":
                    text_parts.append(block.get("text", ""))
                elif btype == "reasoning":
                    # DeepSeek thinking mode: il reasoning_content del turno con
                    # tool_calls DEVE essere rispedito (400 altrimenti). Guarded:
                    # presente solo se il thinking era attivo (policy 'native');
                    # con 'disable_for_tools' il blocco non esiste -> no-op.
                    reasoning_parts.append(block.get("reasoning", ""))
                elif btype == "tool_use":
                    tool_calls.append({
                        "id": block["id"],
                        "type": "function",
                        "function": {
                            "name": block["name"],
                            "arguments": _json.dumps(block.get("input", {})),
                        },
                    })
                elif btype == "tool_result":
                    tool_results.append({
                        "role": "tool",
                        "tool_call_id": block["tool_use_id"],
                        "content": block.get("content", ""),
                    })

            if tool_results:
                result.extend(tool_results)
            elif tool_calls:
                oai_msg: dict[str, Any] = {"role": "assistant"}
                if text_parts:
                    oai_msg["content"] = " ".join(text_parts)
                else:
                    oai_msg["content"] = None
                if reasoning_parts:
                    oai_msg["reasoning_content"] = "\n".join(p for p in reasoning_parts if p)
                oai_msg["tool_calls"] = tool_calls
                result.append(oai_msg)
            else:
                result.append({"role": role, "content": " ".join(text_parts)})
        else:
            result.append({"role": role, "content": str(content)})

    return result


def prepare_openai_compat_request(
    provider_name: str,
    model: str,
    max_tokens: int,
    messages: list[dict],
    system_text: str | None,
) -> tuple[ProviderCapability | None, list[dict], int]:
    """Pre-processing condiviso dei `generate_agent_turn` OpenAI-compatible:
    carica la capability del modello, risolve `max_tokens`, converte i messages
    nel formato OpenAI e inserisce il `system_text` in testa. Punto unico
    (regola L / ADR 0026, S78): prima duplicato in `deepseek_provider` e
    `mistral_provider`.

    Ritorna ``(cap, oai_messages, max_tokens_risolto)``. Capability=None se la
    lookup fallisce (loggato come WARNING, i parametri richiesti vengono usati
    cosi' come sono).
    """
    from .capability_loader import load_capability

    cap: ProviderCapability | None = None
    try:
        cap = load_capability(provider_name, model)
        max_tokens = resolve_max_tokens(cap, max_tokens)
    except Exception as cap_err:  # noqa: BLE001
        logger.warning(
            "capability %s/%s non disponibile (%s): uso parametri richiesti",
            provider_name, model, cap_err,
        )
        cap = None
    oai_messages = convert_messages_to_openai(messages)
    oai_messages = _sanitize_tool_message_sequence(oai_messages)
    oai_messages = _strip_trailing_assistant(oai_messages)
    if system_text:
        oai_messages.insert(0, {"role": "system", "content": system_text})
    return cap, oai_messages, max_tokens


def _strip_trailing_assistant(oai_messages: list[dict]) -> list[dict]:
    """Rimuove gli `assistant` message in CODA. I provider OpenAI-compat strict
    rifiutano una conversazione che termina con assistant: Mistral risponde HTTP
    422 "Expected last role User or Tool ... but got assistant". Punto unico
    cross-provider (regola L): tutti i provider OpenAI-compatible passano da qui.

    Semantica: `generate_agent_turn` deve GENERARE il prossimo turno assistant,
    quindi l'input non deve mai terminare con assistant — si genera "dopo"
    user/tool, non "dopo" assistant. Nel flusso normale la history finisce gia'
    con user/tool e questa funzione e' un no-op. Si attiva nel cascade/fallback
    mid-turn dopo un soft-failure (is_soft_failure): la history reinviata al
    provider di fallback finisce con il turno "molle" (end_turn, poco contenuto,
    niente tool) del provider che ha mollato. Rimuoverlo e' esattamente cio' che
    vogliamo: il fallback RIGENERA quel turno dalla stessa situazione (ultimo
    user/tool) che aveva il provider fallito.

    Guard: non svuota mai la lista. Se restassero solo assistant (caso
    patologico, mai osservato), ripristina l'originale e lascia che sia il
    provider a dare l'errore esplicito invece di inviare una richiesta vuota."""
    out = list(oai_messages)
    dropped = 0
    while out and out[-1].get("role") == "assistant":
        out.pop()
        dropped += 1
    if not out:
        # Solo assistant: non possiamo normalizzare senza svuotare. Ripristina.
        logger.error(
            "adapter sanitize: conversazione di soli assistant (%d msg), "
            "impossibile garantire ultimo ruolo user/tool — invio invariato",
            dropped,
        )
        return list(oai_messages)
    if dropped:
        logger.warning(
            "adapter sanitize: rimossi %d assistant message in coda "
            "(provider OpenAI-compat richiede ultimo ruolo user/tool; "
            "tipico di cascade/fallback dopo soft-failure)",
            dropped,
        )
    return out


def _sanitize_tool_message_sequence(oai_messages: list[dict]) -> list[dict]:
    """Rimuove `tool` messages ORFANI: quelli il cui `tool_call_id` non
    corrisponde ad alcun `tool_calls.id` dichiarato da un `assistant` precedente
    nella stessa sequenza. Punto unico cross-provider (regola L): tutti i
    provider OpenAI-compatible (mistral/deepseek/openai/gemini-openai) rifiutano
    con HTTP 400 invalid_request_message_order una sequenza con `role 'tool'`
    senza l'`assistant(tool_calls=[...id])` che la precede.
    Casi che generano questo stato (osservati in prod):
      - `rolling_summary` taglia la history nel mezzo di una coppia
        AIMessage(tool_calls)+ToolMessage e il primo ToolMessage del cutoff
        resta orfano.
      - fallback mid-turn / ricostruzione parziale della history da DB
        (es. campo messages_json) dove l'AIMessage padre non e' stato
        persistito ma i suoi ToolMessage si.
    Il drop e' loggato come WARNING con il tool_call_id, cosi' i casi non
    triviali sono visibili e si puo' risalire alla causa upstream."""
    # ADIACENZA per-blocco (incidente run 2c6e41fb, 422 Mistral "Unexpected tool
    # call id ... in tool results" + "An assistant message with 'tool_calls'
    # must be followed by ..."): il set GLOBALE degli id dichiarati non basta.
    # I provider strict richiedono che (a) ogni tool message segua IMMEDIATAMENTE
    # l'assistant che ha dichiarato quel tool_call_id (stesso blocco, prima del
    # prossimo messaggio non-tool) e (b) ogni tool_call dell'assistant abbia il
    # suo tool message. La compressione della history (rolling_summary/compress)
    # spezza i blocchi nel mezzo producendo entrambe le violazioni. Qui:
    #   - tool message fuori dal blocco corrente -> DROP (orfano);
    #   - tool_call senza result a fine blocco -> tool result SINTETICO
    #     ("non disponibile, troncato"): preserva il contenuto dell'assistant
    #     mantenendo la sequenza valida.
    out: list[dict] = []
    pending: set[str] = set()  # id del blocco assistant corrente non ancora consumati
    dropped = 0
    synthesized = 0

    def _flush_pending() -> None:
        nonlocal synthesized
        for tid in sorted(pending):
            out.append({
                "role": "tool",
                "tool_call_id": tid,
                "content": "[tool result non disponibile: troncato dalla compressione della history]",
            })
            synthesized += 1
        pending.clear()

    for msg in oai_messages:
        role = msg.get("role")
        if role == "tool":
            tid = msg.get("tool_call_id")
            if not tid or tid not in pending:
                dropped += 1
                continue
            pending.discard(tid)
            out.append(msg)
            continue
        # Messaggio non-tool: chiude il blocco corrente (sintetizza i mancanti).
        _flush_pending()
        if role == "assistant":
            for tc in msg.get("tool_calls") or []:
                tid = tc.get("id")
                if tid:
                    pending.add(tid)
        out.append(msg)
    _flush_pending()

    if dropped or synthesized:
        logger.warning(
            "adapter sanitize: %d tool message orfani rimossi, %d result sintetici "
            "per tool_call senza esito (su %d msg totali)",
            dropped, synthesized, len(oai_messages),
        )
    return out


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
    # I modelli EFFETTIVAMENTE in thinking mode non accettano un tool_choice
    # forzato: DeepSeek V4 risponde HTTP 400 "Thinking mode does not support this
    # tool_choice", e lo stesso vincolo vale per Anthropic con extended thinking
    # e per i reasoning OpenAI (o1/o3, gia' style 'none'). In quel caso si degrada
    # ad "auto": il modello decide da se' quando invocare i tool.
    #
    # CRITICO: la verita' e' `cap.thinking_active_with_tools`, NON `cap.thinking`
    # statico. Quasi tutti i modelli agentici moderni (claude-4.x, gemini-2.5/3.x,
    # gpt-5.x, deepseek-v4-pro) hanno uses_thinking_mode=TRUE ma policy
    # 'disable_for_tools': in una richiesta con tool l'adapter SPEGNE il thinking,
    # quindi il tool_choice forzato e' accettato e va usato. Guardare `cap.thinking`
    # qui annullava silenziosamente la forzatura anti-narration del primo turno per
    # tutti questi modelli -> "pianifica e non agisce". Il gate ora coincide con la
    # disabilitazione del thinking lato adapter (punto unico, regola L).
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
        force_first = _base_force and not weak and not cap.thinking_active_with_tools

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


# ── Thinking OFF per task interni testuali (mig 0390) ────────────────────────
# Cache 60s del flag settings.providers.thinking_disable_internal_text, stesso
# pattern del TTL in capability_loader. Default True (il valore canonico vive
# nel DB, mig 0390; il default qui copre solo il best-effort se il DB cade).
_INTERNAL_TEXT_THINKING_KEY = "providers.thinking_disable_internal_text"
_internal_text_flag: bool = True
_internal_text_ts: float = 0.0
_INTERNAL_TEXT_REFRESH_S = 60.0


def _internal_text_thinking_disabled() -> bool:
    """Flag DB (cache 60s): thinking off nelle chiamate testuali interne."""
    global _internal_text_flag, _internal_text_ts
    import time

    now = time.time()
    if (now - _internal_text_ts) < _INTERNAL_TEXT_REFRESH_S:
        return _internal_text_flag
    try:
        from brain.utils.settings_db import get_bool_setting
        _internal_text_flag = get_bool_setting(_INTERNAL_TEXT_THINKING_KEY, True)
    except Exception:  # noqa: BLE001
        _internal_text_flag = True
    _internal_text_ts = now
    return _internal_text_flag


def should_disable_thinking(
    cap: ProviderCapability | None,
    has_tools: bool,
    internal_task: bool = False,
) -> bool:
    """PUNTO UNICO (regola L): decide se l'adapter deve forzare la modalita'
    NON-THINKING per questa richiesta. Si applica SOLO ai modelli dual-mode con
    policy 'disable_for_tools' (ADR 0025): per gli altri (policy 'native',
    'none', 'exclude' o capability assente) non si tocca nulla.

    Due rami:
    - ``has_tools``: comportamento ADR 0025 invariato — nel loop agentico il
      thinking va spento (function calling deterministico, niente
      reasoning_content da ri-passare).
    - ``internal_task`` senza tool (mig 0390): i task interni TESTUALI (purpose:
      chat title, doc gen, summary, classifier, ...) NON beneficiano del
      reasoning e col thinking acceso bruciano il budget di output in
      reasoning_content producendo content vuoto (incidenti "deepseek non
      scrive" / hollow_completion). Gate dal setting DB
      ``providers.thinking_disable_internal_text`` (regola G, cache 60s).

    La chat utente (non marcata internal_task, senza tool) mantiene il
    comportamento di default del modello: il reasoning li' puo' avere valore.
    Il COME spegnere e' provider-specifico (deepseek: extra_body
    ``{"thinking": {"type": "disabled"}}``); qui vive solo la decisione.
    """
    if cap is None or not cap.thinking_disabled_for_tools:
        return False
    if has_tools:
        return True
    return internal_task and _internal_text_thinking_disabled()


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
    intent: str | None = None,
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
    # Intent CONVERSAZIONALE: una chiusura naturale con sola risposta testuale e'
    # la risposta corretta, non un "modello che molla senza usare i tool". Non e'
    # soft-failure -> niente cascade/escalation (ne' "narrazione a vuoto"). Rete
    # di sicurezza per gli intent conversazionali che dovessero comunque arrivare
    # qui (per la chat pura i tool sono gia' azzerati a monte nel router_node).
    if (intent or "").strip().lower() in ("chat", "general_chat"):
        return False
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
