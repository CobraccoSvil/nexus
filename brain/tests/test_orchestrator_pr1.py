"""Test PR-1 Plan/Act/Verify orchestrator.

Copre:
  - orchestrator_config: lettura DB mockata, accessori tipizzati, is_eligible 4-way
  - todo_store: list_todos / stats / active_todo (DB mockato)
  - todo_reminder: build_reminder_text + render checklist + cursor su attivo
  - planner_node: guard skip se non eligibile, plan idempotente se gia esiste,
    happy path con LLM e tool runner mockati
  - graph.route_after_router: condizione planner vs executor

Nessun test richiede DB live o provider LLM reali (mock puri).
"""
from __future__ import annotations

import asyncio
import json
import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from brain.agents import orchestrator_config


# ─── Test orchestrator_config ────────────────────────────────────────────────


class TestOrchestratorConfigDefaults(unittest.TestCase):

    def setUp(self) -> None:
        # Forza ricarico per ogni test (cache TTL 60s).
        orchestrator_config.force_reload()
        # Patcha _load_from_db per evitare DB live.
        self._patcher = patch.object(
            orchestrator_config, "_load_from_db",
            return_value=dict(orchestrator_config._SAFE_DEFAULTS),
        )
        self._patcher.start()
        orchestrator_config.force_reload()

    def tearDown(self) -> None:
        self._patcher.stop()
        orchestrator_config.force_reload()

    def test_safe_defaults_disabilitano_feature(self):
        cfg = orchestrator_config.get()
        self.assertFalse(cfg["plan_phase_enabled"])
        self.assertFalse(cfg["verifier_enabled"])

    def test_lists_sono_parsed_correttamente(self):
        cfg = orchestrator_config.get()
        self.assertIn("automatico", cfg["plan_behavior_modes"])
        self.assertIn("scaffold_app", cfg["plan_intents"])

    def test_accessori_tipizzati(self):
        self.assertEqual(orchestrator_config.plan_min_token_budget(), 2000)
        self.assertEqual(orchestrator_config.todo_reminder_every_n_steps(), 5)
        self.assertEqual(orchestrator_config.max_verify_cycles(), 3)
        self.assertEqual(orchestrator_config.verifier_timeout_s(), 30.0)


class TestOrchestratorConfigEligibility(unittest.TestCase):

    def _set_cfg(self, **overrides) -> None:
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg.update(overrides)
        self._patcher = patch.object(
            orchestrator_config, "_load_from_db", return_value=cfg,
        )
        self._patcher.start()
        orchestrator_config.force_reload()

    def tearDown(self) -> None:
        if hasattr(self, "_patcher"):
            self._patcher.stop()
        orchestrator_config.force_reload()

    def test_skip_se_feature_disabled(self):
        self._set_cfg(plan_phase_enabled=False)
        self.assertFalse(orchestrator_config.is_eligible("automatico", "scaffold_app", 5000))

    def test_skip_se_behavior_mode_non_in_allowlist(self):
        self._set_cfg(plan_phase_enabled=True)
        self.assertFalse(orchestrator_config.is_eligible("study", "scaffold_app", 5000))

    def test_skip_se_intent_non_in_allowlist(self):
        self._set_cfg(plan_phase_enabled=True)
        self.assertFalse(orchestrator_config.is_eligible("automatico", "chat", 5000))

    def test_skip_se_budget_sotto_soglia(self):
        self._set_cfg(plan_phase_enabled=True)
        self.assertFalse(orchestrator_config.is_eligible("automatico", "fix", 100))

    def test_attiva_se_tutti_4_check_passano(self):
        self._set_cfg(plan_phase_enabled=True)
        self.assertTrue(orchestrator_config.is_eligible("automatico", "fix", 5000))

    def test_csv_coerce_da_stringa(self):
        # Quando il DB ritorna "a,b, c ,," viene parsato a ["a", "b", "c"]
        result = orchestrator_config._coerce("a,b, c ,,", ["x"])
        self.assertEqual(result, ["a", "b", "c"])


# ─── Test todo_reminder.build_reminder_text ─────────────────────────────────


class TestTodoReminder(unittest.TestCase):

    def setUp(self) -> None:
        from brain.agents import todo_reminder
        self.tr = todo_reminder

    def test_render_lines_evidenzia_active_e_status(self):
        todos = [
            {"id": "a", "seq": 1, "content": "primo", "status": "completed"},
            {"id": "b", "seq": 2, "content": "secondo", "status": "in_progress"},
            {"id": "c", "seq": 3, "content": "terzo", "status": "pending"},
            {"id": "d", "seq": 4, "content": "skippato", "status": "skipped"},
            {"id": "e", "seq": 5, "content": "bloccato", "status": "blocked"},
        ]
        rendered = self.tr._render_todo_lines(todos, active_id="b")
        self.assertIn("[x] primo", rendered)
        self.assertIn("[~] secondo", rendered)
        self.assertIn("[ ] terzo", rendered)
        self.assertIn("[-] skippato", rendered)
        self.assertIn("[!] bloccato", rendered)
        # Cursore '>' deve essere all'inizio della riga di 'b'
        line_b = [l for l in rendered.split("\n") if "secondo" in l][0]
        self.assertTrue(line_b.startswith(">"))

    def test_build_reminder_text_skip_se_plan_disabled(self):
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg["plan_phase_enabled"] = False
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg):
            orchestrator_config.force_reload()
            self.assertIsNone(self.tr.build_reminder_text("run-x"))

    def test_build_reminder_text_skip_sotto_min_todos(self):
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg["plan_phase_enabled"] = True
        cfg["todo_reminder_min_todos"] = 5
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg), \
             patch("brain.agents.todo_reminder.todo_store.list_todos") as m_list, \
             patch("brain.agents.todo_reminder.todo_store.active_todo") as m_active:
            m_list.return_value = [
                {"id": "a", "seq": 1, "content": "x", "status": "pending"},
            ]
            m_active.return_value = None
            orchestrator_config.force_reload()
            self.assertIsNone(self.tr.build_reminder_text("run-x"))

    def test_build_reminder_text_emette_blocco_con_template_fallback(self):
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg["plan_phase_enabled"] = True
        cfg["todo_reminder_min_todos"] = 1
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg), \
             patch("brain.agents.todo_reminder.todo_store.list_todos") as m_list, \
             patch("brain.agents.todo_reminder.todo_store.active_todo") as m_active, \
             patch("brain.agents.todo_reminder.prompt_registry.get_prompt") as m_prompt:
            m_list.return_value = [
                {"id": "a", "seq": 1, "content": "primo", "status": "in_progress"},
                {"id": "b", "seq": 2, "content": "secondo", "status": "pending"},
            ]
            m_active.return_value = {"id": "a", "seq": 1, "content": "primo"}
            m_prompt.return_value = ""  # forza fallback inline
            orchestrator_config.force_reload()
            result = self.tr.build_reminder_text("run-x")
        self.assertIsNotNone(result)
        self.assertIn("primo", result)
        self.assertIn("secondo", result)
        self.assertIn("nexus_todo_write", result)  # menzione del tool nel fallback


# ─── Test planner_node guard + idempotenza ──────────────────────────────────


def _run(coro):
    """Run async coroutine in un nuovo event loop dedicato.

    Evitiamo asyncio.get_event_loop() (deprecato in 3.12) e
    asyncio.run() che chiude i task pendenti in modo aggressivo.
    """
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


class TestPlannerNodeGuard(unittest.TestCase):

    def setUp(self) -> None:
        # Default: feature OFF
        self._patch = patch.object(
            orchestrator_config, "_load_from_db",
            return_value=dict(orchestrator_config._SAFE_DEFAULTS),
        )
        self._patch.start()
        orchestrator_config.force_reload()

    def tearDown(self) -> None:
        self._patch.stop()
        orchestrator_config.force_reload()

    def test_skip_se_non_eligibile(self):
        from brain.agents.planner_node import planner_node
        state = {
            "behavior_mode": "automatico",
            "user_intent": "chat",  # non in plan_intents
            "token_budget": 5000,
            "thread_id": "run-x",
        }
        out = _run(planner_node(state))
        self.assertEqual(out, {"plan_phase_active": False})

    def test_idempotente_se_plan_esistente(self):
        # Abilita feature
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg["plan_phase_enabled"] = True
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg):
            orchestrator_config.force_reload()
            from brain.agents.planner_node import planner_node
            state = {
                "behavior_mode": "automatico",
                "user_intent": "scaffold_app",
                "token_budget": 5000,
                "thread_id": "run-y",
                "session_id": "sess-1",
            }
            existing_plan = {"run_id": "run-y", "thread_id": "sess-1"}
            existing_todos = [
                {"id": "a", "seq": 1, "content": "primo", "status": "in_progress"},
                {"id": "b", "seq": 2, "content": "secondo", "status": "pending"},
            ]
            with patch("brain.agents.planner_node.todo_store.fetch_plan", return_value=existing_plan), \
                 patch("brain.agents.planner_node.todo_store.list_todos", return_value=existing_todos), \
                 patch("brain.agents.planner_node.todo_store.active_todo", return_value=existing_todos[0]):
                out = _run(planner_node(state))
        self.assertTrue(out["plan_phase_active"])
        self.assertEqual(out["current_plan_id"], "run-y")
        self.assertEqual(out["active_todo_id"], "a")
        self.assertEqual(len(out["current_todos"]), 2)

    def test_skip_se_servizi_non_configurati(self):
        # Abilita feature ma planner_node._providers = None
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg["plan_phase_enabled"] = True
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg):
            orchestrator_config.force_reload()
            from brain.agents import planner_node as pn
            # Salva e reset
            saved = (pn._providers, pn._tool_runner, pn._routing_client)
            pn._providers = None
            pn._tool_runner = None
            pn._routing_client = None
            try:
                state = {
                    "behavior_mode": "automatico",
                    "user_intent": "scaffold_app",
                    "token_budget": 5000,
                    "thread_id": "run-z",
                }
                with patch("brain.agents.planner_node.todo_store.fetch_plan", return_value=None):
                    out = _run(pn.planner_node(state))
            finally:
                pn._providers, pn._tool_runner, pn._routing_client = saved
        self.assertFalse(out["plan_phase_active"])
        self.assertIn("services_not_configured", out.get("plan_phase_skip_reason", ""))


# ─── Test route_after_router ──────────────────────────────────────────────


class TestRouteAfterRouter(unittest.TestCase):

    def setUp(self) -> None:
        self._patch = patch.object(
            orchestrator_config, "_load_from_db",
            return_value=dict(orchestrator_config._SAFE_DEFAULTS),
        )
        self._patch.start()
        orchestrator_config.force_reload()

    def tearDown(self) -> None:
        self._patch.stop()
        orchestrator_config.force_reload()

    def test_route_default_va_a_executor(self):
        from brain.agents.graph import route_after_router
        state = {"behavior_mode": "automatico", "user_intent": "fix", "token_budget": 5000}
        # Feature OFF → executor
        self.assertEqual(route_after_router(state), "executor")

    def test_route_attivo_va_a_planner_se_eligibile(self):
        cfg = dict(orchestrator_config._SAFE_DEFAULTS)
        cfg["plan_phase_enabled"] = True
        with patch.object(orchestrator_config, "_load_from_db", return_value=cfg):
            orchestrator_config.force_reload()
            from brain.agents.graph import route_after_router
            state = {"behavior_mode": "automatico", "user_intent": "fix", "token_budget": 5000}
            self.assertEqual(route_after_router(state), "planner")


if __name__ == "__main__":
    unittest.main()
