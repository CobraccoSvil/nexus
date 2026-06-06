"""Regressione: per gli intent conversazionali NON si azzerano del tutto i tool.

`filter_chat_discovery_tools` mantiene i meta-tool di gestione/discovery Nexus
(nexus_mcp_tool_search/call + lettura), cosi' il modello puo' scoprire e usare
gli strumenti di lettura se la domanda riguarda il progetto, restando rapido
sullo small-talk. Restano esclusi i tool con side-effect.

NOTA: la parte sulla classificazione keyword (_classify_by_keywords) e' stata
RIMOSSA: l'interpretazione dell'intent e' ora solo semantica (classifier LLM),
quindi non e' piu' testabile con asserzioni deterministiche su stringhe.
"""
from __future__ import annotations

from brain.agents.nodes import _CHAT_DISCOVERY_KEEP, filter_chat_discovery_tools


def test_chat_mantiene_meta_tool_non_azzera() -> None:
    tools = [
        {"name": "nexus_mcp_tool_search"},
        {"name": "nexus_mcp_tool_call"},
        {"name": "nexus_open_file_in_editor"},
        {"name": "recall_context"},
        {"name": "write_file"},
        {"name": "edit_file"},
        {"name": "delete_file"},
        {"name": "run_command"},
        {"name": "read_file"},
    ]
    kept = {t["name"] for t in filter_chat_discovery_tools(tools)}
    # I meta-tool di gestione restano: il modello puo' scoprire e ispezionare.
    assert "nexus_mcp_tool_search" in kept
    assert "nexus_mcp_tool_call" in kept
    # Niente piu' azzeramento totale.
    assert len(kept) >= 2
    # Nessun tool con side-effect su una chat.
    assert "write_file" not in kept
    assert "edit_file" not in kept
    assert "delete_file" not in kept
    assert "run_command" not in kept


def test_chat_discovery_input_vuoto() -> None:
    assert filter_chat_discovery_tools([]) == []
    assert filter_chat_discovery_tools(None) == []  # type: ignore[arg-type]


def test_chat_discovery_keep_e_solo_read_e_meta() -> None:
    # La whitelist non deve mai includere tool di scrittura/esecuzione.
    assert "write_file" not in _CHAT_DISCOVERY_KEEP
    assert "run_command" not in _CHAT_DISCOVERY_KEEP
    assert "nexus_mcp_tool_search" in _CHAT_DISCOVERY_KEEP
