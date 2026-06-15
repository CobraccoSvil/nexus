"""Test esecuzione SEQUENZIALE dei todo come sub-run ISOLATE (mig 0431).

Copre:
  - Gating todo_isolation_active (3 condizioni, DEFAULT OFF).
  - route_after_planner: OFF -> executor (INVARIANTE), ON -> todo_runner,
    DAG parallelo prevale.
  - route_after_todo_runner: re-entry / final_gate / learner / fallback executor.
  - todo_runner_node: ON + piano + Continuo dispatcha via dispatch_subagents
    (max_parallel=1), promuove il todo e avanza.
  - todo_runner_node: guard no-op quando l'isolamento non e' attivo.
  - todo_runner_node: fallimento sub-run con on_failure=stop -> blocked + end_turn.

Tutti mock puri: nessun DB, nessun LLM, nessuna rete.
"""
from __future__ import annotations

import asyncio
import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from brain.agents import orchestrator_config as oc
from brain.agents import todo_runner_node as trn
from brain.agents.nodes.routing import route_after_planner, route_after_todo_runner


def _run(coro):
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


def _set_cfg(**overrides):
    """Forza la cache di orchestrator_config con valori espliciti (no DB)."""
    import time
    base = dict(oc._SAFE_DEFAULTS)
    base.update(overrides)
    with oc._lock:
        oc._cache = base
        oc._cache_loaded_at = time.monotonic()  # evita reload dal DB


class TestGating(unittest.TestCase):

    def tearDown(self):
        oc.force_reload()

    def test_default_off_invariante(self):
        # DEFAULT (todo_isolation_enabled=False) -> mai attivo anche con
        # piano + Continuo: il sistema resta invariato.
        _set_cfg(todo_isolation_enabled=False)
        self.assertFalse(
            oc.todo_isolation_active(
                {"plan_phase_active": True, "automation_mode": "continuous"}
            )
        )

    def test_on_richiede_tutte_e_tre(self):
        _set_cfg(todo_isolation_enabled=True)
        # manca il piano
        self.assertFalse(
            oc.todo_isolation_active(
                {"plan_phase_active": False, "automation_mode": "continuous"}
            )
        )
        # modalita' non autonoma
        self.assertFalse(
            oc.todo_isolation_active(
                {"plan_phase_active": True, "automation_mode": "confirm"}
            )
        )
        # tutte e tre -> attivo
        self.assertTrue(
            oc.todo_isolation_active(
                {"plan_phase_active": True, "automation_mode": "continuous"}
            )
        )

    def test_modalita_italiane_e_behavior_fallback(self):
        _set_cfg(todo_isolation_enabled=True)
        # automation_mode italiano
        self.assertTrue(
            oc.todo_isolation_active(
                {"plan_phase_active": True, "automation_mode": "automatico"}
            )
        )
        # fallback a behavior_mode quando automation_mode manca
        self.assertTrue(
            oc.todo_isolation_active(
                {"plan_phase_active": True, "behavior_mode": "continuo"}
            )
        )


class TestRouteAfterPlanner(unittest.TestCase):

    def tearDown(self):
        oc.force_reload()

    def test_off_va_a_executor(self):
        # Setting OFF -> comportamento storico: planner -> executor.
        _set_cfg(todo_isolation_enabled=False)
        self.assertEqual(
            route_after_planner(
                {"plan_phase_active": True, "automation_mode": "continuous"}
            ),
            "executor",
        )

    def test_on_va_a_todo_runner(self):
        _set_cfg(todo_isolation_enabled=True, dag_parallel_enabled=False)
        self.assertEqual(
            route_after_planner(
                {"plan_phase_active": True, "automation_mode": "continuous"}
            ),
            "todo_runner",
        )

    def test_dag_parallel_prevale(self):
        # Precedenza esplicita: DAG parallelo attivo -> executor (non todo_runner).
        _set_cfg(todo_isolation_enabled=True, dag_parallel_enabled=True)
        self.assertEqual(
            route_after_planner(
                {"plan_phase_active": True, "automation_mode": "continuous"}
            ),
            "executor",
        )


class TestRouteAfterTodoRunner(unittest.TestCase):

    def tearDown(self):
        oc.force_reload()

    def test_tool_use_reentry(self):
        self.assertEqual(
            route_after_todo_runner({"stop_reason": "tool_use", "iterations": 1}),
            "todo_runner",
        )

    def test_no_stop_reason_fallback_executor(self):
        # Guard no-op / dispatch fallito -> fallback al loop storico.
        self.assertEqual(route_after_todo_runner({"iterations": 1}), "executor")

    def test_cap_iterazioni_chiude(self):
        self.assertEqual(
            route_after_todo_runner(
                {"stop_reason": "tool_use", "iterations": 999, "iteration_budget": 5}
            ),
            "learner",
        )

    def test_end_turn_senza_software_va_a_learner(self):
        _set_cfg(final_gate_enabled=True)
        with patch("brain.agents.final_gate._is_software_task", return_value=False):
            self.assertEqual(
                route_after_todo_runner(
                    {"stop_reason": "end_turn", "iterations": 1, "plan_phase_active": True}
                ),
                "learner",
            )

    def test_end_turn_software_va_a_final_gate(self):
        _set_cfg(final_gate_enabled=True, final_gate_max_cycles=2)
        with patch("brain.agents.final_gate._is_software_task", return_value=True):
            self.assertEqual(
                route_after_todo_runner(
                    {"stop_reason": "end_turn", "iterations": 1, "plan_phase_active": True}
                ),
                "final_gate",
            )

    def test_superseded_va_a_learner(self):
        self.assertEqual(
            route_after_todo_runner({"stop_reason": "superseded", "iterations": 1}),
            "learner",
        )


class TestTodoRunnerNode(unittest.TestCase):

    def setUp(self):
        _set_cfg(todo_isolation_enabled=True, dag_parallel_enabled=False)
        trn.configure(tool_runner=MagicMock())

    def tearDown(self):
        oc.force_reload()
        trn.configure(tool_runner=None)

    def test_guard_noop_se_non_attivo(self):
        # Isolamento non attivo (modalita' confirm) -> {} (fallback executor).
        out = _run(trn.todo_runner_node(
            {"plan_phase_active": True, "automation_mode": "confirm", "thread_id": "r1"}
        ))
        self.assertEqual(out, {})

    def test_dispatch_e_avanza(self):
        # Due todo pending: il primo viene dispatchato via dispatch_subagents
        # (max_parallel=1), promosso, e si avanza al secondo (stop_reason=tool_use).
        todos = [
            {"id": "t1", "seq": 1, "content": "crea modello", "status": "pending",
             "acceptance_criteria": [], "depends_on": []},
            {"id": "t2", "seq": 2, "content": "crea rotta", "status": "pending",
             "acceptance_criteria": [], "depends_on": []},
        ]
        # Snapshot MUTABILE: il mock di _mark_todo_status aggiorna lo stato del
        # todo nello snapshot, cosi' list_todos (rilettura in _advance_patch)
        # vede t1 completed -> _pick_next_todo avanza a t2, come in produzione.
        tool_runner = MagicMock()
        tool_result = MagicMock()
        tool_result.result_json = (
            '{"count": 1, "ok": 1, "failed": 0, "results": ['
            '{"status": "completed", "summary": "modello creato", "cost_usd": 0.01, '
            '"tokens": {"prompt": 100, "completion": 20}, "error": null}]}'
        )
        tool_runner.execute_tool = AsyncMock(return_value=tool_result)
        trn.configure(tool_runner=tool_runner)

        marks: list = []

        def _mark(todo_id, status):
            marks.append((todo_id, status))
            for t in todos:
                if t["id"] == todo_id:
                    t["status"] = status

        with patch.object(trn.todo_store, "list_todos", return_value=todos), \
             patch.object(trn, "_mark_todo_status", side_effect=_mark):
            out = _run(trn.todo_runner_node({
                "plan_phase_active": True,
                "automation_mode": "continuous",
                "thread_id": "r1",
                "session_id": "s1",
                "subagent_results": [],
            }))

        # Ha dispatchato esattamente una sub-run con max_parallel=1.
        tool_runner.execute_tool.assert_awaited_once()
        _, kwargs = tool_runner.execute_tool.call_args
        self.assertEqual(kwargs["tool_name"], "dispatch_subagents")
        self.assertEqual(kwargs["tool_input"]["max_parallel"], 1)
        self.assertEqual(len(kwargs["tool_input"]["tasks"]), 1)
        self.assertEqual(kwargs["tool_input"]["tasks"][0]["task"], "crea modello")
        # Il todo t1 e' stato marcato in_progress poi completed.
        self.assertIn(("t1", "in_progress"), marks)
        self.assertIn(("t1", "completed"), marks)
        # Avanza al prossimo (stop_reason=tool_use) e accumula il summary.
        self.assertEqual(out["stop_reason"], "tool_use")
        self.assertEqual(out["active_todo_id"], "t2")
        self.assertEqual(len(out["subagent_results"]), 1)
        self.assertEqual(out["subagent_results"][0]["status"], "completed")

    def test_fallimento_stop_blocca_e_chiude(self):
        # Sub-run failed con on_failure=stop -> todo blocked + end_turn.
        _set_cfg(
            todo_isolation_enabled=True,
            dag_parallel_enabled=False,
            todo_isolation_on_failure="stop",
        )
        todos = [
            {"id": "t1", "seq": 1, "content": "task fragile", "status": "pending",
             "acceptance_criteria": [], "depends_on": []},
        ]
        tool_runner = MagicMock()
        tool_result = MagicMock()
        tool_result.result_json = (
            '{"count": 1, "ok": 0, "failed": 1, "results": ['
            '{"status": "failed", "summary": "errore di build", "cost_usd": 0.0, '
            '"tokens": {}, "error": null}]}'
        )
        tool_runner.execute_tool = AsyncMock(return_value=tool_result)
        trn.configure(tool_runner=tool_runner)

        marks: list = []
        with patch.object(trn.todo_store, "list_todos", return_value=todos), \
             patch.object(trn, "_mark_todo_status", side_effect=lambda i, s: marks.append((i, s))):
            out = _run(trn.todo_runner_node({
                "plan_phase_active": True,
                "automation_mode": "continuous",
                "thread_id": "r1",
                "session_id": "s1",
                "subagent_results": [],
            }))

        self.assertIn(("t1", "blocked"), marks)
        self.assertEqual(out["stop_reason"], "end_turn")
        self.assertEqual(out["subagent_results"][0]["status"], "failed")

    def test_dispatch_fallito_fallback(self):
        # execute_tool solleva -> _dispatch_one ritorna None -> patch {} e
        # ripristino del todo a pending (fallback executor).
        todos = [
            {"id": "t1", "seq": 1, "content": "x", "status": "pending",
             "acceptance_criteria": [], "depends_on": []},
        ]
        tool_runner = MagicMock()
        tool_runner.execute_tool = AsyncMock(side_effect=RuntimeError("gRPC down"))
        trn.configure(tool_runner=tool_runner)

        marks: list = []
        with patch.object(trn.todo_store, "list_todos", return_value=todos), \
             patch.object(trn, "_mark_todo_status", side_effect=lambda i, s: marks.append((i, s))):
            out = _run(trn.todo_runner_node({
                "plan_phase_active": True,
                "automation_mode": "continuous",
                "thread_id": "r1",
                "session_id": "s1",
                "subagent_results": [],
            }))

        self.assertEqual(out, {})
        # Il todo e' stato ripristinato a pending.
        self.assertIn(("t1", "pending"), marks)


if __name__ == "__main__":
    unittest.main()
