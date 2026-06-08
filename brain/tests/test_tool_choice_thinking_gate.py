"""Test del gate thinking <-> tool_choice (punto unico, regola L).

Verifica la correzione del bug sistemico "pianifica e non agisce": i modelli
dual-mode (claude-4.x, gemini-2.5/3.x, gpt-5.x, deepseek-v4-pro) hanno
uses_thinking_mode=TRUE ma policy 'disable_for_tools'. L'adapter spegne il
thinking quando ci sono tool, quindi il tool_choice forzato e' accettato e va
usato. Prima il gate guardava `cap.thinking` statico e annullava la forzatura
anti-narration del primo turno per tutti questi modelli.

I test sono puri (nessun DB, nessuna rete): costruiscono ProviderCapability
sintetiche e verificano le property + resolve_tool_choice per ogni dialetto.
"""
from __future__ import annotations

from brain.providers._models import ProviderCapability
from brain.providers.adapter_base import resolve_tool_choice


def _cap(**overrides) -> ProviderCapability:
    """Capability minima; si sovrascrivono solo i campi rilevanti per il test."""
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


# History "primo turno": solo messaggio utente, nessun tool_result.
_FIRST_TURN = [{"role": "user", "content": "realizza l'app dal figma"}]
# History "turno successivo": un tool concreto e' gia' stato eseguito.
_LATER_TURN = [
    {"role": "user", "content": "x"},
    {"role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "write_file"}}]},
    {"role": "tool", "tool_call_id": "c1", "content": "ok"},
]


# --- property thinking_disabled_for_tools ---

def test_disabled_for_tools_true_su_policy_disable():
    assert _cap(agentic_thinking_policy="disable_for_tools").thinking_disabled_for_tools is True


def test_disabled_for_tools_false_su_native_none():
    assert _cap(agentic_thinking_policy="native").thinking_disabled_for_tools is False
    assert _cap(agentic_thinking_policy="none").thinking_disabled_for_tools is False


# --- property thinking_active_with_tools ---

def test_active_false_quando_dual_mode_disabilita():
    # claude-4.x / gemini-2.5 / gpt-5 / deepseek-v4-pro: thinking-capable ma spento per i tool.
    cap = _cap(thinking=True, agentic_thinking_policy="disable_for_tools")
    assert cap.thinking_active_with_tools is False


def test_active_true_quando_native():
    # native (thinking resta acceso anche con i tool) -> tool_choice NON forzabile.
    cap = _cap(thinking=True, agentic_thinking_policy="native")
    assert cap.thinking_active_with_tools is True


def test_active_false_quando_non_thinking():
    assert _cap(thinking=False, agentic_thinking_policy="none").thinking_active_with_tools is False


# --- resolve_tool_choice: il fix vero e proprio ---

def test_dual_mode_disable_forza_required_al_primo_turno():
    # REGRESSIONE: prima ritornava "auto" perche' guardava cap.thinking statico.
    cap = _cap(thinking=True, agentic_thinking_policy="disable_for_tools",
               tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, _FIRST_TURN) == "required"


def test_dual_mode_disable_forza_any_anthropic():
    cap = _cap(thinking=True, agentic_thinking_policy="disable_for_tools",
               tool_choice_style="anthropic_any")
    assert resolve_tool_choice(cap, _FIRST_TURN) == {"type": "any"}


def test_dual_mode_disable_forza_any_google():
    cap = _cap(thinking=True, agentic_thinking_policy="disable_for_tools",
               tool_choice_style="google_function_calling_any")
    assert resolve_tool_choice(cap, _FIRST_TURN) == {"function_calling_config": {"mode": "ANY"}}


def test_native_thinking_resta_auto():
    # Un modello davvero in thinking mode (native) NON va forzato: l'API rifiuterebbe.
    cap = _cap(thinking=True, agentic_thinking_policy="native",
               tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, _FIRST_TURN) == "auto"


def test_non_thinking_forza_required():
    cap = _cap(thinking=False, agentic_thinking_policy="none",
               tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, _FIRST_TURN) == "required"


def test_turno_successivo_degrada_auto():
    # Oltre il primo turno non si forza (force_override=None usa first_turn_force).
    cap = _cap(thinking=True, agentic_thinking_policy="disable_for_tools",
               tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, _LATER_TURN) == "auto"


def test_force_override_false_degrada_auto():
    cap = _cap(thinking=False, tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, _FIRST_TURN, force_override=False) == "auto"


def test_weak_model_degrada_auto():
    cap = _cap(thinking=False, model="mistral-small-latest",
               tool_choice_style="openai_required")
    assert resolve_tool_choice(cap, _FIRST_TURN, weak_models=("small",)) == "auto"


def test_style_none_ritorna_none():
    # o1/o3: reasoning puro, nessun tool_choice.
    cap = _cap(thinking=False, agentic_thinking_policy="native", tool_choice_style="none")
    assert resolve_tool_choice(cap, _FIRST_TURN) is None


if __name__ == "__main__":
    import sys
    import traceback

    funcs = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in funcs:
        try:
            fn()
            print(f"PASS {fn.__name__}")
        except Exception:
            failed += 1
            print(f"FAIL {fn.__name__}")
            traceback.print_exc()
    print(f"\n{len(funcs) - failed}/{len(funcs)} test passati")
    sys.exit(1 if failed else 0)
