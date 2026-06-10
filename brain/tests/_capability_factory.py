"""Factory condivisa di ProviderCapability sintetiche per i test (regola L).

Lo stesso boilerplate "capability minima + override" era gia' duplicato in
test_tool_choice_forcing / test_adapter_tool_choice / test_tool_choice_thinking_gate;
i test NUOVI devono importare da qui invece di copiarlo (gate jscpd ratchet).
"""
from __future__ import annotations

from brain.providers._models import ProviderCapability


def make_capability(**overrides) -> ProviderCapability:
    """Capability minima sintetica; si sovrascrivono solo i campi rilevanti."""
    base = dict(
        provider="test",
        model="test-model",
        tool_use=True,
        vision=False,
        thinking=False,
        max_context_tokens=128000,
        default_max_output_tokens=4096,
        max_output_tokens_hard=8192,
        tool_choice_style="openai_required",
        tool_choice_first_turn_force=True,
        schema_strict=False,
        schema_dialect="openai",
        tool_call_format="openai_delta",
        max_tools_in_request=None,
        supports_prompt_cache=False,
        prompt_cache_dialect=None,
        supports_parallel_tools=True,
        stop_reason_dialect="openai",
        soft_failure_iter_threshold=3,
        soft_failure_content_threshold=2,
        history_keep_recent_messages=20,
        history_max_old_tool_result_chars=2000,
        request_timeout_seconds=120,
        connect_timeout_seconds=10,
        tool_result_max_chars=20000,
        tool_result_max_bytes=200000,
        tool_result_max_lines=500,
        agentic_thinking_policy="none",
    )
    base.update(overrides)
    return ProviderCapability(**base)
