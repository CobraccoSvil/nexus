"""Test lettura/iniezione worklog di sessione (mig 0411) e learned
instructions (mig 0412).

Eseguibile a mano: `PYTHONPATH=. python3 brain/tests/test_session_worklog_inject.py`.
Auto-contenuto: nessun DB — db_connect e settings sono monkeypatchati.
"""
from __future__ import annotations

import sys

from brain.agents import session_worklog as SW


class _FakeCursor:
    """Cursor finto: `fetchone` ritorna `row`, `fetchall` ritorna `rows`.

    fetch_learned_instructions_block fa due execute (rules via fetchall, poi
    template via fetchone); fetch_worklog_block fa un solo fetchone.
    """

    def __init__(self, row=None, rows=None):
        self._row = row
        self._rows = rows or []

    def execute(self, *_args, **_kwargs):
        pass

    def fetchone(self):
        return self._row

    def fetchall(self):
        return self._rows

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


class _FakeConn:
    def __init__(self, row=None, rows=None):
        self._row = row
        self._rows = rows

    def cursor(self):
        return _FakeCursor(self._row, self._rows)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


def _patch(row=None, enabled="true", raise_db=False):
    SW.get_setting_cached = lambda key, default="": enabled
    if raise_db:
        def _boom():
            raise RuntimeError("db down")
        SW.db_connect = _boom
    else:
        SW.db_connect = lambda: _FakeConn(row=row)


def _patch_settings(values: dict):
    SW.get_setting_cached = lambda key, default="": values.get(key, default)


def _patch_learned(rows=None, tpl_row=None, values=None, raise_db=False):
    _patch_settings(values or {
        "orchestrator.learned_instructions_enabled": "true",
        "orchestrator.learned_instructions_max_chars": "1500",
    })
    if raise_db:
        def _boom():
            raise RuntimeError("db down")
        SW.db_connect = _boom
    else:
        SW.db_connect = lambda: _FakeConn(row=tpl_row, rows=rows)


# ── worklog (mig 0411) ──────────────────────────────────────────────────────

def test_blocco_wrappato() -> None:
    _patch(row=("Stato: run abc completed\nFile gia' creati: src/a.ts",))
    out = SW.fetch_worklog_block("11111111-1111-1111-1111-111111111111")
    assert out.startswith("<session_worklog>"), out
    assert out.endswith("</session_worklog>"), out
    assert "src/a.ts" in out
    print("OK test_blocco_wrappato")


def test_vuoto_se_nessuna_riga() -> None:
    _patch(row=None)
    assert SW.fetch_worklog_block("11111111-1111-1111-1111-111111111111") == ""
    print("OK test_vuoto_se_nessuna_riga")


def test_vuoto_se_rendered_block_blank() -> None:
    _patch(row=("   ",))
    assert SW.fetch_worklog_block("11111111-1111-1111-1111-111111111111") == ""
    print("OK test_vuoto_se_rendered_block_blank")


def test_disabilitato_da_setting() -> None:
    _patch(row=("contenuto",), enabled="false")
    assert SW.fetch_worklog_block("11111111-1111-1111-1111-111111111111") == ""
    print("OK test_disabilitato_da_setting")


def test_fail_open_su_errore_db() -> None:
    # DB giu' -> stringa vuota, mai eccezione (il worklog non blocca il run).
    _patch(raise_db=True)
    assert SW.fetch_worklog_block("11111111-1111-1111-1111-111111111111") == ""
    print("OK test_fail_open_su_errore_db")


def test_session_id_vuoto() -> None:
    _patch(row=("contenuto",))
    assert SW.fetch_worklog_block("") == ""
    print("OK test_session_id_vuoto")


# ── learned instructions (mig 0412) ─────────────────────────────────────────

def test_learned_blocco_da_regole_attive() -> None:
    _patch_learned(rows=[("tooling", "Usa pnpm, mai npm"), ("environment", "systemctl --user")])
    out = SW.fetch_learned_instructions_block("22222222-2222-2222-2222-222222222222")
    assert out.startswith("<learned_instructions>"), out
    assert out.endswith("</learned_instructions>"), out
    assert "[tooling]" in out and "Usa pnpm" in out
    assert "[environment]" in out
    print("OK test_learned_blocco_da_regole_attive")


def test_learned_usa_template_db() -> None:
    _patch_learned(
        rows=[("tooling", "Usa pnpm")],
        tpl_row=("<learned_instructions>\nCustom header:\n{{rules}}\n</learned_instructions>",),
    )
    out = SW.fetch_learned_instructions_block("22222222-2222-2222-2222-222222222222")
    assert "Custom header:" in out, out
    assert "Usa pnpm" in out
    print("OK test_learned_usa_template_db")


def test_learned_vuoto_se_nessuna_regola() -> None:
    _patch_learned(rows=[])
    assert SW.fetch_learned_instructions_block("22222222-2222-2222-2222-222222222222") == ""
    print("OK test_learned_vuoto_se_nessuna_regola")


def test_learned_disabilitato() -> None:
    _patch_learned(rows=[("tooling", "x")], values={
        "orchestrator.learned_instructions_enabled": "false",
    })
    assert SW.fetch_learned_instructions_block("22222222-2222-2222-2222-222222222222") == ""
    print("OK test_learned_disabilitato")


def test_learned_fail_open() -> None:
    _patch_learned(raise_db=True)
    assert SW.fetch_learned_instructions_block("22222222-2222-2222-2222-222222222222") == ""
    print("OK test_learned_fail_open")


def test_learned_budget_troncato() -> None:
    rows = [("convention", "regola numero " + str(i) + " molto lunga " * 5) for i in range(50)]
    _patch_learned(rows=rows, values={
        "orchestrator.learned_instructions_enabled": "true",
        "orchestrator.learned_instructions_max_chars": "300",
    })
    out = SW.fetch_learned_instructions_block("22222222-2222-2222-2222-222222222222")
    assert "troncate" in out, out
    print("OK test_learned_budget_troncato")


def test_learned_project_vuoto() -> None:
    _patch_learned(rows=[("tooling", "x")])
    assert SW.fetch_learned_instructions_block("") == ""
    print("OK test_learned_project_vuoto")


if __name__ == "__main__":
    test_blocco_wrappato()
    test_vuoto_se_nessuna_riga()
    test_vuoto_se_rendered_block_blank()
    test_disabilitato_da_setting()
    test_fail_open_su_errore_db()
    test_session_id_vuoto()
    test_learned_blocco_da_regole_attive()
    test_learned_usa_template_db()
    test_learned_vuoto_se_nessuna_regola()
    test_learned_disabilitato()
    test_learned_fail_open()
    test_learned_budget_troncato()
    test_learned_project_vuoto()
    print("Tutti i test session_worklog + learned OK")
    sys.exit(0)
