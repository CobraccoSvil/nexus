"""Test del punto unico anti-contaminazione history (turn focus directive).

Verifica (funzioni pure, nessun DB):
  1. build_turn_focus_directive estrae l'ultima richiesta utente e produce il
     blocco di focus; "" se non c'e' un messaggio utente valido.
  2. new_topic=True aggiunge la riga di rinforzo (cambio d'argomento).
  3. L'estratto lungo viene troncato (la directive resta leggera/cacheabile).
  4. _inject_turn_focus mette il marcatore in testa, e' idempotente e non muta
     mai i messaggi (P3 prefix stabile per il KV-cache dei provider).
"""
import unittest

from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes.helpers import (
    _TURN_FOCUS_MARKER,
    _inject_turn_focus,
    build_turn_focus_directive,
)


class TestBuildTurnFocusDirective(unittest.TestCase):
    def test_empty_messages_returns_empty(self):
        self.assertEqual(build_turn_focus_directive([]), "")

    def test_no_human_message_returns_empty(self):
        msgs = [AIMessage(content="solo assistant")]
        self.assertEqual(build_turn_focus_directive(msgs), "")

    def test_extracts_last_user_request_not_old_task(self):
        # Caso reale: history su un vecchio task, nuovo turno diverso. La
        # directive deve ancorare il NUOVO task, non quello dominante in storia.
        msgs = [
            HumanMessage(content="task precedente: modifica bookingService.ts"),
            AIMessage(content="ho editato bookingService.ts"),
            HumanMessage(content="crea index.html nella root del progetto"),
        ]
        out = build_turn_focus_directive(msgs)
        self.assertIn("FOCUS DEL TURNO CORRENTE", out)
        self.assertIn("crea index.html nella root del progetto", out)
        self.assertNotIn("bookingService", out)

    def test_new_topic_adds_reinforcement(self):
        msgs = [HumanMessage(content="nuovo task")]
        base = build_turn_focus_directive(msgs, new_topic=False)
        reinforced = build_turn_focus_directive(msgs, new_topic=True)
        self.assertNotIn("cambio di argomento", base)
        self.assertIn("cambio di argomento", reinforced)

    def test_long_request_truncated(self):
        long_req = "x" * 1000
        msgs = [HumanMessage(content=long_req)]
        out = build_turn_focus_directive(msgs)
        self.assertIn("[...]", out)
        self.assertLess(out.count("x"), 1000)

    def test_idempotent(self):
        msgs = [HumanMessage(content="task ripetibile")]
        self.assertEqual(
            build_turn_focus_directive(msgs),
            build_turn_focus_directive(msgs),
        )


class TestInjectTurnFocus(unittest.TestCase):
    def test_empty_directive_is_noop(self):
        msgs = [HumanMessage(content="x")]
        out_msgs, out_sys = _inject_turn_focus(msgs, "SYSTEM", "")
        self.assertEqual(out_sys, "SYSTEM")
        self.assertIs(out_msgs, msgs)

    def test_injects_marker_at_head(self):
        msgs = [HumanMessage(content="x")]
        _, out_sys = _inject_turn_focus(msgs, "SYSTEM BASE", "DIRETTIVA")
        self.assertTrue(out_sys.startswith(_TURN_FOCUS_MARKER))
        self.assertIn("DIRETTIVA", out_sys)
        self.assertIn("SYSTEM BASE", out_sys)

    def test_idempotent_marker_present(self):
        msgs = [HumanMessage(content="x")]
        _, once = _inject_turn_focus(msgs, "BASE", "DIRETTIVA")
        _, twice = _inject_turn_focus(msgs, once, "DIRETTIVA-DIVERSA")
        # Marcatore gia' presente -> no-op: niente doppia iniezione.
        self.assertEqual(once, twice)
        self.assertEqual(twice.count(_TURN_FOCUS_MARKER), 1)

    def test_messages_never_mutated(self):
        msgs = [HumanMessage(content="x"), AIMessage(content="y")]
        snapshot = list(msgs)
        out_msgs, _ = _inject_turn_focus(msgs, "BASE", "DIRETTIVA")
        self.assertEqual(msgs, snapshot)
        self.assertIs(out_msgs, msgs)


if __name__ == "__main__":
    unittest.main()
