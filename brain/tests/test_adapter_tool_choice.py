"""Regressione: resolve_tool_choice non deve forzare un tool_choice sui modelli
in thinking mode.

Root cause storico: DeepSeek V4 (thinking mode) risponde HTTP 400
"Thinking mode does not support this tool_choice" se riceve tool_choice="required".
La capability aveva thinking=false, quindi il primo turno forzava "required".
Il fix (adapter_base.resolve_tool_choice) aggiunge il guard `not cap.thinking`:
per qualunque modello thinking il tool_choice degrada ad "auto", senza forzatura.
"""
from brain.providers._models import ProviderCapability
from brain.providers.adapter_base import resolve_tool_choice


def _cap(**overrides) -> ProviderCapability:
    """Capability minima con default sensati; override mirati per il caso in test."""
    base = dict(
        provider="deepseek",
        model="deepseek-v4-flash",
        tool_use=True,
        vision=False,
        thinking=False,
        max_context_tokens=128000,
        default_max_output_tokens=8192,
        max_output_tokens_hard=16384,
        tool_choice_style="openai_required",
        tool_choice_first_turn_force=True,
        schema_strict=False,
        schema_dialect="openai_loose",
        tool_call_format="openai_delta",
        max_tools_in_request=None,
        supports_prompt_cache=False,
        prompt_cache_dialect=None,
        supports_parallel_tools=True,
        stop_reason_dialect="openai_finish_reason",
        soft_failure_iter_threshold=3,
        soft_failure_content_threshold=800,
        history_keep_recent_messages=12,
        history_max_old_tool_result_chars=2000,
        request_timeout_seconds=60,
        connect_timeout_seconds=10,
        tool_result_max_chars=6000,
        tool_result_max_bytes=512000,
        tool_result_max_lines=2000,
    )
    base.update(overrides)
    return ProviderCapability(**base)


# Primo turno: nessun tool_result nella history -> is_first_agent_turn True.
FIRST_TURN = [{"role": "user", "content": "Crea il file vite.config.ts"}]


def test_non_thinking_openai_required_forces_required_on_first_turn():
    cap = _cap(thinking=False, tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, FIRST_TURN) == "required"


def test_thinking_openai_required_degrades_to_auto_on_first_turn():
    # Caso DeepSeek V4: thinking mode -> niente forzatura, altrimenti HTTP 400.
    cap = _cap(thinking=True, tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, FIRST_TURN) == "auto"


def test_thinking_anthropic_any_degrades_to_auto_on_first_turn():
    cap = _cap(thinking=True, tool_choice_style="anthropic_any")
    assert resolve_tool_choice(cap, FIRST_TURN) == {"type": "auto"}


def test_thinking_google_any_degrades_to_auto_on_first_turn():
    cap = _cap(thinking=True, tool_choice_style="google_function_calling_any")
    assert resolve_tool_choice(cap, FIRST_TURN) == {
        "function_calling_config": {"mode": "AUTO"}
    }


def test_style_none_returns_none_regardless_of_thinking():
    cap = _cap(thinking=True, tool_choice_style="none")
    assert resolve_tool_choice(cap, FIRST_TURN) is None
