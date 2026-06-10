"""WAVE 3 — esito DICHIARATO via task_complete (de-lessicalizzazione).

Il modello dichiara l'esito con un tool strutturato invece di farlo inferire
dal testo (~150 frasi it/en di _detect_unfulfilled_intent). Questi test fissano:
- la normalizzazione/validazione dell'input task_complete;
- lo schema del tool (additivo, brain-only).
"""
from brain.agents.nodes.helpers import (
    TASK_COMPLETE_TOOL,
    TASK_COMPLETE_TOOL_NAME,
    normalize_declared_outcome,
)


def test_schema_tool_valido():
    assert TASK_COMPLETE_TOOL["name"] == TASK_COMPLETE_TOOL_NAME == "task_complete"
    props = TASK_COMPLETE_TOOL["input_schema"]["properties"]
    assert set(["outcome", "summary", "next_step", "blocked_by"]) <= set(props)
    assert TASK_COMPLETE_TOOL["input_schema"]["required"] == ["outcome", "summary"]
    assert props["outcome"]["enum"] == ["done", "blocked", "needs_input"]


def test_normalize_done():
    d = normalize_declared_outcome({"outcome": "done", "summary": "Fatto."})
    assert d == {"outcome": "done", "summary": "Fatto."}


def test_normalize_blocked_con_campi_extra():
    d = normalize_declared_outcome({
        "outcome": "BLOCKED", "summary": "Manca la chiave API",
        "blocked_by": "OPENAI_API_KEY assente", "next_step": "",
    })
    assert d["outcome"] == "blocked"  # case-insensitive
    assert d["blocked_by"] == "OPENAI_API_KEY assente"
    assert "next_step" not in d  # vuoto -> escluso


def test_normalize_outcome_invalido_ritorna_none():
    # Fuori enum -> None: il chiamante ricade sui segnali strutturali/lessicali.
    assert normalize_declared_outcome({"outcome": "finito", "summary": "x"}) is None
    assert normalize_declared_outcome({"summary": "manca outcome"}) is None
    assert normalize_declared_outcome("non un dict") is None
    assert normalize_declared_outcome(None) is None


def test_normalize_needs_input():
    d = normalize_declared_outcome({
        "outcome": "needs_input", "summary": "Quale DB usare?",
        "next_step": "Conferma il nome del database",
    })
    assert d["outcome"] == "needs_input"
    assert d["next_step"] == "Conferma il nome del database"
