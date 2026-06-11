"""Test per il reminder di lingua resiliente al contesto/profilo (bug #88).

Copre la funzione pura _inject_language_reminder e il loader cache
_load_language_reminder di brain/agents/nodes.py:

  1. reminder iniettato nel system_text in TESTA e ribadito in CODA
     (enabled=True); i messaggi NON vengono toccati (P3 prefix stabile:
     mutare l'ultimo HumanMessage invalidava il KV-cache a ogni iterazione);
  2. nessuna modifica quando enabled=False;
  3. idempotenza: chiamare due volte non duplica;
  4. content come lista di blocchi: messaggi comunque invariati;
  5. default sicuri quando il DB e' irraggiungibile (loader settings mockato).

Mock puri: nessuna connessione DB o provider LLM reale.
"""
import unittest
from unittest import mock

from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes import (
    _LANG_REMINDER_DEFAULT_ENABLED,
    _LANG_REMINDER_DEFAULT_TEXT,
    _LANG_REMINDER_MARKER,
    _LANG_REMINDER_CACHE,
    _inject_language_reminder,
    _load_language_reminder,
)

REMINDER = "Rispondi SEMPRE e SOLO in italiano. Mai cinese."


class TestInjectLanguageReminder(unittest.TestCase):
    """Funzione pura _inject_language_reminder."""

    def test_inietta_system_testa_e_coda(self):
        messages = [
            HumanMessage(content="primo task"),
            AIMessage(content="ok procedo"),
            HumanMessage(content="continua con lo step 2"),
        ]
        system_text = "Sei un agente."

        new_messages, new_system = _inject_language_reminder(
            messages, system_text, enabled=True, reminder_text=REMINDER
        )

        # System: marcatore in testa, reminder ribadito in testa E in coda,
        # testo originale preservato nel mezzo.
        self.assertIn(_LANG_REMINDER_MARKER, new_system)
        self.assertTrue(new_system.startswith(_LANG_REMINDER_MARKER))
        self.assertEqual(new_system.count(REMINDER), 2)
        self.assertIn("Sei un agente.", new_system)

        # P3 prefix stabile: i messaggi NON vengono toccati.
        self.assertIs(new_messages, messages)
        self.assertNotIn(REMINDER, new_messages[2].content)
        self.assertEqual(new_messages[1].content, "ok procedo")

    def test_non_muta_originale(self):
        original_last = HumanMessage(content="task originale")
        messages = [original_last]
        _inject_language_reminder(
            messages, "sys", enabled=True, reminder_text=REMINDER
        )
        # L'oggetto originale nello stato condiviso non e' mutato.
        self.assertEqual(original_last.content, "task originale")
        self.assertEqual(messages[0].content, "task originale")

    def test_disabilitato_nessuna_modifica(self):
        messages = [HumanMessage(content="task")]
        system_text = "Sei un agente."

        new_messages, new_system = _inject_language_reminder(
            messages, system_text, enabled=False, reminder_text=REMINDER
        )

        self.assertEqual(new_system, system_text)
        self.assertIs(new_messages, messages)
        self.assertNotIn(REMINDER, new_messages[0].content)

    def test_idempotenza(self):
        messages = [HumanMessage(content="task")]
        system_text = "Sei un agente."

        msgs1, sys1 = _inject_language_reminder(
            messages, system_text, enabled=True, reminder_text=REMINDER
        )
        msgs2, sys2 = _inject_language_reminder(
            msgs1, sys1, enabled=True, reminder_text=REMINDER
        )

        # Seconda chiamata non duplica nel system.
        self.assertEqual(sys1, sys2)
        self.assertEqual(sys2.count(_LANG_REMINDER_MARKER), 1)
        # I messaggi restano sempre invariati (P3 prefix stabile).
        self.assertIs(msgs2, msgs1)
        self.assertEqual(msgs2[0].content, "task")
        self.assertNotIn(REMINDER, msgs2[0].content)

    def test_content_lista_blocchi_messaggi_invariati(self):
        # Ultimo HumanMessage con content non-stringa (lista di blocchi):
        # system iniettato, messaggi comunque invariati.
        blocks = [{"type": "text", "text": "vedi allegato"}]
        last = HumanMessage(content=blocks)
        messages = [HumanMessage(content="intro"), last]
        system_text = "Sei un agente."

        new_messages, new_system = _inject_language_reminder(
            messages, system_text, enabled=True, reminder_text=REMINDER
        )

        self.assertIn(REMINDER, new_system)
        self.assertIs(new_messages, messages)
        self.assertEqual(new_messages[1].content, blocks)

    def test_nessun_human_message(self):
        # Solo AIMessage: nessun errore, system comunque iniettato.
        messages = [AIMessage(content="solo assistant")]
        new_messages, new_system = _inject_language_reminder(
            messages, "sys", enabled=True, reminder_text=REMINDER
        )
        self.assertIn(REMINDER, new_system)
        self.assertIs(new_messages, messages)


class TestLoadLanguageReminder(unittest.TestCase):
    """Loader cache _load_language_reminder con default sicuri."""

    def setUp(self):
        # Reset cache prima di ogni test per isolamento.
        _LANG_REMINDER_CACHE["loaded_at"] = 0.0
        _LANG_REMINDER_CACHE["enabled"] = None
        _LANG_REMINDER_CACHE["text"] = None

    def tearDown(self):
        _LANG_REMINDER_CACHE["loaded_at"] = 0.0
        _LANG_REMINDER_CACHE["enabled"] = None
        _LANG_REMINDER_CACHE["text"] = None

    def test_default_sicuri_db_down(self):
        # get_setting/get_bool_setting non sollevano: simuliamo DB down
        # facendole tornare i default passati (comportamento reale del modulo).
        def fake_bool(key, default=False):
            return default

        def fake_get(key, default=""):
            return default

        with mock.patch("brain.utils.settings_db.get_bool_setting", fake_bool), \
             mock.patch("brain.utils.settings_db.get_setting", fake_get):
            enabled, text = _load_language_reminder()

        self.assertEqual(enabled, _LANG_REMINDER_DEFAULT_ENABLED)
        self.assertEqual(text, _LANG_REMINDER_DEFAULT_TEXT)

    def test_default_su_eccezione_loader(self):
        # Se il loader settings solleva (caso estremo), default sicuri.
        with mock.patch(
            "brain.utils.settings_db.get_bool_setting",
            side_effect=RuntimeError("DB irraggiungibile"),
        ):
            enabled, text = _load_language_reminder()

        self.assertEqual(enabled, _LANG_REMINDER_DEFAULT_ENABLED)
        self.assertEqual(text, _LANG_REMINDER_DEFAULT_TEXT)

    def test_valori_da_db(self):
        with mock.patch(
            "brain.utils.settings_db.get_bool_setting", return_value=False
        ), mock.patch(
            "brain.utils.settings_db.get_setting", return_value="Solo italiano custom."
        ):
            enabled, text = _load_language_reminder()

        self.assertFalse(enabled)
        self.assertEqual(text, "Solo italiano custom.")


if __name__ == "__main__":
    unittest.main()
