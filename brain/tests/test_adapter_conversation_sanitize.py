"""Test della normalizzazione conversazione per provider OpenAI-compat.

Copre `_strip_trailing_assistant`: i provider OpenAI-compat strict (Mistral)
rifiutano una conversazione che termina con role 'assistant' (HTTP 422
"Expected last role User or Tool ... but got assistant"). La normalizzazione
rimuove gli assistant in coda perche' generate_agent_turn deve GENERARE il
prossimo turno assistant: l'input non deve mai terminare con assistant.

Regressione catturata: cascade/fallback mid-turn dopo soft-failure, dove la
history reinviata al provider di fallback finisce con il turno "molle" del
provider che ha mollato.
"""
from brain.providers.adapter_base import _strip_trailing_assistant


def test_rimuove_assistant_finale_singolo():
    # Caso soft-failure cascade: l'ultimo turno e' un assistant "molle".
    msgs = [
        {"role": "user", "content": "fai il task"},
        {"role": "assistant", "content": "non sono sicuro di come procedere"},
    ]
    out = _strip_trailing_assistant(msgs)
    assert out == [{"role": "user", "content": "fai il task"}]
    assert out[-1]["role"] == "user"


def test_rimuove_assistant_finali_multipli():
    msgs = [
        {"role": "user", "content": "task"},
        {"role": "tool", "tool_call_id": "t1", "content": "risultato"},
        {"role": "assistant", "content": "penso..."},
        {"role": "assistant", "content": "ancora penso..."},
    ]
    out = _strip_trailing_assistant(msgs)
    assert out[-1]["role"] == "tool"
    assert len(out) == 2


def test_noop_se_finisce_con_user():
    msgs = [
        {"role": "assistant", "content": "ciao"},
        {"role": "user", "content": "domanda"},
    ]
    out = _strip_trailing_assistant(msgs)
    assert out == msgs  # invariato: ultimo gia' user


def test_noop_se_finisce_con_tool():
    msgs = [
        {"role": "assistant", "content": "", "tool_calls": [{"id": "t1"}]},
        {"role": "tool", "tool_call_id": "t1", "content": "ok"},
    ]
    out = _strip_trailing_assistant(msgs)
    assert out == msgs  # invariato: ultimo gia' tool


def test_assistant_con_tool_calls_finale_rimosso():
    # Turno interrotto: assistant con tool_calls ma senza i tool result -> e'
    # l'ultimo, va rimosso (il fallback rigenera). Resta il tool precedente.
    msgs = [
        {"role": "user", "content": "task"},
        {"role": "assistant", "content": "", "tool_calls": [{"id": "a"}]},
        {"role": "tool", "tool_call_id": "a", "content": "res"},
        {"role": "assistant", "content": "", "tool_calls": [{"id": "b"}]},
    ]
    out = _strip_trailing_assistant(msgs)
    assert out[-1]["role"] == "tool"
    assert out[-1]["tool_call_id"] == "a"
    assert len(out) == 3


def test_guard_solo_assistant_ripristina_originale():
    # Caso patologico: solo assistant. Non svuotiamo: ripristina l'originale e
    # lascia che sia il provider a dare l'errore esplicito.
    msgs = [
        {"role": "assistant", "content": "a"},
        {"role": "assistant", "content": "b"},
    ]
    out = _strip_trailing_assistant(msgs)
    assert out == msgs


def test_lista_vuota_invariata():
    assert _strip_trailing_assistant([]) == []
