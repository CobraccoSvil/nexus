"""Test adattamento del prompt al modello di fallback (mig 0393).

Eseguibile a mano: `PYTHONPATH=. python3 brain/tests/test_fallback_adapt.py`.
"""
from __future__ import annotations

import sys

from brain.providers import registry as R


def test_inject_returns_new_list_user_last() -> None:
    msgs = [{"role": "user", "content": "fai X"}, {"role": "assistant", "content": "ok..."}]
    out = R.inject_fallback_directive(msgs, "DIRETTIVA")
    assert out is not msgs, "deve ritornare una NUOVA lista"
    assert len(msgs) == 2, "non deve mutare l'input"
    assert out[-1] == {"role": "user", "content": "DIRETTIVA"}
    # Invariante Mistral: ultimo ruolo user (fix 422 trailing-assistant).
    assert out[-1]["role"] == "user"
    print("OK test_inject_returns_new_list_user_last")


def test_inject_idempotente() -> None:
    msgs = [{"role": "user", "content": "DIRETTIVA"}]
    out = R.inject_fallback_directive(msgs, "DIRETTIVA")
    assert len(out) == 1, "direttiva gia' in coda -> non duplicare"
    print("OK test_inject_idempotente")


def test_directive_disabled_returns_none() -> None:
    from brain.utils import settings_db
    orig_bool, orig_get = settings_db.get_bool_setting, settings_db.get_setting
    settings_db.get_bool_setting = lambda k, d=False: False
    try:
        assert R._fallback_adapt_directive("m") is None
    finally:
        settings_db.get_bool_setting, settings_db.get_setting = orig_bool, orig_get
    print("OK test_directive_disabled_returns_none")


def test_directive_default_and_placeholder() -> None:
    from brain.utils import settings_db
    orig_bool, orig_get = settings_db.get_bool_setting, settings_db.get_setting
    settings_db.get_bool_setting = lambda k, d=False: True
    settings_db.get_setting = lambda k, d="": "Subentra {model}: passi piccoli."
    try:
        d = R._fallback_adapt_directive("deepseek-v4-pro")
        assert d == "Subentra deepseek-v4-pro: passi piccoli.", d
        # Default quando il setting e' vuoto.
        settings_db.get_setting = lambda k, d="": ""
        d2 = R._fallback_adapt_directive("x")
        assert "passi PICCOLI" in d2, "deve usare il default del codice"
    finally:
        settings_db.get_bool_setting, settings_db.get_setting = orig_bool, orig_get
    print("OK test_directive_default_and_placeholder")


if __name__ == "__main__":
    test_inject_returns_new_list_user_last()
    test_inject_idempotente()
    test_directive_disabled_returns_none()
    test_directive_default_and_placeholder()
    print("\nTUTTI I TEST fallback_adapt PASSATI")
    sys.exit(0)
