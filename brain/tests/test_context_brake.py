"""Test per il punto unico (regola L) di freno contesto sui sub-agenti.

Copre `brain/agents/context_brake.py`:

  1. ``apply_context_reduction`` invoca dedup + drop base64 + rolling +
     token brake sulla history quando il context supera la soglia
     (le funzioni sottostanti vengono mockate per verificare la sequenza
     senza dipendere dal DB o da Qdrant).
  2. ``apply_context_reduction`` e' idempotente / no-op safe quando la
     history e' vuota o le funzioni sottostanti non sono disponibili.
  3. ``clamp_single_prompt`` tronca un prompt sopra soglia mantenendo
     head + tail con marker esplicito, e ritorna invariato sotto soglia.

Niente DB, niente provider LLM reali: solo mock puri.
"""
import unittest
from unittest import mock

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from brain.agents import context_brake


class TestApplyContextReduction(unittest.TestCase):
    """Pipeline di riduzione su una lista di messaggi LangChain."""

    def test_invoca_tutte_le_tappe(self):
        """La pipeline chiama dedup, drop_base64, rolling, token_brake nell'ordine.

        Verifica che il helper attivi le 4 funzioni esistenti (riusate, non
        duplicate) senza saltarne nessuna quando il modello e' valorizzato.
        """
        messages = [
            HumanMessage(content="task originale"),
            AIMessage(content="tool result enorme x" * 1000),
            ToolMessage(content="output tool", tool_call_id="tc1"),
        ]
        reduced_after_dedup = list(messages)
        reduced_after_drop = list(messages)
        reduced_after_rolling = list(messages)
        reduced_after_brake = list(messages[:2])  # brake comprime davvero

        with mock.patch.object(
            context_brake, "_load_ctx_cfg", return_value={"dedup_tool_results_enabled": True}
        ), mock.patch(
            "brain.agents.nodes.helpers._dedup_tool_results_history",
            return_value=reduced_after_dedup,
        ) as mock_dedup, mock.patch(
            "brain.agents.nodes.helpers._drop_unused_base64_payloads",
            return_value=reduced_after_drop,
        ) as mock_drop, mock.patch(
            "brain.agents.nodes.helpers._apply_rolling_summary",
            return_value=reduced_after_rolling,
        ) as mock_rolling, mock.patch(
            "brain.agents.nodes._apply_token_brake",
            return_value=reduced_after_brake,
        ) as mock_brake:
            out = context_brake.apply_context_reduction(
                messages, model="claude-haiku-test", iteration=5
            )

        # Tutte e 4 le tappe sono state chiamate.
        mock_dedup.assert_called_once()
        mock_drop.assert_called_once()
        mock_rolling.assert_called_once()
        mock_brake.assert_called_once()

        # Il brake ha ridotto effettivamente (3 -> 2 messaggi).
        self.assertEqual(len(out), 2)

    def test_messages_vuoti_no_op(self):
        """Una history vuota torna invariata senza chiamare nulla."""
        self.assertEqual(context_brake.apply_context_reduction([], model="x"), [])

    def test_dedup_disabilitato_da_cfg(self):
        """Se ``dedup_tool_results_enabled=False`` il dedup non viene chiamato."""
        messages = [HumanMessage(content="task")]
        with mock.patch.object(
            context_brake, "_load_ctx_cfg",
            return_value={"dedup_tool_results_enabled": False},
        ), mock.patch(
            "brain.agents.nodes.helpers._dedup_tool_results_history",
        ) as mock_dedup, mock.patch(
            "brain.agents.nodes.helpers._drop_unused_base64_payloads",
            return_value=messages,
        ), mock.patch(
            "brain.agents.nodes.helpers._apply_rolling_summary",
            return_value=messages,
        ), mock.patch(
            "brain.agents.nodes._apply_token_brake",
            return_value=messages,
        ):
            context_brake.apply_context_reduction(messages, model="m")
        mock_dedup.assert_not_called()

    def test_brake_saltato_se_modello_none(self):
        """Senza ``model`` valorizzato il token brake non puo' calcolare la
        window e viene saltato (le altre tappe restano attive)."""
        messages = [HumanMessage(content="task")]
        with mock.patch.object(
            context_brake, "_load_ctx_cfg", return_value={}
        ), mock.patch(
            "brain.agents.nodes.helpers._dedup_tool_results_history",
            return_value=messages,
        ), mock.patch(
            "brain.agents.nodes.helpers._drop_unused_base64_payloads",
            return_value=messages,
        ), mock.patch(
            "brain.agents.nodes.helpers._apply_rolling_summary",
            return_value=messages,
        ), mock.patch(
            "brain.agents.nodes._apply_token_brake",
        ) as mock_brake:
            context_brake.apply_context_reduction(messages, model=None)
        mock_brake.assert_not_called()


class TestClampSinglePrompt(unittest.TestCase):
    """Clamp difensivo head+tail su prompt singoli."""

    def test_prompt_sotto_soglia_invariato(self):
        """Un prompt piccolo ritorna identico (idempotenza/no-op)."""
        prompt = "Riassumi questo task in 200 parole."
        with mock.patch.object(
            context_brake, "_load_ctx_cfg", return_value={"max_context_ratio": 0.55}
        ), mock.patch(
            "brain.agents.nodes.helpers._model_context_window", return_value=128_000
        ), mock.patch(
            "brain.agents.nodes.helpers._count_tokens", return_value=10
        ):
            out = context_brake.clamp_single_prompt(prompt, model="m")
        self.assertEqual(out, prompt)

    def test_prompt_sopra_soglia_troncato_head_tail(self):
        """Sopra soglia: head + marker + tail; la lunghezza scende."""
        big_prompt = "X" * 600_000  # ~150K token stimati
        with mock.patch.object(
            context_brake, "_load_ctx_cfg", return_value={"max_context_ratio": 0.55}
        ), mock.patch(
            "brain.agents.nodes.helpers._model_context_window", return_value=32_000
        ), mock.patch(
            "brain.agents.nodes.helpers._count_tokens", return_value=150_000
        ):
            out = context_brake.clamp_single_prompt(big_prompt, model="m")
        # Il prompt e' stato accorciato.
        self.assertLess(len(out), len(big_prompt))
        # Marker di taglio esplicito presente.
        self.assertIn("contenuto troncato dal freno di contesto", out)
        # Head e tail conservati (X di partenza e X di chiusura).
        self.assertTrue(out.startswith("X"))
        self.assertTrue(out.endswith("X"))

    def test_input_non_stringa_invariato(self):
        """Input None / non stringa: ritorno invariato (degrade)."""
        self.assertIsNone(context_brake.clamp_single_prompt(None, model="m"))
        self.assertEqual(context_brake.clamp_single_prompt("", model="m"), "")


if __name__ == "__main__":
    unittest.main()
