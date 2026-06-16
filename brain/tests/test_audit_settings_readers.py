"""Regressioni del censimento configurazioni (scripts/audit_settings.py).

Coprono i due falsi del gate ratchet (scripts/audit-settings-baseline.json)
emersi il 2026-06-16:

  * MORTA falso positivo: il rilevatore di lettori SQL (SQL_KEY_EQ_RE) non
    riconosceva le query `WHERE key = '...'` spezzate su literal Python
    adiacenti (concatenazione implicita), come in brain/agents/final_gate.py.
  * FANTASMA: la chiave agent.loop.resource_reallocation_threshold, letta da
    brain/agents/nodes/helpers.py, non era definita da nessuna migrazione
    (regola G: la configurazione vive nel DB).

Il modulo audit_settings.py vive in scripts/ (non e' un package): lo carichiamo
via importlib dal percorso assoluto, indipendente da cwd/PYTHONPATH.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
_AUDIT_PATH = _ROOT / "scripts" / "audit_settings.py"


def _load_audit_module():
    spec = importlib.util.spec_from_file_location(
        "audit_settings_under_test", _AUDIT_PATH
    )
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_sql_key_eq_re_matches_single_line():
    """La forma canonica su una riga resta riconosciuta (nessuna regressione)."""
    mod = _load_audit_module()
    text = "cur.execute(\"SELECT value FROM settings WHERE key = 'agent.foo.bar'\")"
    keys = [m.group(1) for m in mod.SQL_KEY_EQ_RE.finditer(text)]
    assert keys == ["agent.foo.bar"]


def test_sql_key_eq_re_matches_python_implicit_concat():
    """La SELECT spezzata su literal adiacenti (FROM settings nel primo, WHERE
    key nel secondo) deve comunque produrre la chiave: e' la forma reale di
    brain/agents/final_gate.py:120-123."""
    mod = _load_audit_module()
    text = (
        "            cur.execute(\n"
        "                \"SELECT value FROM settings \"\n"
        "                \"WHERE key = 'agent.final_gate.runtime_log_command_per_project'\"\n"
        "            )\n"
    )
    keys = [m.group(1) for m in mod.SQL_KEY_EQ_RE.finditer(text)]
    assert "agent.final_gate.runtime_log_command_per_project" in keys


def test_final_gate_key_detected_as_reader():
    """End-to-end sul sorgente reale: la chiave del final_gate risulta tra i
    lettori censiti, quindi non e' piu' MORTA."""
    mod = _load_audit_module()
    readers, _unresolved, _quoted = mod.collect_code_readers()
    assert "agent.final_gate.runtime_log_command_per_project" in readers


def test_reallocation_threshold_defined_in_migration():
    """La chiave letta da helpers.py deve essere inserita da una migrazione
    versionata (regola G): altrimenti torna FANTASMA."""
    mod = _load_audit_module()
    inserted, _deleted = mod.collect_migrations()
    assert "agent.loop.resource_reallocation_threshold" in inserted
