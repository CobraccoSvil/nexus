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


def test_coerce_text_reasoner_blocks() -> None:
    # Regressione (3 bug trovati live): i modelli reasoner (magistral) ritornano
    # content come LISTA di blocchi [thinking, text]; _coerce_text estrae il solo
    # blocco text (dove sta il JSON), ignorando il thinking.
    assert cj._coerce_text("gia stringa") == "gia stringa"
    blocks = [
        {"type": "thinking", "thinking": [{"type": "text", "text": "ragiono..."}], "closed": True},
        {"type": "text", "text": '{"fulfilled": false, "reason": "rimandato"}'},
    ]
    assert cj._coerce_text(blocks) == '{"fulfilled": false, "reason": "rimandato"}'
    # Il verdetto estratto deve essere parsabile end-to-end.
    assert cj._parse_response(cj._coerce_text(blocks)) == {"fulfilled": False, "reason": "rimandato"}
    # Lista senza blocchi text espliciti: fallback concatenazione, mai eccezione.
    assert isinstance(cj._coerce_text([{"type": "thinking", "text": "x"}]), str)
    assert cj._coerce_text([]) == ""
    print("OK test_coerce_text_reasoner_blocks")


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


def test_judge_returns_verdict() -> None:
    # judge() (decisore, mig 0422): mock provider che ritorna JSON valido ->
    # ritorna il verdetto parsato. Indipendente da shadow_enabled.
    orig_cfg, orig_resolve = cj._load_config, cj._resolve_model
    cj._load_config = lambda: {"shadow_enabled": True, "min_result_chars": 40, "active": True}

    async def _res():
        return ("google", "gemini-x")

    cj._resolve_model = _res
    try:
        class _Prov:
            async def generate(self, model, prompt, **kw):  # noqa: ARG002
                class _R:
                    content = '{"fulfilled": false, "reason": "non risolto"}'
                return _R()

        class _Registry:
            _providers = {"google": _Prov()}

        state = {"result": "z" * 100, "turn_action_oriented": True, "messages": []}
        verdict = asyncio.run(cj.judge(state, _Registry()))
        assert verdict == {"fulfilled": False, "reason": "non risolto"}
    finally:
        cj._load_config, cj._resolve_model = orig_cfg, orig_resolve
    print("OK test_judge_returns_verdict")


def test_judge_result_override() -> None:
    # result nello state corto, ma result_override lungo -> il gating lunghezza
    # usa l'override (l'executor lo passa perche' lo state non e' ancora aggiornato).
    orig_cfg, orig_resolve = cj._load_config, cj._resolve_model
    cj._load_config = lambda: {"shadow_enabled": True, "min_result_chars": 40, "active": True}
    called = {"resolve": False}

    async def _spy():
        called["resolve"] = True
        return None

    cj._resolve_model = _spy
    try:
        asyncio.run(cj.judge(
            {"result": "breve", "turn_action_oriented": True, "messages": []},
            object(),
            result_override="x" * 100,
        ))
        assert called["resolve"] is True  # override lungo supera il gating lunghezza
    finally:
        cj._load_config, cj._resolve_model = orig_cfg, orig_resolve
    print("OK test_judge_result_override")


def test_unfulfilled_signal_judge_priority() -> None:
    # _unfulfilled_signal: il verdetto del judge DECIDE, ignorando la blacklist
    # sul result (gerarchia mig 0422: judge prima della blacklist).
    from brain.agents.nodes.routing import _unfulfilled_signal

    assert _unfulfilled_signal({"closure_verdict": {"fulfilled": False}, "result": "tutto ok"}) is True
    assert _unfulfilled_signal({"closure_verdict": {"fulfilled": True}, "result": "non posso procedere"}) is False
    print("OK test_unfulfilled_signal_judge_priority")


def test_unfulfilled_signal_fallback_blacklist() -> None:
    # Senza verdict (o verdict malformato) -> fallback alla blacklist lessicale
    # sullo stesso result: stesso esito di _detect_unfulfilled_intent.
    from brain.agents.nodes.routing import _unfulfilled_signal
    from brain.agents.nodes.helpers import _detect_unfulfilled_intent

    for txt in ("Ho completato il lavoro e i test passano.", "Non posso procedere oltre."):
        assert _unfulfilled_signal({"result": txt}) == _detect_unfulfilled_intent(txt)
    # verdict non-bool -> il judge si astiene -> fallback blacklist.
    assert _unfulfilled_signal(
        {"closure_verdict": {"fulfilled": "false"}, "result": "ok"}
    ) == _detect_unfulfilled_intent("ok")
    print("OK test_unfulfilled_signal_fallback_blacklist")


if __name__ == "__main__":
    test_parse_strict_bool()
    test_coerce_text_reasoner_blocks()
    test_prompt_contains_task_and_result()
    test_shadow_abstains_when_disabled()
    test_shadow_abstains_when_declared()
    test_shadow_abstains_short_result()
    test_judge_returns_verdict()
    test_judge_result_override()
    test_unfulfilled_signal_judge_priority()
    test_unfulfilled_signal_fallback_blacklist()
    print("\nTUTTI I TEST closure_judge PASSATI")
    sys.exit(0)
