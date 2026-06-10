"""Test WAVE 3.3 — closure_judge (giudice LLM di chiusura in shadow).

Copre le funzioni PURE (parsing strict-bool, prompt builder) e il gating di
astensione di run_shadow senza dipendenze esterne (DB/LLM mockati). Eseguibile
a mano: `PYTHONPATH=. python3 brain/tests/test_closure_judge.py` (no pytest di
sistema, vedi convenzione brain/tests).
"""
from __future__ import annotations

import asyncio
import sys

from brain.agents.nodes import closure_judge as cj


def test_parse_strict_bool() -> None:
    # Bool reale True/False -> accettato.
    assert cj._parse_response('{"fulfilled": true, "reason": "fatto"}') == {
        "fulfilled": True,
        "reason": "fatto",
    }
    assert cj._parse_response('{"fulfilled": false, "reason": "rimandato"}')["fulfilled"] is False
    # JSON annegato in testo -> estratto.
    assert cj._parse_response('ecco: {"fulfilled": true, "reason": "ok"} fine')["fulfilled"] is True
    # bool come stringa "false" -> NON deve diventare True: astensione (None).
    assert cj._parse_response('{"fulfilled": "false", "reason": "x"}') is None
    # fulfilled assente -> astensione.
    assert cj._parse_response('{"reason": "x"}') is None
    # numerico 1/0 -> non bool reale -> astensione.
    assert cj._parse_response('{"fulfilled": 1}') is None
    # raw vuoto / non JSON -> None.
    assert cj._parse_response("") is None
    assert cj._parse_response("nessun json qui") is None
    print("OK test_parse_strict_bool")


def test_prompt_contains_task_and_result() -> None:
    p = cj._build_prompt("crea il login", "ho creato il file login.tsx")
    assert "crea il login" in p
    assert "login.tsx" in p
    assert "JSON" in p  # impone output chiuso
    # Niente blacklist di frasi-spia nel prompt (giudizio semantico, non lessicale).
    assert "non posso" not in p.lower()
    print("OK test_prompt_contains_task_and_result")


def test_shadow_abstains_when_disabled(monkeypatch_cfg: dict | None = None) -> None:
    # shadow_enabled=False -> run_shadow ritorna subito senza toccare providers.
    orig = cj._load_config
    cj._load_config = lambda: {"shadow_enabled": False, "min_result_chars": 40}
    try:
        called = {"providers": False}

        class _Boom:
            @property
            def _providers(self):  # pragma: no cover - non deve essere letto
                called["providers"] = True
                return {}

        asyncio.run(cj.run_shadow({"result": "x" * 100}, _Boom(), lambda r: False))
        assert called["providers"] is False
    finally:
        cj._load_config = orig
    print("OK test_shadow_abstains_when_disabled")


def test_shadow_abstains_when_declared() -> None:
    # Esito gia' dichiarato via task_complete -> judge inutile, astensione.
    orig = cj._load_config
    cj._load_config = lambda: {"shadow_enabled": True, "min_result_chars": 40}
    try:
        called = {"resolve": False}
        orig_resolve = cj._resolve_model

        async def _spy():
            called["resolve"] = True
            return None

        cj._resolve_model = _spy
        state = {
            "result": "y" * 100,
            "declared_outcome": {"outcome": "done"},
            "turn_action_oriented": True,
        }
        asyncio.run(cj.run_shadow(state, object(), lambda r: False))
        assert called["resolve"] is False  # non arriva nemmeno a risolvere il modello
        cj._resolve_model = orig_resolve
    finally:
        cj._load_config = orig
    print("OK test_shadow_abstains_when_declared")


def test_shadow_abstains_short_result() -> None:
    orig = cj._load_config
    cj._load_config = lambda: {"shadow_enabled": True, "min_result_chars": 40}
    try:
        called = {"resolve": False}
        orig_resolve = cj._resolve_model

        async def _spy():
            called["resolve"] = True
            return None

        cj._resolve_model = _spy
        # result corto (< soglia) -> astensione.
        asyncio.run(cj.run_shadow({"result": "breve", "turn_action_oriented": True}, object(), lambda r: False))
        assert called["resolve"] is False
        cj._resolve_model = orig_resolve
    finally:
        cj._load_config = orig
    print("OK test_shadow_abstains_short_result")


if __name__ == "__main__":
    test_parse_strict_bool()
    test_prompt_contains_task_and_result()
    test_shadow_abstains_when_disabled()
    test_shadow_abstains_when_declared()
    test_shadow_abstains_short_result()
    print("\nTUTTI I TEST closure_judge PASSATI")
    sys.exit(0)
