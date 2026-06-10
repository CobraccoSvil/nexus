"""Regressione falso-doppione azione ripetuta (2026-06-10).

Incidente: due run col lavoro COMPLETO chiusi failed_diagnosed ("Mi sono
bloccato ripetendo la stessa azione (edit_file) 2 volte") perche'
_detect_repeated_action contava le ripetizioni IGNORANDO l'esito: il primo
edit_file era RIUSCITO (modifica applicata) e il secondo, identico, falliva
con "old_string non trovato" proprio perche' gia' applicato. Non e' stallo:
e' ridondanza su lavoro fatto.

Fix: le signature con almeno un esito di successo sono escluse dal conteggio
(helpers._tool_result_outcome_after, punto unico per "il tool_use e' riuscito?",
gestisce sia ToolMessage sia HumanMessage/anthropic_content di tool_dispatch).
"""
from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes.helpers import (
    _detect_repeated_action,
    _tool_result_outcome_after,
)


def _ai_edit(path: str, tuid: str) -> AIMessage:
    return AIMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {"type": "tool_use", "id": tuid, "name": "edit_file",
                 "input": {"path": path, "old_string": "a", "new_string": "b"}}
            ]
        },
    )


def _result(tuid: str, text: str, is_error: bool = False) -> HumanMessage:
    return HumanMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {"type": "tool_result", "tool_use_id": tuid,
                 "is_error": is_error, "content": [{"type": "text", "text": text}]}
            ]
        },
    )


def test_falso_doppione_success_poi_gia_applicato_non_e_stallo():
    # Incidente reale: edit OK -> stesso edit fallisce perche' gia' applicato.
    msgs = [
        _ai_edit("backend/src/app.ts", "t1"),
        _result("t1", "Modifica applicata con successo."),
        _ai_edit("backend/src/app.ts", "t2"),
        _result("t2", "Errore: old_string non trovato nel file", is_error=True),
    ]
    label, count = _detect_repeated_action(msgs)
    assert count == 0 and label is None, f"non deve essere stallo: {label} x{count}"


def test_stallo_vero_due_fallimenti_rilevato():
    msgs = [
        _ai_edit("x.ts", "t1"),
        _result("t1", "Errore: old_string non trovato", is_error=True),
        _ai_edit("x.ts", "t2"),
        _result("t2", "Errore: old_string non trovato", is_error=True),
    ]
    label, count = _detect_repeated_action(msgs)
    assert count == 2 and label and "x.ts" in label


def test_pending_senza_risultato_conta():
    # Nessun tool_result (pending): comportamento conservativo, conta.
    msgs = [_ai_edit("y.ts", "t1"), _ai_edit("y.ts", "t2")]
    _, count = _detect_repeated_action(msgs)
    assert count == 2


def test_outcome_helper_human_message():
    msgs = [
        _ai_edit("z.ts", "t1"),
        _result("t1", "OK fatto."),
    ]
    assert _tool_result_outcome_after(msgs, 0) is False
    msgs_err = [
        _ai_edit("z.ts", "t1"),
        _result("t1", "Errore: qualcosa", is_error=True),
    ]
    assert _tool_result_outcome_after(msgs_err, 0) is True
    assert _tool_result_outcome_after([_ai_edit("z.ts", "t1")], 0) is None


def test_azioni_diverse_non_interferiscono():
    # Un successo su file A non maschera lo stallo su file B.
    msgs = [
        _ai_edit("a.ts", "t1"),
        _result("t1", "Applicato."),
        _ai_edit("b.ts", "t2"),
        _result("t2", "Errore: old_string non trovato", is_error=True),
        _ai_edit("b.ts", "t3"),
        _result("t3", "Errore: old_string non trovato", is_error=True),
    ]
    label, count = _detect_repeated_action(msgs)
    assert count == 2 and label and "b.ts" in label
