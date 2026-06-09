"""Test del punto unico progress_controller.decide (funzione pura).

Verificano la gerarchia coordinata guida -> escalate -> abort-verso-verifica per
ogni asse di stallo, l'assenza di abort prematuro (il bug dominante: l'esplorazione
abortiva senza prima forzare l'azione) e i default neutri.
"""
from brain.agents.progress_controller import (
    ABORT_STOP_REASON,
    ProgressDecision,
    ProgressSignals,
    decide,
)


def test_no_stallo_proceed():
    """Sotto soglia su ogni asse -> proceed, niente nudge ne' abort."""
    d = decide(ProgressSignals(exploration_count=3, exploration_threshold=6))
    assert d.action == "proceed"
    assert d.force_action is False
    assert d.nudge_text is None
    assert d.stop_reason is None


def test_esplorazione_2x_prima_volta_guida_non_aborta():
    """Caso dominante: a 2x soglia, se mai guidata, NON aborta -> forza-azione.

    E' il fix di fondo: prima si costringe ad agire (rimuovi read-only +
    tool_choice required), non si chiude il run.
    """
    d = decide(ProgressSignals(exploration_count=12, exploration_threshold=6))
    assert d.action == "guide"
    assert d.axis == "exploration"
    assert d.force_action is True
    assert d.nudge_text and "STOP esplorazione" in d.nudge_text
    assert d.stop_reason is None  # NON aborta


def test_esplorazione_2x_gia_guidata_senza_escalation_aborta_verso_verifica():
    """Gia' forzata e ancora bloccata, nessun candidato escalation -> ABORT, ma
    con lo stop_reason coordinato che instrada alla verifica E2E."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            already_guided=frozenset({"exploration"}),
            has_escalation_candidate=False,
        )
    )
    assert d.action == "abort"
    assert d.stop_reason == ABORT_STOP_REASON
    assert d.force_action is False


def test_esplorazione_2x_gia_guidata_con_candidato_escala_prima_di_abortire():
    """Gia' forzata, c'e' un candidato e budget disponibile -> ESCALATE, non abort."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            already_guided=frozenset({"exploration"}),
            has_escalation_candidate=True,
            escalations=0,
            max_escalations=3,
        )
    )
    assert d.action == "escalate"
    assert d.axis == "exploration"
    assert d.stop_reason is None


def test_escalation_budget_esaurito_aborta():
    """Budget escalation esaurito -> abort anche se ci sarebbe un candidato."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            already_guided=frozenset({"exploration"}),
            has_escalation_candidate=True,
            escalations=3,
            max_escalations=3,
        )
    )
    assert d.action == "abort"
    assert d.stop_reason == ABORT_STOP_REASON


def test_signature_loop_prima_volta_guida():
    """Loop su tool identico, mai guidato -> forza-azione con nudge mirato."""
    d = decide(ProgressSignals(signature_loop_tool="read_file"))
    assert d.action == "guide"
    assert d.axis == "signature"
    assert d.force_action is True
    assert d.nudge_text and "read_file" in d.nudge_text


def test_g1_descriptive_prima_volta_guida():
    """Risposta descrittiva su richiesta d'azione, mai guidata -> forza-azione."""
    d = decide(ProgressSignals(g1_over_cap=True))
    assert d.action == "guide"
    assert d.axis == "g1_descriptive"
    assert d.force_action is True


def test_priorita_esplorazione_su_signature():
    """Se due assi sono in stallo, l'esplorazione ha priorita' (ordine del report)."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            signature_loop_tool="grep",
        )
    )
    assert d.axis == "exploration"


def test_soglia_zero_non_divide_per_zero():
    """exploration_threshold=0 non causa errori (clamp a 1 nel confronto)."""
    d = decide(ProgressSignals(exploration_count=2, exploration_threshold=0))
    # 2 >= 2*max(1,0)=2 -> stallo esplorazione, guida.
    assert isinstance(d, ProgressDecision)
    assert d.action == "guide"
