"""Test per i due fix anti-loop su scaffolding applicativo da allegato.

Copre:
  1. _detect_scaffolding_intent: override deterministico intent "architecture"
     (famiglia VERBO_di_creazione + OGGETTO_applicativo), robusto con
     apostrofi/articoli, immune al token "file".
  2. Loop-detection SEMANTICA: contatore consecutive_exploration_calls,
     classificazione tool esplorativi vs produttivi, soglia DB-driven.

Mock puri: nessuna connessione DB o provider LLM reale.
"""
import unittest

from brain.agents.nodes import _EXPLORATION_ONLY_TOOLS

# NOTA: i test su _detect_scaffolding_intent sono stati RIMOSSI insieme alla
# funzione. Lo scaffolding ("crea un'app per X" -> architecture) e' ora
# interpretato semanticamente dal classifier LLM (prompt in
# brain/router/agentic_classifier.py), non piu' da regex su verbi+oggetti.


class TestExplorationToolSet(unittest.TestCase):
    """Verifica la composizione del set di tool esplorativi."""

    def test_tool_esplorativi_inclusi(self):
        attesi = {
            "nexus_list_archive_entries",
            "nexus_read_archive_entry",
            "nexus_inspect_attachment",
            "nexus_extract_figma_structure",
            "nexus_list_attachments",
            "nexus_read_attachment",
            "read_file",
        }
        self.assertTrue(attesi.issubset(_EXPLORATION_ONLY_TOOLS))

    def test_tool_produttivi_esclusi(self):
        produttivi = {"write_file", "edit_file", "run_command", "request_port"}
        self.assertTrue(produttivi.isdisjoint(_EXPLORATION_ONLY_TOOLS))


class TestExplorationLoopCounter(unittest.TestCase):
    """FIX 2: logica di conteggio della loop-detection semantica.

    Riproduce la stessa logica applicata in executor_node (aggiornamento del
    contatore a valle di pending_tool_uses) per validarla in isolamento, senza
    dipendere dall'intero nodo (che richiede provider/router).
    """

    @staticmethod
    def _update(count: int, pending_names: list[str]) -> int:
        """Replica dell'aggiornamento contatore in executor_node."""
        if not pending_names:
            return count
        if all(n in _EXPLORATION_ONLY_TOOLS for n in pending_names):
            return count + len(pending_names)
        return 0

    def test_accumulo_su_call_esplorative(self):
        count = 0
        count = self._update(count, ["nexus_list_archive_entries"])
        self.assertEqual(count, 1)
        count = self._update(count, ["nexus_read_archive_entry", "nexus_inspect_attachment"])
        self.assertEqual(count, 3)
        count = self._update(count, ["nexus_extract_figma_structure"])
        self.assertEqual(count, 4)

    def test_call_produttiva_azzera(self):
        count = 5
        count = self._update(count, ["write_file"])
        self.assertEqual(count, 0)

    def test_mix_con_produttiva_azzera(self):
        count = 5
        # Anche una sola produttiva nel batch azzera (il modello sta scrivendo).
        count = self._update(count, ["nexus_read_attachment", "write_file"])
        self.assertEqual(count, 0)

    def test_nessuna_call_mantiene_contatore(self):
        count = 4
        count = self._update(count, [])
        self.assertEqual(count, 4)

    def test_soglia_nudge_e_abort(self):
        soglia = 6
        # Simula N call esplorative consecutive -> raggiunge soglia (nudge).
        count = 0
        for _ in range(soglia):
            count = self._update(count, ["nexus_read_archive_entry"])
        self.assertGreaterEqual(count, soglia, "nudge atteso a soglia")
        # Continua a esplorare ignorando il nudge -> raggiunge 2x soglia (abort).
        for _ in range(soglia):
            count = self._update(count, ["nexus_read_archive_entry"])
        self.assertGreaterEqual(count, 2 * soglia, "abort atteso a 2x soglia")
        # Una call produttiva azzera tutto.
        count = self._update(count, ["request_port"])
        self.assertEqual(count, 0)


if __name__ == "__main__":
    unittest.main()
