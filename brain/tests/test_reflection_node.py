"""Test per la Fase 2 del sistema di self-reflection.

Copre:
  - reflection_rubric: build_reflection_prompt, parse_reflection_response, aggregate_score
  - reflection_config: lettura dal DB (mockato), accessori tipizzati
  - reflection_node: guard per feature flag, tag <reflection>, sampling, risposta LLM mockata
  - state: nuovi campi reflection_score, final_reward

Non richiede connessione DB o provider LLM reali (mock puri).
"""
from __future__ import annotations

import asyncio
import json
import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from brain.agents.reflection_rubric import (
    aggregate_score,
    build_reflection_prompt,
    parse_reflection_response,
)


# ─── Test rubrica ─────────────────────────────────────────────────────────────

class TestReflectionRubric(unittest.TestCase):

    def test_build_reflection_prompt_restituisce_tuple_non_vuota(self):
        sys_p, user_p = build_reflection_prompt(
            "fix bug login",
            "Ho corretto il file auth.ts aggiungendo il controllo del token.",
        )
        self.assertIsInstance(sys_p, str)
        self.assertIsInstance(user_p, str)
        self.assertGreater(len(sys_p), 10)
        self.assertGreater(len(user_p), 10)

    def test_build_reflection_prompt_contiene_task_e_output(self):
        _, user_p = build_reflection_prompt("task originale", "output agente")
        self.assertIn("task originale", user_p)
        self.assertIn("output agente", user_p)

    def test_build_reflection_prompt_tronca_input_troppo_lungo(self):
        # Caratteri rari che non compaiono nel template della rubrica
        task_lungo = "☃" * 5000    # simbolo pupazzo di neve
        output_lungo = "♥" * 6000  # simbolo cuore
        _, user_p = build_reflection_prompt(task_lungo, output_lungo)
        # Il task e' troncato a 2000, l'output a 3000
        self.assertLessEqual(user_p.count("☃"), 2000)
        self.assertLessEqual(user_p.count("♥"), 3000)

    def test_parse_reflection_response_json_valido(self):
        raw = json.dumps({
            "score": 0.85,
            "dimensions": {
                "correctness": 0.9,
                "completeness": 0.8,
                "efficiency": 0.85,
                "safety": 1.0,
            },
            "weaknesses": ["manca gestione errore"],
            "suggestions": ["aggiungere try/except"],
        })
        result = parse_reflection_response(raw)
        self.assertIsNotNone(result)
        self.assertAlmostEqual(result["score"], 0.85, places=2)
        self.assertIn("correctness", result["dimensions"])
        self.assertEqual(len(result["weaknesses"]), 1)
        self.assertEqual(len(result["suggestions"]), 1)

    def test_parse_reflection_response_json_immerso_in_testo(self):
        raw = 'Ecco la valutazione:\n' + json.dumps({
            "score": 0.72,
            "dimensions": {
                "correctness": 0.7,
                "completeness": 0.75,
                "efficiency": 0.7,
                "safety": 0.8,
            },
            "weaknesses": [],
            "suggestions": [],
        }) + '\nFine.'
        result = parse_reflection_response(raw)
        self.assertIsNotNone(result)
        self.assertAlmostEqual(result["score"], 0.72, places=2)

    def test_parse_reflection_response_score_fuori_range_restituisce_none(self):
        raw = json.dumps({
            "score": 1.5,  # invalido
            "dimensions": {
                "correctness": 0.9,
                "completeness": 0.8,
                "efficiency": 0.85,
                "safety": 1.0,
            },
            "weaknesses": [],
            "suggestions": [],
        })
        result = parse_reflection_response(raw)
        self.assertIsNone(result)

    def test_parse_reflection_response_testo_vuoto_restituisce_none(self):
        result = parse_reflection_response("")
        self.assertIsNone(result)

    def test_parse_reflection_response_testo_non_json_restituisce_none(self):
        result = parse_reflection_response("questo non e' JSON valido !")
        self.assertIsNone(result)

    def test_parse_reflection_response_limita_weaknesses_a_tre(self):
        raw = json.dumps({
            "score": 0.6,
            "dimensions": {
                "correctness": 0.6,
                "completeness": 0.6,
                "efficiency": 0.6,
                "safety": 0.6,
            },
            "weaknesses": ["w1", "w2", "w3", "w4", "w5"],
            "suggestions": ["s1", "s2", "s3", "s4"],
        })
        result = parse_reflection_response(raw)
        self.assertIsNotNone(result)
        self.assertLessEqual(len(result["weaknesses"]), 3)
        self.assertLessEqual(len(result["suggestions"]), 3)

    def test_aggregate_score_ponderazione_corretta(self):
        # correctness=1.0 (peso 0.40), completeness=1.0 (0.30),
        # efficiency=1.0 (0.15), safety=1.0 (0.15) -> totale 1.0
        dims = {"correctness": 1.0, "completeness": 1.0, "efficiency": 1.0, "safety": 1.0}
        self.assertAlmostEqual(aggregate_score(dims), 1.0, places=3)

    def test_aggregate_score_zero(self):
        dims = {"correctness": 0.0, "completeness": 0.0, "efficiency": 0.0, "safety": 0.0}
        self.assertAlmostEqual(aggregate_score(dims), 0.0, places=3)


# ─── Test reflection_node ─────────────────────────────────────────────────────

class TestReflectionNode(unittest.IsolatedAsyncioTestCase):

    def _stato_base(self, system_text: str = "<reflection>rubrica</reflection>") -> dict:
        from langchain_core.messages import HumanMessage
        return {
            "system_text": system_text,
            "result": "Ho risolto il bug correttamente.",
            "stop_reason": "end_turn",
            "iterations": 3,
            "thread_id": "test-thread-001",
            "profile_name": "coder",
            "provider_used": "anthropic",
            "model_used": "claude-3-5-sonnet-20241022",
            "messages": [HumanMessage(content="Correggi il bug nel login")],
        }

    async def test_skip_quando_feature_flag_disabilitato(self):
        """Quando reflection_enabled=false nel DB, il nodo salta."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        cfg_disabilitato = {
            "reflection_enabled": False,
            "reflection_sample_rate": 1.0,
            "reflection_timeout_s": 10.0,
            "reflection_model": "claude-3-5-haiku-20241022",
            "reflection_reward_weight": 0.3,
            "reflection_reasoning_bank_min_score": 0.85,
        }
        with patch.object(reflection_config, "get", return_value=cfg_disabilitato):
            result = await nodes_mod.reflection_node(self._stato_base())
        self.assertIsNone(result.get("reflection_score"))
        self.assertIsNone(result.get("final_reward"))

    async def test_skip_quando_tag_reflection_assente(self):
        from brain.agents.nodes import reflection_node
        stato = self._stato_base(system_text="<role>Coder</role>")
        result = await reflection_node(stato)
        self.assertIsNone(result.get("reflection_score"))

    async def test_skip_quando_result_vuoto(self):
        from brain.agents.nodes import reflection_node
        stato = self._stato_base()
        stato["result"] = ""
        result = await reflection_node(stato)
        self.assertIsNone(result.get("reflection_score"))

    async def test_skip_per_sampling(self):
        """Con sample_rate=0.0 nel DB il nodo deve sempre saltare."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        cfg_no_sample = {
            "reflection_enabled": True,
            "reflection_sample_rate": 0.0,
            "reflection_timeout_s": 10.0,
            "reflection_model": "claude-3-5-haiku-20241022",
            "reflection_reward_weight": 0.3,
            "reflection_reasoning_bank_min_score": 0.85,
        }
        with patch.object(reflection_config, "get", return_value=cfg_no_sample):
            result = await nodes_mod.reflection_node(self._stato_base())
        self.assertIsNone(result.get("reflection_score"))

    def _cfg_abilitato(self, **override) -> dict:
        """Configurazione di test con reflection abilitata e sample_rate=1.0."""
        base = {
            "reflection_enabled": True,
            "reflection_sample_rate": 1.0,
            "reflection_timeout_s": 10.0,
            "reflection_model": "claude-3-5-haiku-20241022",
            "reflection_reward_weight": 0.3,
            "reflection_reasoning_bank_min_score": 0.85,
        }
        base.update(override)
        return base

    async def test_esegue_con_sample_rate_uno(self):
        """Con sample_rate=1.0 nel DB e providers mockato, deve chiamare il LLM."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        mock_result = MagicMock()
        mock_result.content = json.dumps({
            "score": 0.88,
            "dimensions": {
                "correctness": 0.9,
                "completeness": 0.85,
                "efficiency": 0.9,
                "safety": 1.0,
            },
            "weaknesses": ["nessun commento nel codice"],
            "suggestions": ["aggiungere docstring"],
        })
        mock_prov = MagicMock()
        mock_prov.generate_completion_async = AsyncMock(return_value=mock_result)
        mock_providers = MagicMock()
        mock_providers._providers = {"anthropic": mock_prov}

        with (
            patch.object(reflection_config, "get", return_value=self._cfg_abilitato()),
            patch.object(nodes_mod, "_providers", mock_providers),
            patch.object(nodes_mod, "_persist_reflection", AsyncMock()),
        ):
            result = await nodes_mod.reflection_node(self._stato_base())

        self.assertIsNotNone(result.get("reflection_score"))
        self.assertAlmostEqual(result["reflection_score"], 0.88, places=2)
        self.assertIsNotNone(result.get("final_reward"))
        # final_reward = 0.7 * 1.0 (end_turn con result) + 0.3 * 0.88 = 0.964
        self.assertAlmostEqual(result["final_reward"], 0.7 * 1.0 + 0.3 * 0.88, places=3)

    async def test_reward_fuso_con_reflection_score(self):
        """Verifica la formula final_reward = (1-peso)*heuristic + peso*score."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        mock_result = MagicMock()
        mock_result.content = json.dumps({
            "score": 0.5,
            "dimensions": {
                "correctness": 0.5,
                "completeness": 0.5,
                "efficiency": 0.5,
                "safety": 0.5,
            },
            "weaknesses": [],
            "suggestions": [],
        })
        mock_prov = MagicMock()
        mock_prov.generate_completion_async = AsyncMock(return_value=mock_result)
        mock_providers = MagicMock()
        mock_providers._providers = {"anthropic": mock_prov}

        with (
            patch.object(reflection_config, "get", return_value=self._cfg_abilitato()),
            patch.object(nodes_mod, "_providers", mock_providers),
            patch.object(nodes_mod, "_persist_reflection", AsyncMock()),
        ):
            # stop_reason=end_turn, result non vuoto -> heuristic=1.0
            result = await nodes_mod.reflection_node(self._stato_base())

        expected_reward = 0.7 * 1.0 + 0.3 * 0.5
        self.assertAlmostEqual(result["final_reward"], expected_reward, places=3)

    async def test_peso_reward_personalizzato_da_db(self):
        """Verifica che il peso reward_weight sia letto dal DB."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        mock_result = MagicMock()
        mock_result.content = json.dumps({
            "score": 0.6,
            "dimensions": {
                "correctness": 0.6,
                "completeness": 0.6,
                "efficiency": 0.6,
                "safety": 0.6,
            },
            "weaknesses": [],
            "suggestions": [],
        })
        mock_prov = MagicMock()
        mock_prov.generate_completion_async = AsyncMock(return_value=mock_result)
        mock_providers = MagicMock()
        mock_providers._providers = {"anthropic": mock_prov}

        # Peso al 50% invece del 30%
        with (
            patch.object(reflection_config, "get", return_value=self._cfg_abilitato(reflection_reward_weight=0.5)),
            patch.object(nodes_mod, "_providers", mock_providers),
            patch.object(nodes_mod, "_persist_reflection", AsyncMock()),
        ):
            result = await nodes_mod.reflection_node(self._stato_base())

        # final_reward = 0.5 * 1.0 + 0.5 * 0.6 = 0.8
        self.assertAlmostEqual(result["final_reward"], 0.5 * 1.0 + 0.5 * 0.6, places=3)

    async def test_timeout_provider_restituisce_none_score(self):
        """Se il provider va in timeout, reflection_score deve essere None."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        async def _timeout_prov(*args, **kwargs):
            await asyncio.sleep(999)

        mock_prov = MagicMock()
        mock_prov.generate_completion_async = _timeout_prov
        mock_providers = MagicMock()
        mock_providers._providers = {"anthropic": mock_prov}

        with (
            patch.object(reflection_config, "get", return_value=self._cfg_abilitato(reflection_timeout_s=0.01)),
            patch.object(nodes_mod, "_providers", mock_providers),
        ):
            result = await nodes_mod.reflection_node(self._stato_base())

        self.assertIsNone(result.get("reflection_score"))
        self.assertIsNone(result.get("final_reward"))

    async def test_restituisce_weaknesses_e_suggestions(self):
        """Verifica che weaknesses e suggestions vengano propagati nello stato."""
        from brain.agents import nodes as nodes_mod
        from brain.agents import reflection_config

        mock_result = MagicMock()
        mock_result.content = json.dumps({
            "score": 0.91,
            "dimensions": {
                "correctness": 0.95,
                "completeness": 0.9,
                "efficiency": 0.85,
                "safety": 1.0,
            },
            "weaknesses": ["manca logging"],
            "suggestions": ["aggiungere tracing"],
        })
        mock_prov = MagicMock()
        mock_prov.generate_completion_async = AsyncMock(return_value=mock_result)
        mock_providers = MagicMock()
        mock_providers._providers = {"anthropic": mock_prov}

        with (
            patch.object(reflection_config, "get", return_value=self._cfg_abilitato()),
            patch.object(nodes_mod, "_providers", mock_providers),
            patch.object(nodes_mod, "_persist_reflection", AsyncMock()),
        ):
            result = await nodes_mod.reflection_node(self._stato_base())

        self.assertEqual(result.get("reflection_weaknesses"), ["manca logging"])
        self.assertEqual(result.get("reflection_suggestions"), ["aggiungere tracing"])


if __name__ == "__main__":
    unittest.main()
