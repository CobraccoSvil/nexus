"""Test ADR 0018 (c): segnale STRUTTURALE del reroute G1.

structural_unfulfilled_signal scatta quando i tool erano disponibili, nessuna
tool call e' stata emessa nel turno, il task e' action-oriented e siamo entro
la soglia di iterazione — SENZA guardare i verbi del testo (caso BookingPage).
Non scatta se un tool e' stato usato, se il task non e' d'azione, o se
l'iterazione e' alta.

Esecuzione senza pytest:
    python3 -c "import brain.tests.test_g1_structural_signal as t; t.run_all()"
"""
from brain.agents.nodes.helpers import structural_unfulfilled_signal


def _args(**ov):
    args = dict(
        had_tools_available=True,
        no_tool_call_this_turn=True,
        action_oriented=True,
        iteration=0,
        max_iteration=2,
    )
    args.update(ov)
    return args


def test_fires_on_bookingpage_case():
    # Tool disponibili + 0 tool call + task d'azione + iter bassa -> scatta.
    assert structural_unfulfilled_signal(**_args()) is True


def test_fires_at_threshold_iteration():
    assert structural_unfulfilled_signal(**_args(iteration=2, max_iteration=2)) is True


def test_no_fire_when_tool_used_this_turn():
    # Il modello HA emesso una tool call: chiusura legittima, niente reroute.
    assert structural_unfulfilled_signal(**_args(no_tool_call_this_turn=False)) is False


def test_no_fire_when_no_tools_available():
    # Nessun tool disponibile nel turno: lo stop non e' "premature".
    assert structural_unfulfilled_signal(**_args(had_tools_available=False)) is False


def test_no_fire_when_not_action_oriented():
    # Task non d'azione (es. domanda informativa): chiusura testuale lecita.
    assert structural_unfulfilled_signal(**_args(action_oriented=False)) is False


def test_no_fire_when_iteration_above_threshold():
    # Iterazione alta: il modello ha avuto i suoi turni, niente forzatura.
    assert structural_unfulfilled_signal(**_args(iteration=5, max_iteration=2)) is False


def run_all():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
    print("test_g1_structural_signal: OK (%d test)" % len(fns))


if __name__ == "__main__":
    run_all()
