"""Test di integrazione: route_after_executor instrada gli abort verso la
verifica E2E (final_gate) invece di chiudere "morto" al learner.

Cattura la regressione strutturale: prima gli abort (loop_detected /
g1_cap_reached) andavano dritti al learner SCAVALCANDO il final_gate; un task
incompleto chiudeva senza alcuna verifica del flusso reale. Ora tutti gli abort
coordinati (loop_abort) e legacy passano per la verifica quando eleggibile.
"""
import pytest

from brain.agents import orchestrator_config
from brain.agents.nodes import routing


@pytest.fixture(autouse=True)
def _cfg(monkeypatch):
    """cfg sintetico: final_gate attivo, alcuni intent software."""
    monkeypatch.setattr(
        orchestrator_config,
        "get",
        lambda: {
            "final_gate_enabled": True,
            "final_gate_max_cycles": 3,
            "final_gate_software_intents": ["fix", "build_feature", "debug_issue"],
            "verifier_enabled": True,
        },
    )


def _state(**kw):
    base = {"iterations": 5, "iteration_budget": 60, "pending_tool_uses": []}
    base.update(kw)
    return base


@pytest.mark.parametrize("stop", ["loop_abort", "loop_detected", "g1_cap_reached"])
def test_abort_software_va_al_final_gate(stop):
    """Ogni abort (coordinato o legacy) su task software -> verifica E2E."""
    assert routing.route_after_executor(_state(stop_reason=stop, user_intent="fix")) == "final_gate"


def test_abort_non_software_va_al_learner():
    """Su task non software non c'e' gate da eseguire -> chiusura diretta."""
    assert routing.route_after_executor(
        _state(stop_reason="loop_abort", user_intent="general_chat")
    ) == "learner"


def test_abort_con_cap_gate_raggiunto_va_al_learner():
    """Cap final_gate raggiunto -> learner (niente loop infinito di verifica)."""
    assert routing.route_after_executor(
        _state(stop_reason="loop_abort", user_intent="fix", final_gate_cycle=3)
    ) == "learner"


def test_superseded_sempre_learner():
    """Run superato (last-wins): chiusura cooperativa immediata, mai al gate."""
    assert routing.route_after_executor(
        _state(stop_reason="superseded", user_intent="fix")
    ) == "learner"


def test_g1_escalated_passa_per_g1_continue():
    """g1_escalated re-instrada l'executor PER il nodo passthrough g1_continue:
    il vecchio self-loop executor->executor non veniva materializzato dal
    checkpointer custom (fix self-loop 2026-06-15). La re-execution del modello
    escalato avviene comunque (g1_continue -> executor)."""
    assert routing.route_after_executor(
        _state(stop_reason="g1_escalated", user_intent="fix")
    ) == "g1_continue"


def test_abort_con_plan_attivo_non_va_al_final_gate():
    """Con plan attivo il final_gate generale non gira (il verifier ha il suo flusso)."""
    assert routing.route_after_executor(
        _state(stop_reason="loop_abort", user_intent="fix", plan_phase_active=True)
    ) == "learner"
