"""Test del fix integrazione del system_text:
- offload PRIORITIZZATO e budget-aware (session_worklog offloadabile come ultima
  risorsa, learned_instructions sempre inline);
- dedup RAG cross-fonte (KB non ripete item gia' nel RAG-chat).

Eseguibile a mano: PYTHONPATH=. python3 brain/tests/test_system_offload_dedup.py
Auto-contenuto: funzioni pure, nessun DB ne' rete.
"""
from __future__ import annotations

import sys

from brain.agents.nodes import helpers
from brain.agents.nodes import _dedup_kb_against_rag


# ── offload prioritizzato ───────────────────────────────────────────────────

def test_worklog_offloadabile_come_ultima_risorsa() -> None:
    tags = [t.strip() for t in helpers._SYS_OFFLOADABLE_SECTIONS_DEFAULT.split(",")]
    assert "session_worklog" in tags, tags
    assert tags[-1] == "session_worklog", "worklog deve essere ultima risorsa (priorita' minima)"
    assert "learned_instructions" not in tags, "learned_instructions resta sempre inline"
    print("OK test_worklog_offloadabile_come_ultima_risorsa")


def test_recovery_tool_worklog() -> None:
    assert helpers._SYS_OFFLOAD_RECOVERY_TOOL.get("session_worklog") == "nexus_get_worklog"
    print("OK test_recovery_tool_worklog")


def test_extract_estrae_worklog_lasciando_direttive() -> None:
    st = "DIRETTIVE OPERATIVE\n<session_worklog>\nstoria di lavoro\n</session_worklog>\nANTI_LOOP"
    remaining, sections = helpers._extract_offloadable_sections(st, ["session_worklog"])
    assert len(sections) == 1 and sections[0][0] == "session_worklog"
    assert "<session_worklog>" not in remaining
    assert "DIRETTIVE OPERATIVE" in remaining and "ANTI_LOOP" in remaining
    print("OK test_extract_estrae_worklog_lasciando_direttive")


# ── dedup RAG cross-fonte ───────────────────────────────────────────────────

def test_dedup_rimuove_item_kb_gia_nel_rag() -> None:
    rag = '<contesto_pertinente><interazione score="0.9">come fixo il bug X</interazione></contesto_pertinente>'
    kb = (
        '<knowledge_base_progetto><nota intent="fix"><titolo>T</titolo>'
        '<contenuto>Come   FIXO il bug X</contenuto></nota></knowledge_base_progetto>'
    )
    out = _dedup_kb_against_rag(rag, kb)
    assert "<nota" not in out, out  # match per testo normalizzato -> nota rimossa
    print("OK test_dedup_rimuove_item_kb_gia_nel_rag")


def test_dedup_noop_su_index_mode() -> None:
    # KB in mode 'index': <nota .../> self-closing, nessun <contenuto> -> no-op.
    rag = "<contesto_pertinente><interazione>X</interazione></contesto_pertinente>"
    kb = '<knowledge_base_progetto><nota intent="fix" titolo="T" note_id="1" score="0.8"/></knowledge_base_progetto>'
    assert _dedup_kb_against_rag(rag, kb) == kb
    print("OK test_dedup_noop_su_index_mode")


def test_dedup_conserva_item_unici() -> None:
    rag = "<contesto_pertinente><interazione>argomento A</interazione></contesto_pertinente>"
    kb = "<knowledge_base_progetto><nota><contenuto>argomento totalmente diverso B</contenuto></nota></knowledge_base_progetto>"
    out = _dedup_kb_against_rag(rag, kb)
    assert "argomento totalmente diverso B" in out
    print("OK test_dedup_conserva_item_unici")


def test_dedup_fail_open_su_rag_vuoto() -> None:
    kb = "<knowledge_base_progetto><nota><contenuto>X</contenuto></nota></knowledge_base_progetto>"
    assert _dedup_kb_against_rag("", kb) == kb
    print("OK test_dedup_fail_open_su_rag_vuoto")


if __name__ == "__main__":
    test_worklog_offloadabile_come_ultima_risorsa()
    test_recovery_tool_worklog()
    test_extract_estrae_worklog_lasciando_direttive()
    test_dedup_rimuove_item_kb_gia_nel_rag()
    test_dedup_noop_su_index_mode()
    test_dedup_conserva_item_unici()
    test_dedup_fail_open_su_rag_vuoto()
    print("Tutti i test system_offload + dedup OK")
    sys.exit(0)
