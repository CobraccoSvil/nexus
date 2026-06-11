"""Parser response provider OpenAI-compatible (punto unico, regola L / ADR 0026).

Prima il blocco "parse choice + tool_calls + fallback inline_tool_invocations"
era duplicato pari-pari (34L cluster jscpd) in:
  - brain/providers/openai_provider.py::generate_agent_turn
  - brain/providers/mistral_provider.py::generate_agent_turn
  - brain/providers/deepseek_provider.py::generate_agent_turn (in parte)

Ora vive qui. La funzione e' deliberatamente "pura" sull'output (no chiamate
SDK), per essere paritetica fra i tre provider OpenAI-compatible.
"""
from __future__ import annotations

import json
from typing import Any


def parse_openai_compatible_choice(
    msg: Any,
    finish_reason: str | None,
    tools: list[dict],
    text_content: str,
    initial_assistant_content: list[dict] | None = None,
) -> tuple[str, str, list[dict], list[dict]]:
    """Interpreta una `choice.message` di un SDK OpenAI-compatible.

    Ritorna ``(stop_reason, text_content, tool_use_blocks, assistant_content)``.

    - Se ``finish_reason == "tool_calls"`` e ``msg.tool_calls`` non e' vuoto,
      estrae i tool_use_blocks dai tool_calls (deserializzando ``function.arguments``
      come JSON; fallback a dict vuoto su errore parse).
    - Altrimenti tenta di estrarre XML inline tool invocations dal testo (vedi
      ``_schema_utils.parse_inline_tool_invocations``); se trovate, le promuove
      a tool_use_blocks e ripulisce il testo.
    - In assenza di tool calls, ``assistant_content`` contiene il solo blocco
      `{"type": "text", ...}` se il testo non e' vuoto.

    ``initial_assistant_content``: blocchi da preservare in testa
    all'``assistant_content`` (es. il blocco ``reasoning`` del thinking mode
    DeepSeek, che DEVE essere rispedito nei turni successivi). NB: nel ramo
    XML inline il testo ripulito viene inserito a indice 0, quindi PRIMA di
    questi blocchi (comportamento storico DeepSeek preservato).

    ``stop_reason`` parte da ``"end_turn"`` e diventa ``"tool_use"`` se almeno
    un blocco tool e' stato emesso.
    """
    stop_reason = "end_turn"
    tool_use_blocks: list[dict] = []
    assistant_content: list[dict] = (
        list(initial_assistant_content) if initial_assistant_content else []
    )

    if finish_reason == "tool_calls" and getattr(msg, "tool_calls", None):
        stop_reason = "tool_use"
        for tc in msg.tool_calls:
            try:
                args = json.loads(tc.function.arguments)
            except Exception:
                args = {}
            block = {"id": tc.id, "name": tc.function.name, "input": args}
            tool_use_blocks.append(block)
            assistant_content.append({"type": "tool_use", **block})
        return stop_reason, text_content, tool_use_blocks, assistant_content

    # Fallback: XML inline tool invocations dentro il testo
    tool_names = {t.get("name", "") for t in tools if t.get("name")}
    from ._schema_utils import parse_inline_tool_invocations

    xml_blocks, cleaned_text = parse_inline_tool_invocations(text_content, tool_names)
    if xml_blocks:
        stop_reason = "tool_use"
        for blk in xml_blocks:
            tool_use_blocks.append(blk)
            assistant_content.append({"type": "tool_use", **blk})
        if cleaned_text.strip():
            assistant_content.insert(0, {"type": "text", "text": cleaned_text})
        text_content = cleaned_text
    elif text_content:
        assistant_content.append({"type": "text", "text": text_content})

    return stop_reason, text_content, tool_use_blocks, assistant_content


def build_agent_turn_result(
    provider: str,
    model: str,
    text_content: str,
    stop_reason: str,
    tool_use_blocks: list[dict],
    assistant_content: list[dict],
    usage_data: dict[str, Any],
) -> Any:
    """Coda comune di ``generate_agent_turn`` per i provider OpenAI-compatible.

    Costruisce il ``ProviderResult`` di successo con i metadata standard del
    turno agentico. Prima era duplicata pari-pari in openai/deepseek/mistral.
    """
    from .base import ProviderResult

    return ProviderResult(
        provider=provider,
        model=model,
        content=text_content,
        metadata={
            "stop_reason": stop_reason,
            "tool_use_blocks": tool_use_blocks,
            "assistant_content": assistant_content,
            "usage": usage_data,
        },
    )


def build_generate_result(
    provider: str,
    model: str,
    response: Any,
    content: str | None = None,
) -> Any:
    """Coda comune di ``generate`` (non agentico) per i provider
    OpenAI-compatible: ``ProviderResult`` di successo con usage standard
    (prompt/completion/total) + ``finish_reason``. Prima era duplicata
    pari-pari in openai/deepseek/mistral (cluster jscpd E3).

    ``content``: testo gia' post-processato dal provider (es. strip dei marker
    DSML in deepseek); default il content della prima choice.
    """
    from .base import ProviderResult

    choice = response.choices[0]
    return ProviderResult(
        provider=provider,
        model=model,
        content=(choice.message.content or "") if content is None else content,
        metadata={
            "usage": {
                "prompt_tokens": response.usage.prompt_tokens,
                "completion_tokens": response.usage.completion_tokens,
                "total_tokens": response.usage.total_tokens,
            },
            "finish_reason": choice.finish_reason,
        },
    )


def build_agent_turn_error(exc: Exception, provider: str, model: str) -> Any:
    """Coda d'errore comune di ``generate`` e ``generate_agent_turn``
    (OpenAI-compatible). Contratto dati B (regola L): error_class +
    http_status strutturati dall'oggetto SDK reale, niente fallback lessicale
    a valle.

    Delega la classificazione a ``format_error_result`` (punto unico W2.2) e
    impacchetta il ``ProviderResult`` d'errore con il contratto ``[Error: ...]``
    atteso dai chiamanti (cascade fallback, soft-failure detection).
    """
    from .base import ProviderResult
    from .error_handler import format_error_result

    meta = format_error_result(exc, provider, model)
    return ProviderResult(
        provider=provider,
        model=model,
        content=f"[Error: {meta['error']}]",
        metadata=meta,
    )
