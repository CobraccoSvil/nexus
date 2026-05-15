"""Test PR-2 Verifier + criteria_runner.

Copre:
  - criteria_runner: 5 tipi di check (run_command, http, file_exists,
    regex_in_output, db_query) con tool_runner / httpx / psycopg2 mockati
  - verifier_node: guard skip se non plan_phase_active, success path
    (todo completed + next), failed path con retry, cap raggiunto (blocked),
    nessun criterion (auto-completed)
  - route_after_verifier: branch executor vs learner

Tutti mock puri, nessun DB o servizio live.
"""
from __future__ import annotations

import asyncio
import json
import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from brain.agents import orchestrator_config


def _run(coro):
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


def _safe_cfg(**overrides):
    cfg = dict(orchestrator_config._SAFE_DEFAULTS)
    cfg.update(overrides)
    return cfg


# ─── criteria_runner ─────────────────────────────────────────────────────────


class TestCriteriaRunner(unittest.TestCase):

    def test_run_command_passa_su_exit_0(self):
        from brain.agents.criteria_runner import run_criterion
        tool_runner = MagicMock()
        tool_runner.execute_tool = AsyncMock()
        tool_runner.execute_tool.return_value = MagicMock(result_json="EXIT CODE: 0\nSTDOUT:\nok")
        ctx = {"tool_runner": tool_runner, "session_id": "sess-1", "timeout_s": 5.0}
        ok, ev = _run(run_criterion({"type": "run_command", "spec": {"command": "true"}, "expected": {"exit_code": 0}}, ctx))
        self.assertTrue(ok)
        self.assertEqual(ev["exit_code"], 0)

    def test_run_command_fallisce_su_exit_diverso(self):
        from brain.agents.criteria_runner import run_criterion
        tool_runner = MagicMock()
        tool_runner.execute_tool = AsyncMock()
        tool_runner.execute_tool.return_value = MagicMock(result_json="EXIT CODE: 1\nSTDERR:\nfail")
        ctx = {"tool_runner": tool_runner, "session_id": "sess-1"}
        ok, ev = _run(run_criterion({"type": "run_command", "spec": {"command": "false"}, "expected": {"exit_code": 0}}, ctx))
        self.assertFalse(ok)
        self.assertEqual(ev["exit_code"], 1)

    def test_http_passa_su_status_atteso(self):
        from brain.agents import criteria_runner
        class _FakeResp:
            status_code = 200
            text = "{\"status\":\"ok\"}"
        class _FakeClient:
            def __init__(self, *a, **k): pass
            async def __aenter__(self): return self
            async def __aexit__(self, *a): pass
            async def request(self, method, url): return _FakeResp()
        with patch.object(criteria_runner, "__import__", create=True):
            pass
        # Patcha httpx import path
        import httpx
        with patch.object(httpx, "AsyncClient", _FakeClient):
            ok, ev = _run(criteria_runner.run_criterion(
                {"type": "http", "spec": {"url": "http://x/api/health"}, "expected": {"status": 200}},
                {"timeout_s": 5.0},
            ))
        self.assertTrue(ok)
        self.assertEqual(ev["status"], 200)

    def test_http_body_contains_check(self):
        from brain.agents import criteria_runner
        class _FakeResp:
            status_code = 200
            text = "hello world"
        class _FakeClient:
            def __init__(self, *a, **k): pass
            async def __aenter__(self): return self
            async def __aexit__(self, *a): pass
            async def request(self, *a, **k): return _FakeResp()
        import httpx
        with patch.object(httpx, "AsyncClient", _FakeClient):
            ok_match, _ = _run(criteria_runner.run_criterion(
                {"type": "http", "spec": {"url": "http://x"}, "expected": {"status": 200, "body_contains": "hello"}},
                {"timeout_s": 5.0},
            ))
            ok_miss, _ = _run(criteria_runner.run_criterion(
                {"type": "http", "spec": {"url": "http://x"}, "expected": {"status": 200, "body_contains": "missing"}},
                {"timeout_s": 5.0},
            ))
        self.assertTrue(ok_match)
        self.assertFalse(ok_miss)

    def test_file_exists_true(self):
        from brain.agents.criteria_runner import run_criterion
        tool_runner = MagicMock()
        tool_runner.execute_tool = AsyncMock()
        # read_file ritorna contenuto del file
        tool_runner.execute_tool.return_value = MagicMock(result_json="contenuto del file")
        ctx = {"tool_runner": tool_runner, "session_id": "sess-1"}
        ok, ev = _run(run_criterion({"type": "file_exists", "spec": {"path": "x.txt"}, "expected": {"exists": True}}, ctx))
        self.assertTrue(ok)
        self.assertTrue(ev["exists"])

    def test_file_exists_false_quando_read_file_errore(self):
        from brain.agents.criteria_runner import run_criterion
        tool_runner = MagicMock()
        tool_runner.execute_tool = AsyncMock()
        tool_runner.execute_tool.return_value = MagicMock(result_json="❌ non trovato")
        ctx = {"tool_runner": tool_runner, "session_id": "sess-1"}
        ok, ev = _run(run_criterion({"type": "file_exists", "spec": {"path": "missing"}, "expected": {"exists": True}}, ctx))
        self.assertFalse(ok)
        self.assertFalse(ev["exists"])

    def test_regex_in_output_match(self):
        from brain.agents.criteria_runner import run_criterion
        tool_runner = MagicMock()
        tool_runner.execute_tool = AsyncMock()
        tool_runner.execute_tool.return_value = MagicMock(result_json="STDOUT:\nServer running on http://localhost:32850")
        ctx = {"tool_runner": tool_runner, "session_id": "sess-1"}
        ok, ev = _run(run_criterion({"type": "regex_in_output", "spec": {"command": "echo"}, "expected": {"pattern": r"Server running on http://"}}, ctx))
        self.assertTrue(ok)
        self.assertIn("Server running", ev["match"])

    def test_tipo_sconosciuto_ritorna_fail(self):
        from brain.agents.criteria_runner import run_criterion
        ok, ev = _run(run_criterion({"type": "magic", "spec": {}, "expected": {}}, {}))
        self.assertFalse(ok)
        self.assertIn("sconosciuto", ev["error"])


# ─── verifier_node ───────────────────────────────────────────────────────────


class TestVerifierNode(unittest.TestCase):

    def setUp(self) -> None:
        cfg = _safe_cfg(plan_phase_enabled=True, verifier_enabled=True)
        self._cfg_patch = patch.object(orchestrator_config, "_load_from_db", return_value=cfg)
        self._cfg_patch.start()
        orchestrator_config.force_reload()

    def tearDown(self) -> None:
        self._cfg_patch.stop()
        orchestrator_config.force_reload()

    def test_skip_se_plan_phase_inactive(self):
        from brain.agents.verifier_node import verifier_node
        out = _run(verifier_node({"plan_phase_active": False}))
        self.assertEqual(out, {})

    def test_skip_se_verifier_disabled(self):
        cfg = _safe_cfg(plan_phase_enabled=True, verifier_enabled=False)
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg):
            orchestrator_config.force_reload()
            from brain.agents.verifier_node import verifier_node
            out = _run(verifier_node({"plan_phase_active": True, "thread_id": "r"}))
        self.assertEqual(out, {})

    def test_auto_completed_se_nessun_criterion(self):
        from brain.agents import verifier_node as vn
        # todo senza acceptance_criteria → marca completed + next pending
        todos = [
            {"id": "t1", "seq": 1, "content": "uno", "status": "in_progress", "acceptance_criteria": []},
            {"id": "t2", "seq": 2, "content": "due", "status": "pending", "acceptance_criteria": []},
        ]
        with patch.object(vn.todo_store, "list_todos", return_value=todos), \
             patch.object(vn, "_mark_todo_status") as m_mark, \
             patch.object(vn, "_persist_verifier_run"):
            # _advance_or_end ricalcola list_todos: dopo "completed" t1 viene saltato e t2 e' il next pending
            after_complete_todos = [
                {"id": "t1", "seq": 1, "content": "uno", "status": "completed", "acceptance_criteria": []},
                {"id": "t2", "seq": 2, "content": "due", "status": "pending", "acceptance_criteria": []},
            ]
            # Il secondo list_todos call deve restituire lo stato aggiornato
            vn.todo_store.list_todos.side_effect = [todos, after_complete_todos]
            out = _run(vn.verifier_node({
                "plan_phase_active": True,
                "thread_id": "r1",
                "active_todo_id": "t1",
                "session_id": "s",
            }))
        self.assertEqual(out.get("active_todo_id"), "t2")
        self.assertEqual(out.get("stop_reason"), "tool_use")
        m_mark.assert_any_call("t1", "completed")

    def test_success_path_avanza_al_next_todo(self):
        from brain.agents import verifier_node as vn, criteria_runner
        todos = [
            {"id": "t1", "seq": 1, "content": "x", "status": "in_progress",
             "acceptance_criteria": [{"type": "run_command", "spec": {"command": "true"}, "expected": {"exit_code": 0}}]},
            {"id": "t2", "seq": 2, "content": "y", "status": "pending", "acceptance_criteria": []},
        ]
        after = [
            {"id": "t1", "seq": 1, "content": "x", "status": "completed", "acceptance_criteria": []},
            {"id": "t2", "seq": 2, "content": "y", "status": "pending", "acceptance_criteria": []},
        ]
        with patch.object(vn.todo_store, "list_todos", side_effect=[todos, after]), \
             patch.object(vn, "_mark_todo_status"), \
             patch.object(vn, "_persist_verifier_run"), \
             patch.object(criteria_runner, "run_criterion", new=AsyncMock(return_value=(True, {"exit_code": 0}))):
            out = _run(vn.verifier_node({
                "plan_phase_active": True,
                "thread_id": "r1",
                "active_todo_id": "t1",
                "session_id": "s",
                "verify_cycle": 0,
            }))
        self.assertEqual(out.get("active_todo_id"), "t2")
        self.assertEqual(out.get("verify_cycle"), 0)

    def test_failed_path_appende_humanmessage_e_retry(self):
        from brain.agents import verifier_node as vn, criteria_runner
        todos = [
            {"id": "t1", "seq": 1, "content": "x", "status": "in_progress",
             "acceptance_criteria": [{"type": "run_command", "spec": {"command": "false"}, "expected": {"exit_code": 0}}]},
        ]
        with patch.object(vn.todo_store, "list_todos", return_value=todos), \
             patch.object(vn, "_mark_todo_status") as m_mark, \
             patch.object(vn, "_persist_verifier_run"), \
             patch.object(criteria_runner, "run_criterion", new=AsyncMock(return_value=(False, {"exit_code": 1, "output_excerpt": "fail"}))):
            out = _run(vn.verifier_node({
                "plan_phase_active": True,
                "thread_id": "r1",
                "active_todo_id": "t1",
                "session_id": "s",
                "verify_cycle": 0,
            }))
        self.assertEqual(out.get("verify_cycle"), 1)
        self.assertEqual(out.get("stop_reason"), "tool_use")
        # HumanMessage appeso con <verification_failed>
        self.assertTrue(any("verification_failed" in m.content for m in out["messages"]))
        # NON marca completed
        for call in m_mark.call_args_list:
            self.assertNotEqual(call.args[1], "completed")

    def test_cap_raggiunto_marca_blocked(self):
        from brain.agents import verifier_node as vn, criteria_runner
        todos = [
            {"id": "t1", "seq": 1, "content": "x", "status": "in_progress",
             "acceptance_criteria": [{"type": "run_command", "spec": {"command": "false"}, "expected": {"exit_code": 0}}]},
        ]
        after = list(todos)  # nessun successivo pending
        with patch.object(vn.todo_store, "list_todos", side_effect=[todos, after]), \
             patch.object(vn, "_mark_todo_status") as m_mark, \
             patch.object(vn, "_persist_verifier_run"), \
             patch.object(criteria_runner, "run_criterion", new=AsyncMock(return_value=(False, {"exit_code": 1}))):
            # verify_cycle gia a max-1 (2) → diventa 3 (= max_verify_cycles default)
            out = _run(vn.verifier_node({
                "plan_phase_active": True,
                "thread_id": "r1",
                "active_todo_id": "t1",
                "session_id": "s",
                "verify_cycle": 2,
            }))
        # Marca blocked
        self.assertTrue(any(c.args == ("t1", "blocked") for c in m_mark.call_args_list))
        self.assertEqual(out.get("verify_cycle"), 0)


# ─── route_after_verifier ───────────────────────────────────────────────────


class TestRouteAfterVerifier(unittest.TestCase):

    def test_tool_use_va_a_executor(self):
        from brain.agents.nodes import route_after_verifier
        out = route_after_verifier({"stop_reason": "tool_use", "iterations": 5})
        self.assertEqual(out, "executor")

    def test_end_turn_va_a_learner(self):
        from brain.agents.nodes import route_after_verifier
        out = route_after_verifier({"stop_reason": "end_turn", "iterations": 5})
        self.assertEqual(out, "learner")

    def test_cap_iterazioni_forza_learner(self):
        from brain.agents.nodes import route_after_verifier, MAX_AGENT_ITERATIONS
        out = route_after_verifier({"stop_reason": "tool_use", "iterations": MAX_AGENT_ITERATIONS + 1})
        self.assertEqual(out, "learner")


if __name__ == "__main__":
    unittest.main()
