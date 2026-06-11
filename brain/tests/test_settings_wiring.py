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


def _fake_db_pool_connect(rows: list[tuple[str, str]]):
    """Sostituto di brain.utils.db_pool.connect (punto unico DB, Wave 5):
    context manager che presta una connessione finta con le righe date."""
    cur = mock.MagicMock()
    cur.fetchall.return_value = rows
    cur.__enter__ = lambda self: cur
    cur.__exit__ = lambda self, *a: False
    conn = mock.MagicMock()
    conn.cursor.return_value = cur

    @contextlib.contextmanager
    def _connect(*args, **kwargs):
        yield conn

    return _connect


def test_confirm_if_implemented_mappata_dal_loader_clarify(monkeypatch) -> None:
    # Default presente anche senza DB.
    monkeypatch.delenv("DATABASE_URL", raising=False)
    cfg = cn._load_config()
    assert cfg["confirm_if_implemented"] is True

    # Con la riga DB a false, il loader la applica (era il ramo mancante).
    # Il loader passa dal pool condiviso: si mocka il punto unico db_pool.connect.
    monkeypatch.setenv("DATABASE_URL", "postgres://fake/fake")
    fake_connect = _fake_db_pool_connect([("clarify.confirm_if_implemented", "false")])
    with mock.patch("brain.utils.db_pool.connect", fake_connect):
        cfg = cn._load_config()
    assert cfg["confirm_if_implemented"] is False
