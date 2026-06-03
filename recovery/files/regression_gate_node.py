"""Test del regression_gate_node (M13.4 SOFT + M13.5 HARD).

Copre la logica di gating, l'invarianza SOFT (default) e i 3 rami HARD
(block/cap/non-eligible) senza dipendere da DB, mcp-core o tool_runner reali:
tutte le dipendenze esterne sono monkeypatchate.

Invariante chiave default-OFF (hard_block=false): il nodo NON deve MAI mettere
stop_reason ne' rimandare all'executor. Al piu' emette `meta_steps` (warning),
una nota KB e un todo di follow-up. Comportamento identico a M13.4.
"""
from __future__ import annotations

import asyncio
from typing import Any

import pytest

from brain.agents import regression_gate_node as rg


class _FakeToolResult:
    def __init__(self, result_json: str = "EXIT CODE: 0") -> None:
        self.result_json = result_json


class _FakeToolRunner:
    """Registra le chiamate e ritorna un exit code configurabile per run_command."""

    def __init__(self, exit_code: int = 0) -> None:
        self._exit_code = exit_code
        self.calls: list[dict[str, Any]] = []

    async def execute_tool(self, *, tool_name: str, tool_input: dict, session_id: str, tool_use_id: str):
        self.calls.append({"tool_name": tool_name, "tool_input": tool_input})
        return _FakeToolResult(f"EXIT CODE: {self._exit_code}")


def _patch_settings(
    monkeypatch, *, enabled=True, soft_only=True, hard_block=False,
    max_tests=10, max_cycles=1, timeout=120,
):
    monkeypatch.setattr(
        "brain.utils.settings_db.get_bool_setting",
        lambda key, default=False: {"regression_gate.enabled": enabled,
                                     "regression_gate.soft_only": soft_only,
                                     "regression_gate.hard_block": hard_block}.get(key, default),
    )
    monkeypatch.setattr(
        "brain.utils.settings_db.get_int_setting",
        lambda key, default=0: {"regression_gate.max_tests": max_tests,
                                "regression_gate.max_cycles": max_cycles,
                                "regression_gate.test_timeout_s": timeout}.get(key, default),
    )


def _capture_record_run(monkeypatch):
    """Monkeypatch _record_run e ritorna la lista (mutabile) delle chiamate."""
    calls: list[dict[str, Any]] = []
    monkeypatch.setattr(rg, "_record_run", lambda **kw: calls.append(kw))
    return calls


def _base_state() -> dict[str, Any]:
    return {"thread_id": "run-abc-123", "project_id": "proj-1", "session_id": "sess-1"}


def test_disabled_returns_empty(monkeypatch):
    """enabled=false -> skip immediato, nessun effetto."""
    _patch_settings(monkeypatch, enabled=False)
    out = asyncio.run(rg.regression_gate_node(_base_state()))
    assert out == {}


def test_no_modified_files_skips(monkeypatch):
    """Nessun file modificato -> skip senza chiamare mcp-core."""
    _patch_settings(monkeypatch)
    monkeypatch.setattr(rg, "_modified_files_from_steps", lambda run_id: [])
    called = {"impact": False}
    monkeypatch.setattr(
        rg, "_fetch_impact_tests",
        lambda pid, paths: called.__setitem__("impact", True) or {"ok": True, "tests": []},
    )
    out = asyncio.run(rg.regression_gate_node(_base_state()))
    assert out == {}
    assert called["impact"] is False


def test_zero_tests_skips(monkeypatch):
    """File modificati ma 0 test sull'impact set -> skip."""
    _patch_settings(monkeypatch)
    monkeypatch.setattr(rg, "_modified_files_from_steps", lambda run_id: ["src/a.py"])
    monkeypatch.setattr(rg, "_fetch_impact_tests", lambda pid, paths: {"ok": True, "tests": []})
    out = asyncio.run(rg.regression_gate_node(_base_state()))
    assert out == {}


def test_all_tests_pass_no_warning(monkeypatch):
    """Tutti i test passano -> nessun meta_step, record-run gate_status='passed'."""
    _patch_settings(monkeypatch)
    rg.configure(_FakeToolRunner(exit_code=0))
    rec = _capture_record_run(monkeypatch)
    monkeypatch.setattr(rg, "_modified_files_from_steps", lambda run_id: ["src/a.py"])
    monkeypatch.setattr(
        rg, "_fetch_impact_tests",
        lambda pid, paths: {"ok": True, "tests": [{"test_path": "test_a.py", "method": "import"}]},
    )
    out = asyncio.run(rg.regression_gate_node(_base_state()))
    assert out == {}
    assert len(rec) == 1
    assert rec[0]["gate_status"] == "passed"


def test_failed_test_emits_soft_warning_without_blocking(monkeypatch):
    """hard_block=false (DEFAULT) + test fallito -> meta_step SOFT, NESSUN blocco.

    Invariante default-OFF (identica a M13.4): lo state patch non contiene
    stop_reason ne' alcun segnale di re-execution; contiene solo meta_steps.
    record-run registra gate_status='warning'.
    """
    _patch_settings(monkeypatch)  # hard_block=false di default
    rg.configure(_FakeToolRunner(exit_code=1))
    rec = _capture_record_run(monkeypatch)
    monkeypatch.setattr(rg, "_modified_files_from_steps", lambda run_id: ["src/a.py", "src/b.py"])
    monkeypatch.setattr(
        rg, "_fetch_impact_tests",
        lambda pid, paths: {"ok": True,
                            "tests": [{"test_path": "test_a.py", "method": "import",
                                       "confidence": 0.8}]},
    )

    emitted = {"called": False}

    async def _fake_emit(**kwargs):
        emitted["called"] = True

    monkeypatch.setattr(rg, "_emit_soft_warning", _fake_emit)

    out = asyncio.run(rg.regression_gate_node(_base_state()))

    assert emitted["called"] is True
    assert "meta_steps" in out
    assert out["meta_steps"][0]["kind"] == "regression_warning"
    assert out["meta_steps"][0]["payload"]["mode"] == "soft"
    # DEFAULT-OFF: niente blocco, identico a M13.4.
    assert "stop_reason" not in out
    assert "regression_cycle" not in out
    assert len(rec) == 1 and rec[0]["gate_status"] == "warning"


# ─── M13.5: HARD block ──────────────────────────────────────────────────────


def _hard_state(cycle: int = 0) -> dict[str, Any]:
    s = _base_state()
    if cycle:
        s["regression_cycle"] = cycle
    return s


def _setup_failing_hard_test(monkeypatch, *, method="import", confidence=0.8):
    """Comune ai test HARD: 1 test fallito mappato (method/confidence dati)."""
    rg.configure(_FakeToolRunner(exit_code=1))
    monkeypatch.setattr(rg, "_modified_files_from_steps", lambda run_id: ["src/a.py"])
    monkeypatch.setattr(
        rg, "_fetch_impact_tests",
        lambda pid, paths: {"ok": True,
                            "tests": [{"test_path": "test_a.py", "method": method,
                                       "confidence": confidence}]},
    )

    async def _fake_emit(**kwargs):
        _fake_emit.called = True  # type: ignore[attr-defined]

    _fake_emit.called = False  # type: ignore[attr-defined]
    monkeypatch.setattr(rg, "_emit_soft_warning", _fake_emit)
    return _fake_emit


def test_hard_block_returns_to_executor(monkeypatch):
    """hard_block=true + fallimento HARD-eligible + cycle<max -> ritorno a executor."""
    _patch_settings(monkeypatch, soft_only=False, hard_block=True, max_cycles=1)
    rec = _capture_record_run(monkeypatch)
    emit = _setup_failing_hard_test(monkeypatch)

    out = asyncio.run(rg.regression_gate_node(_hard_state(cycle=0)))

    assert out["stop_reason"] == "tool_use"
    assert out["pending_tool_uses"] == []
    assert out["regression_cycle"] == 1
    assert "messages" in out and len(out["messages"]) == 1
    assert "<regression_detected" in out["messages"][0].content
    # BLOCK non crea il todo (si ritenta): _emit_soft_warning non chiamato.
    assert emit.called is False
    assert len(rec) == 1 and rec[0]["gate_status"] == "blocked"


def test_hard_block_caps_to_soft(monkeypatch):
    """hard_block=true + cycle>=max -> degrada a SOFT, gate_status='blocked_capped'.

    Non ritorna a executor (no stop_reason): prosegue verso learner.
    """
    _patch_settings(monkeypatch, soft_only=False, hard_block=True, max_cycles=1)
    rec = _capture_record_run(monkeypatch)
    emit = _setup_failing_hard_test(monkeypatch)

    out = asyncio.run(rg.regression_gate_node(_hard_state(cycle=1)))

    assert "stop_reason" not in out
    assert "regression_cycle" not in out
    assert out["meta_steps"][0]["payload"]["mode"] == "blocked_capped"
    # CAP degrada a SOFT: nota+todo creati.
    assert emit.called is True
    assert len(rec) == 1 and rec[0]["gate_status"] == "blocked_capped"


def test_hard_block_non_eligible_stays_soft(monkeypatch):
    """hard_block=true ma fallimento NON HARD-eligible (semantic) -> resta SOFT."""
    _patch_settings(monkeypatch, soft_only=False, hard_block=True, max_cycles=1)
    rec = _capture_record_run(monkeypatch)
    emit = _setup_failing_hard_test(monkeypatch, method="semantic", confidence=0.9)

    out = asyncio.run(rg.regression_gate_node(_hard_state(cycle=0)))

    # method=semantic non e' HARD-eligible: nessun blocco, ramo SOFT.
    assert "stop_reason" not in out
    assert out["meta_steps"][0]["payload"]["mode"] == "soft"
    assert emit.called is True
    assert len(rec) == 1 and rec[0]["gate_status"] == "warning"


def test_hard_block_low_confidence_stays_soft(monkeypatch):
    """hard_block=true, method import ma confidence<0.6 -> NON HARD-eligible, SOFT."""
    _patch_settings(monkeypatch, soft_only=False, hard_block=True, max_cycles=1)
    rec = _capture_record_run(monkeypatch)
    _setup_failing_hard_test(monkeypatch, method="import", confidence=0.5)

    out = asyncio.run(rg.regression_gate_node(_hard_state(cycle=0)))

    assert "stop_reason" not in out
    assert out["meta_steps"][0]["payload"]["mode"] == "soft"
    assert len(rec) == 1 and rec[0]["gate_status"] == "warning"


def test_hard_eligible_helper():
    """_is_hard_eligible: import/naming + confidence>=0.6, altrimenti False."""
    assert rg._is_hard_eligible({"method": "import", "confidence": 0.6}) is True
    assert rg._is_hard_eligible({"method": "naming", "confidence": 0.8}) is True
    assert rg._is_hard_eligible({"method": "import", "confidence": 0.59}) is False
    assert rg._is_hard_eligible({"method": "semantic", "confidence": 0.9}) is False
    assert rg._is_hard_eligible({"method": "import", "confidence": None}) is False
    assert rg._is_hard_eligible({"method": None, "confidence": 0.9}) is False
    assert rg._is_hard_eligible({}) is False


def test_command_mapping():
    """Mapping deterministico test_path -> comando runner."""
    assert rg._test_command_for("e2e/login.spec.ts") == "npx playwright test e2e/login.spec.ts"
    assert rg._test_command_for("src/foo.test.ts") == "npx vitest run src/foo.test.ts"
    assert rg._test_command_for("tests/test_user.py") == "pytest tests/test_user.py"
    assert rg._test_command_for("crates/x/tests/it.rs") == "cargo test"
    assert rg._test_command_for("README.md") is None


def test_gate_never_raises_on_internal_error(monkeypatch):
    """Errori interni del gate non si propagano: ritorna {} (best-effort)."""
    _patch_settings(monkeypatch)

    def _boom(run_id):
        raise RuntimeError("boom")

    monkeypatch.setattr(rg, "_modified_files_from_steps", _boom)
    out = asyncio.run(rg.regression_gate_node(_base_state()))
    assert out == {}
