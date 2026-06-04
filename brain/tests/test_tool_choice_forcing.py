"""Test ADR 0018 (b): funzione pura should_force_tool_choice + override
resolve_tool_choice.

Verifica i criteri di forcing (forza / non-forza per ogni criterio, flag off,
provider non supportato) e che l'override force_override di resolve_tool_choice
rispetti i guard hard (thinking/weak/style none) ma forzi quando richiesto.

Esecuzione senza pytest:
    python3 -c "import brain.tests.test_tool_choice_forcing as t; t.run_all()"
"""
from brain.agents.nodes.helpers import (
    provider_style_supports_forcing,
    should_force_tool_choice,
)
from brain.providers._models import ProviderCapability
from brain.providers.adapter_base import resolve_tool_choice


# ── should_force_tool_choice: criteri ──────────────────────────────────────

def _base_args(**ov):
    args = dict(
        tools_available=True,
        action_oriented=True,
        iteration=0,
        in_discovery_phase=False,
        provider_supports_forcing=True,
        enabled=True,
        max_iteration=2,
    )
    args.update(ov)
    return args


def test_forces_when_all_criteria_met():
    assert should_force_tool_choice(**_base_args()) is True


def test_forces_at_threshold_iteration():
    # iteration == max_iteration deve ancora forzare (<=).
    assert should_force_tool_choice(**_base_args(iteration=2, max_iteration=2)) is True


def test_no_force_when_flag_off():
    assert should_force_tool_choice(**_base_args(enabled=False)) is False


def test_no_force_when_no_tools():
    assert should_force_tool_choice(**_base_args(tools_available=False)) is False


def test_no_force_when_not_action_oriented():
    assert should_force_tool_choice(**_base_args(action_oriented=False)) is False


def test_no_force_when_iteration_above_threshold():
    assert should_force_tool_choice(**_base_args(iteration=3, max_iteration=2)) is False


def test_no_force_in_discovery_phase():
    assert should_force_tool_choice(**_base_args(in_discovery_phase=True)) is False


def test_no_force_when_provider_unsupported():
    assert should_force_tool_choice(**_base_args(provider_supports_forcing=False)) is False


# ── provider_style_supports_forcing ────────────────────────────────────────

def test_supported_styles():
    assert provider_style_supports_forcing("anthropic_any") is True
    assert provider_style_supports_forcing("openai_required") is True
    assert provider_style_supports_forcing("google_function_calling_any") is True


def test_unsupported_styles():
    assert provider_style_supports_forcing("openai_auto") is False
    assert provider_style_supports_forcing("none") is False
    assert provider_style_supports_forcing(None) is False
    assert provider_style_supports_forcing("") is False


# ── resolve_tool_choice override ────────────────────────────────────────────

def _cap(**overrides) -> ProviderCapability:
    base = dict(
        provider="openai",
        model="gpt-4o",
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


# Turno NON-primo: c'e' gia' un tool_result concreto nella history.
SECOND_TURN = [
    {"role": "user", "content": "Crea i file"},
    {"role": "assistant", "content": [
        {"type": "tool_use", "id": "t1", "name": "write_file", "input": {}}
    ]},
    {"role": "tool", "name": "write_file", "content": "ok"},
]


def test_override_true_forces_required_even_on_later_turn():
    # Senza override, al turno non-primo sarebbe "auto". Con override True -> required.
    cap = _cap()
    assert resolve_tool_choice(cap, SECOND_TURN) == "auto"
    assert resolve_tool_choice(cap, SECOND_TURN, force_override=True) == "required"


def test_override_false_disables_forcing_on_first_turn():
    cap = _cap()
    first = [{"role": "user", "content": "Crea il file"}]
    assert resolve_tool_choice(cap, first) == "required"
    assert resolve_tool_choice(cap, first, force_override=False) == "auto"


def test_override_true_respects_thinking_guard():
    # Anche con override True, un modello thinking non viene forzato (HTTP 400).
    cap = _cap(thinking=True)
    assert resolve_tool_choice(cap, SECOND_TURN, force_override=True) == "auto"


def test_override_true_anthropic_any():
    cap = _cap(provider="anthropic", model="claude", tool_choice_style="anthropic_any")
    assert resolve_tool_choice(cap, SECOND_TURN, force_override=True) == {"type": "any"}


def run_all():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
    print("test_tool_choice_forcing: OK (%d test)" % len(fns))


if __name__ == "__main__":
    run_all()
