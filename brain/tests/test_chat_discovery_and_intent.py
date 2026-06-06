"""Regressione: domande sul progetto devono ispezionare i file reali, non
ricevere risposte generiche su progetti ipotetici.

Copre due fix collegati (audit chat progetto Marco, 06/06/2026):

A) `filter_chat_discovery_tools` — per gli intent conversazionali NON si azzerano
   piu' del tutto i tool: restano i meta-tool di gestione/discovery Nexus, cosi'
   il modello puo' scoprire e usare gli strumenti di lettura se la domanda
   riguarda il progetto. Restano esclusi i tool con side-effect.

B) `_classify_by_keywords` — le domande conoscitive sull'esistenza/scopo di
   entita' del progetto vanno su `code_read`; le osservazioni di malfunzionamento
   su `fix`. Prima cadevano sul default `chat` -> modello lite senza contesto.
"""
from __future__ import annotations

from brain.agents.nodes import _CHAT_DISCOVERY_KEEP, filter_chat_discovery_tools
from brain.router.service import SemanticRouter


# ── A) toolkit chat: meta-tool mantenuti, side-effect esclusi ────────────────

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


# ── B) classificazione domande/osservazioni sul progetto ─────────────────────

def test_domanda_due_index_html_e_code_read() -> None:
    # Caso reale: senza accento su "perche", come digitato dall'utente.
    r = SemanticRouter()
    out = r._classify_by_keywords("perche ci sono due file index.html?")
    assert out["intent"] == "code_read", out


def test_form_mal_disposte_e_fix() -> None:
    r = SemanticRouter()
    out = r._classify_by_keywords(
        "le form del sito sono mal disposte, i campi sono piccoli"
    )
    assert out["intent"] == "fix", out


def test_menu_non_funziona_e_fix() -> None:
    r = SemanticRouter()
    out = r._classify_by_keywords(
        "il menu della home non funziona, i link sono sbagliati"
    )
    assert out["intent"] == "fix", out


def test_smalltalk_resta_chat() -> None:
    # Lo small-talk vero non deve essere promosso a intent operativo.
    r = SemanticRouter()
    out = r._classify_by_keywords("ciao, come stai oggi?")
    assert out["intent"] == "chat", out


def test_regressione_casi_esistenti_intatti() -> None:
    # I casi gia' coperti da altri test non devono cambiare classificazione.
    r = SemanticRouter()
    assert r._classify_by_keywords("cancella il file variables.txt")["intent"] == "file_ops"
    assert r._classify_by_keywords("quante variabili ci sono")["intent"] == "code_read"
    assert r._classify_by_keywords("genera l'analisi tecnica")["intent"] == "docs"
    assert r._classify_by_keywords("Esegui docker compose down per fermare i container")["intent"] == "system_admin"
