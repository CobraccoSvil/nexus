"""Test PR-3 sub-agents pattern.

Copre:
  - subagent_store: helpers SQL mockati
  - subagent_dispatch_node._filter_tools_by_whitelist
  - subagent_dispatch_node._extract_final_text + _extract_artifacts
  - subagent_dispatch_node._compact_summary
  - run_subagent happy path con agent_graph mockato
  - run_subagent: kind sconosciuto, prompt mancante, timeout

Tutti mock puri.
"""
from __future__ import annotations

import asyncio
import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from langchain_core.messages import AIMessage, HumanMessage


def _run(coro):
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


# ─── Helpers ─────────────────────────────────────────────────────────────────


class TestSubagentHelpers(unittest.TestCase):

    def test_filter_tools_by_whitelist_costruisce_schema(self):
        from brain.agents.subagent_dispatch_node import _filter_tools_by_whitelist
        out = _filter_tools_by_whitelist(["list_files", "read_file", "magic_tool"])
        names = [t["name"] for t in out]
        self.assertEqual(names, ["list_files", "read_file", "magic_tool"])
        # Tutti hanno schema valido
        for t in out:
            self.assertIn("input_schema", t)
            self.assertEqual(t["input_schema"]["type"], "object")

    def test_filter_tools_empty_ritorna_vuoto(self):
        from brain.agents.subagent_dispatch_node import _filter_tools_by_whitelist
        self.assertEqual(_filter_tools_by_whitelist([]), [])

    def test_extract_final_text_prende_ultimo_message(self):
        from brain.agents.subagent_dispatch_node import _extract_final_text
        state = {
            "messages": [
                HumanMessage(content="task originale"),
                AIMessage(content="risposta intermedia"),
                AIMessage(content="risposta finale dell'agente"),
            ]
        }
        self.assertEqual(_extract_final_text(state), "risposta finale dell'agente")

    def test_extract_final_text_skip_errori(self):
        from brain.agents.subagent_dispatch_node import _extract_final_text
        state = {
            "messages": [
                AIMessage(content="risposta utile"),
                AIMessage(content="[Errore provider xxx]"),
            ]
        }
        self.assertEqual(_extract_final_text(state), "risposta utile")

    def test_compact_summary_tronca_max_chars(self):
        from brain.agents.subagent_dispatch_node import _compact_summary
        long = "x" * 1000
        out = _compact_summary(long, max_chars=100)
        self.assertEqual(len(out), 100)
        self.assertTrue(out.endswith("...[truncated]"))

    def test_compact_summary_short_no_op(self):
        from brain.agents.subagent_dispatch_node import _compact_summary
        s = "breve"
        self.assertEqual(_compact_summary(s, max_chars=100), s)

    def test_extract_artifacts_da_tool_result(self):
        from brain.agents.subagent_dispatch_node import _extract_artifacts
        msg = HumanMessage(content="")
        msg.additional_kwargs = {
            "anthropic_content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "x",
                    "content": "File 'src/a.ts' scritto. File backend/b.py modificato.",
                }
            ]
        }
        state = {"messages": [msg]}
        out = _extract_artifacts(state)
        self.assertIn("src/a.ts", out)
        self.assertIn("backend/b.py", out)


# ─── run_subagent ───────────────────────────────────────────────────────────


class TestRunSubagent(unittest.TestCase):

    def test_kind_sconosciuto(self):
        from brain.agents import subagent_dispatch_node as sdn
        with patch.object(sdn.subagent_store, "fetch_definition", return_value=None):
            out = _run(sdn.run_subagent(
                subagent_run_id="r1", parent_run_id="p", project_id="pj",
                user_id="u", session_id="s", kind="missing", task="x",
                agent_graph=MagicMock(),
            ))
        self.assertEqual(out["status"], "failed")
        self.assertIn("non trovato", out["summary"])

    def test_prompt_mancante(self):
        from brain.agents import subagent_dispatch_node as sdn
        defn = {
            "kind": "plan", "prompt_key": "missing.prompt",
            "tool_whitelist": ["read_file"], "model_purpose": "planner",
            "max_iterations": 25, "timeout_s": 60, "is_background": False,
        }
        with patch.object(sdn.subagent_store, "fetch_definition", return_value=defn), \
             patch.object(sdn.prompt_registry, "get_prompt", return_value=""):
            out = _run(sdn.run_subagent(
                subagent_run_id="r1", parent_run_id="p", project_id="pj",
                user_id="u", session_id="s", kind="plan", task="x",
                agent_graph=MagicMock(),
            ))
        self.assertEqual(out["status"], "failed")
        self.assertIn("prompt", out["summary"])

    def test_happy_path(self):
        from brain.agents import subagent_dispatch_node as sdn
        defn = {
            "kind": "explore", "prompt_key": "subagent.explore.base",
            "tool_whitelist": ["read_file", "list_files"], "model_purpose": "explorer",
            "max_iterations": 20, "timeout_s": 60, "is_background": False,
        }
        # Risultato finale del grafo
        final_state = {
            "messages": [
                HumanMessage(content="task"),
                AIMessage(content="riepilogo dell esplorazione: trovato in src/a.ts:42"),
            ],
            "iterations": 7,
            "prompt_tokens": 1500,
            "completion_tokens": 200,
            "total_cost_usd": 0.012,
        }
        graph = MagicMock()
        graph.ainvoke = AsyncMock(return_value=final_state)
        with patch.object(sdn.subagent_store, "fetch_definition", return_value=defn), \
             patch.object(sdn.prompt_registry, "get_prompt", return_value="<role>explorer</role>"), \
             patch.object(sdn.subagent_store, "update_run_start"), \
             patch.object(sdn.subagent_store, "update_run_completion") as m_complete:
            out = _run(sdn.run_subagent(
                subagent_run_id="r1", parent_run_id="p", project_id="pj",
                user_id="u", session_id="s", kind="explore",
                task="trova auth bug", expected_format="200 char",
                agent_graph=graph,
            ))
        self.assertEqual(out["status"], "completed")
        self.assertIn("riepilogo", out["summary"])
        self.assertEqual(out["iterations"], 7)
        self.assertEqual(out["cost_usd"], 0.012)
        # update_run_completion chiamata con i valori giusti
        m_complete.assert_called_once()
        _, kwargs = m_complete.call_args
        self.assertEqual(kwargs["status"], "completed")
        self.assertEqual(kwargs["iterations"], 7)

    def test_timeout(self):
        from brain.agents import subagent_dispatch_node as sdn
        defn = {
            "kind": "implement", "prompt_key": "subagent.implement.base",
            "tool_whitelist": ["read_file", "write_file"], "model_purpose": "planner",
            "max_iterations": 30, "timeout_s": 0.01,  # ~immediato
            "is_background": False,
        }
        async def _never_ending(*_a, **_k):
            await asyncio.sleep(10)
            return {}
        graph = MagicMock()
        graph.ainvoke = _never_ending
        with patch.object(sdn.subagent_store, "fetch_definition", return_value=defn), \
             patch.object(sdn.prompt_registry, "get_prompt", return_value="x"), \
             patch.object(sdn.subagent_store, "update_run_start"), \
             patch.object(sdn.subagent_store, "update_run_completion") as m_complete:
            out = _run(sdn.run_subagent(
                subagent_run_id="r1", parent_run_id="p", project_id="pj",
                user_id="u", session_id="s", kind="implement", task="x",
                agent_graph=graph,
            ))
        self.assertEqual(out["status"], "timeout")
        # Persistito come timeout
        m_complete.assert_called_once()
        _, kwargs = m_complete.call_args
        self.assertEqual(kwargs["status"], "timeout")


if __name__ == "__main__":
    unittest.main()
