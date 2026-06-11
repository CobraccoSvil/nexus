"""Regressione per i bug di wiring settings scoperti dall'audit 2026-06-11.

Due chiavi amministrabili erano lette dai consumatori (cfg.get) ma MAI
caricate dal DB dai rispettivi loader: il valore in tabella era inerte.

  1. orchestrator.clarifying_questions_{enabled,max}: usate da planner_node
     ma assenti da orchestrator_config._KEYS (la query usa WHERE key IN).
  2. clarify.confirm_if_implemented (ex kb.intake.confirm_if_implemented,
     rinominata in mig 0407): il loader di clarify_or_expand_node legge solo
     key LIKE 'clarify.%' e non aveva il ramo di mapping.
"""
from __future__ import annotations

import contextlib
import sys
import types
from unittest import mock

from brain.agents import orchestrator_config as oc
from brain.agents import clarify_or_expand_node as cn


def test_clarifying_questions_sono_caricate_dal_db() -> None:
    # Il bug: chiavi lette dal planner ma fuori dalla query IN del loader.
    assert "clarifying_questions_enabled" in oc._KEYS
    assert "clarifying_questions_max" in oc._KEYS
    assert oc._SAFE_DEFAULTS["clarifying_questions_enabled"] is True
    assert oc._SAFE_DEFAULTS["clarifying_questions_max"] == 3
    # Nome completo nel DB: prefisso orchestrator. (nessun override).
    assert oc._full_key("clarifying_questions_max") == "orchestrator.clarifying_questions_max"


def _fake_psycopg2(rows: list[tuple[str, str]]) -> types.ModuleType:
    cur = mock.MagicMock()
    cur.fetchall.return_value = rows
    cur.__enter__ = lambda self: cur
    cur.__exit__ = lambda self, *a: False
    conn = mock.MagicMock()
    conn.cursor.return_value = cur
    conn.__enter__ = lambda self: conn
    conn.__exit__ = lambda self, *a: False
    fake = types.ModuleType("psycopg2")
    fake.connect = lambda url: conn  # type: ignore[attr-defined]
    return fake


def test_confirm_if_implemented_mappata_dal_loader_clarify(monkeypatch) -> None:
    # Default presente anche senza DB.
    monkeypatch.delenv("DATABASE_URL", raising=False)
    cfg = cn._load_config()
    assert cfg["confirm_if_implemented"] is True

    # Con la riga DB a false, il loader la applica (era il ramo mancante).
    monkeypatch.setenv("DATABASE_URL", "postgres://fake/fake")
    fake = _fake_psycopg2([("clarify.confirm_if_implemented", "false")])
    with mock.patch.dict(sys.modules, {"psycopg2": fake}):
        cfg = cn._load_config()
    assert cfg["confirm_if_implemented"] is False
