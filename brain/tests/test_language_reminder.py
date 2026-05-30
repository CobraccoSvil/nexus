"""Test per il reminder di lingua resiliente al contesto/profilo (bug #88).

Copre la funzione pura _inject_language_reminder e il loader cache
_load_language_reminder di brain/agents/nodes.py:

  1. reminder iniettato in system_text e nell'ultimo HumanMessage (enabled=True);
  2. nessuna modifica quando enabled=False;
  3. idempotenza: chiamare due volte non duplica;
  4. content come lista di blocchi: punto 2 saltato, punto 1 applicato;
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

    def test_inietta_system_e_ultimo_human(self):
        messages = [
            HumanMessage(content="primo task"),
            AIMessage(content="ok procedo"),
            HumanMessage(content="continua con lo step 2"),
        ]
        system_text = "Sei un agente."

        new_messages, new_system = _inject_language_reminder(
            messages, system_text, enabled=True, reminder_text=REMINDER
        )

        # Punto 1: system_text contiene marcatore + reminder.
        self.assertIn(_LANG_REMINDER_MARKER, new_system)
        self.assertIn(REMINDER, new_system)
        self.assertTrue(new_system.startswith("Sei un agente."))

        # Punto 2: solo l'ULTIMO HumanMessage modificato.
        self.assertIn(REMINDER, new_messages[2].content)
        self.assertNotIn(REMINDER, new_messages[0].content)
        # L'AIMessage intermedio resta intatto.
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
        # Seconda chiamata non duplica nel content dell'ultimo HumanMessage.
        self.assertEqual(msgs1[0].content, msgs2[0].content)
        self.assertEqual(msgs2[0].content.count(REMINDER), 1)

    def test_content_lista_blocchi_salta_punto2(self):
        # Ultimo HumanMessage con content non-stringa (lista di blocchi):
        # punto 2 saltato, punto 1 comunque applicato.
        blocks = [{"type": "text", "text": "vedi allegato"}]
        last = HumanMessage(content=blocks)
        messages = [HumanMessage(content="intro"), last]
        system_text = "Sei un agente."

        new_messages, new_system = _inject_language_reminder(
            messages, system_text, enabled=True, reminder_text=REMINDER
        )

        # Punto 1 applicato.
        self.assertIn(REMINDER, new_system)
        # Punto 2 saltato: nessun messaggio modificato (lista invariata).
        self.assertIs(new_messages, messages)
        self.assertEqual(new_messages[1].content, blocks)

    def test_nessun_human_message(self):
        # Solo AIMessage: punto 2 saltato senza errori, punto 1 applicato.
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
