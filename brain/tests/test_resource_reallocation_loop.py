"""Test del detector strutturale del loop di riallocazione porte (task #22).

Diagnosi confermata: l'agente chiama request_port piu' volte VARIANDO il label
mentre i servizi del progetto sono gia' attivi. Il loop sfugge a signature-loop
(label diverso = signature diversa) e a repeated_action (request_port non e' tra i
tool tracciati da _REPEATED_ACTION_TOOLS). Il segnale e' STRUTTURALE: il numero di
tool_use request_port ravvicinate, A PRESCINDERE dal label (e' proprio il variare
del label il sintomo). Helper puro, testabile senza DB ne' LLM.

Vedi anche test_progress_controller.py per la gerarchia decide(asse).
"""
from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes.helpers import (
    _count_recent_request_port,
    _has_active_resources_in_history,
)


def _ai_request_port(label: str, tuid: str) -> AIMessage:
    """AIMessage che chiama request_port con un certo label (lo scopo del servizio)."""
    return AIMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {"type": "tool_use", "id": tuid, "name": "request_port",
                 "input": {"label": label}}
            ]
        },
    )


def _ai_tool(name: str, tuid: str, inp: dict | None = None) -> AIMessage:
    return AIMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {"type": "tool_use", "id": tuid, "name": name, "input": inp or {}}
            ]
        },
    )


def _human(text: str) -> HumanMessage:
    return HumanMessage(content=text)


def test_conta_request_port_a_prescindere_dal_label():
    """Tre request_port con label DIVERSI contano 3: il variare del label e' il
    sintomo del loop, non un fattore di reset."""
    msgs = [
        _ai_request_port("backend", "t1"),
        _human("ho avviato il backend"),
        _ai_request_port("backend-dev", "t2"),
        _human("ancora"),
        _ai_request_port("backend-api", "t3"),
    ]
    assert _count_recent_request_port(msgs) == 3


def test_singolo_request_port_legittimo_non_e_loop():
    """Una sola allocazione per un servizio nuovo -> conteggio 1 (sotto soglia
    default 3): l'asse non deve scattare su una richiesta legittima."""
    msgs = [
        _human("avvia il backend"),
        _ai_request_port("backend", "t1"),
        _human("ok"),
    ]
    assert _count_recent_request_port(msgs) == 1


def test_altri_tool_non_contano_come_request_port():
    """Solo request_port conta: write_file/run_command non gonfiano il contatore."""
    msgs = [
        _ai_tool("write_file", "t1", {"path": "a.ts"}),
        _ai_tool("run_command", "t2", {"command": "npm run dev"}),
        _ai_request_port("frontend", "t3"),
    ]
    assert _count_recent_request_port(msgs) == 1


def test_lookback_limita_la_finestra():
    """Solo le request_port nella finestra recente contano (loop RAVVICINATO)."""
    vecchie = [_ai_request_port("svc-%d" % i, "old%d" % i) for i in range(5)]
    recenti = [_ai_request_port("svc-new", "new1")]
    msgs = vecchie + [_human("riempitivo")] * 20 + recenti
    # Con lookback piccolo vediamo solo l'ultima.
    assert _count_recent_request_port(msgs, lookback=4) == 1
    # Con lookback ampio le vediamo tutte.
    assert _count_recent_request_port(msgs, lookback=100) == 6


def test_messaggi_vuoti_zero():
    assert _count_recent_request_port([]) == 0
    assert _count_recent_request_port([_human("ciao")]) == 0


def test_has_active_resources_su_request_port():
    """Se l'agente ha gia' richiesto una porta, il run ha risorse note -> grounding
    verso il riuso nei nudge."""
    assert _has_active_resources_in_history([_ai_request_port("backend", "t1")]) is True


def test_has_active_resources_su_service_tools():
    """list_active_services / service_restart segnalano operativita' su servizi
    esistenti -> risorse attive note."""
    assert _has_active_resources_in_history(
        [_ai_tool("list_active_services", "t1")]
    ) is True
    assert _has_active_resources_in_history(
        [_ai_tool("service_restart", "t2", {"name": "backend"})]
    ) is True


def test_has_active_resources_falso_senza_risorse():
    """Solo letture/scritture, nessuna risorsa-porta/servizio -> False (i nudge
    esplorativi possono ancora proporre request_port per un servizio nuovo)."""
    msgs = [
        _ai_tool("read_file", "t1", {"path": "x.ts"}),
        _ai_tool("write_file", "t2", {"path": "y.ts"}),
    ]
    assert _has_active_resources_in_history(msgs) is False
