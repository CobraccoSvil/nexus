#!/usr/bin/env python3
"""Genera il golden di parita' 1:1 per `build_turn_focus_directive` Rust.

Importa la funzione REALE dal brain
(`brain.agents.nodes.helpers.build_turn_focus_directive`) e la esercita su N>=15
casi rappresentativi (history vuota, un solo HumanMessage, multi-turn con
estrazione dell'ULTIMO human, blocchi di sistema da rimuovere, troncamento a 600
char, unicode, new_topic). Output: /tmp/golden_turn_focus.json — lista di
{case_id, messages, new_topic, output} consumata dal test Rust
`decisions::turn_focus::golden::golden_turn_focus_parita`.

Fallback (regola del task): se l'import del brain richiede dipendenze non
disponibili (langchain_core, ecc.), lo script usa una replica BYTE-FEDELE della
funzione (`_fallback_build`) + classi messaggio minimali, cosi' il golden si
genera comunque in modo deterministico e DB-free. La replica e' allineata 1:1 al
sorgente `helpers.py:866-927` (+ `task_playbook._user_text_only`).

Nota: i casi usano SOLO contenuti `str` sui messaggi (la forma `str(content)`
Python sui contenuti a blocchi non e' riproducibile 1:1 lato Rust ed e' fuori
contratto — vedi doc della funzione Rust).

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_turn_focus.py
  cargo test -p nexus-agent-graph --lib golden_turn_focus_parita -- --ignored
"""
from __future__ import annotations

import json
import os
import re
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)


# ── Tentativo di import REALE del brain ──────────────────────────────────────
_REAL = False
try:
    from langchain_core.messages import AIMessage, HumanMessage, ToolMessage  # type: ignore
    from brain.agents.nodes.helpers import build_turn_focus_directive as _real_build  # type: ignore

    _REAL = True
except Exception as _exc:  # noqa: BLE001
    print(f"[gen_golden_turn_focus] import brain non disponibile ({_exc}); uso replica byte-fedele")


# ── Replica byte-fedele (fallback) ───────────────────────────────────────────
# Classi messaggio minimali: build_turn_focus_directive filtra con
# isinstance(m, HumanMessage) e legge m.content.
class _Msg:
    def __init__(self, content: str) -> None:
        self.content = content


class _HumanMessage(_Msg):
    pass


class _AIMessage(_Msg):
    pass


class _ToolMessage(_Msg):
    pass


# `_SYSTEM_BLOCK_RE` (task_playbook.py:142-145), copiato 1:1.
_SYSTEM_BLOCK_RE = re.compile(
    r"<(allegati|allegati_sessione|task_playbook)[^>]*>.*?</\1>",
    re.DOTALL | re.IGNORECASE,
)


def _fb_user_text_only(text: str) -> str:
    return _SYSTEM_BLOCK_RE.sub("", text)


def _fallback_build(messages, new_topic: bool = False) -> str:
    """Replica 1:1 di helpers.py:866-927 (con _user_text_only inline)."""
    if not messages:
        return ""
    last_user = ""
    for m in reversed(messages):
        if isinstance(m, _HumanMessage):
            c = m.content if isinstance(m.content, str) else str(m.content)
            last_user = _fb_user_text_only(c)
            break
    last_user = (last_user or "").strip()
    if not last_user:
        return ""
    excerpt = last_user if len(last_user) <= 600 else last_user[:600].rstrip() + " [...]"
    lines = [
        "### FOCUS DEL TURNO CORRENTE ###",
        "La richiesta da portare a termine ADESSO e' l'ultimo messaggio "
        "dell'utente:",
        f"\"{excerpt}\"",
        "",
        "La cronologia precedente e' CONTESTO DI SUPPORTO, non l'oggetto di "
        "questa richiesta. Se il turno corrente riguarda un task diverso da "
        "quello discusso prima, segui il turno corrente e NON proseguire il "
        "lavoro precedente. Non dare per scontato che file, componenti o "
        "obiettivi citati nella cronologia siano l'oggetto di QUESTA richiesta, "
        "a meno che il turno corrente non li nomini esplicitamente.",
    ]
    if new_topic:
        lines.append(
            "NOTA: rilevato un cambio di argomento rispetto alla cronologia. "
            "Concentrati esclusivamente sulla richiesta corrente; ignora il "
            "lavoro precedente salvo quanto serve a soddisfarla."
        )
    return "\n".join(lines)


# ── Costruttori messaggio (reali se importati, altrimenti fallback) ──────────
def _mk(role: str, text: str):
    if _REAL:
        if role in ("user", "human"):
            return HumanMessage(content=text)
        if role in ("assistant", "ai"):
            return AIMessage(content=text)
        if role == "tool":
            return ToolMessage(content=text, tool_call_id="golden")
        raise ValueError(f"ruolo sconosciuto: {role}")
    if role in ("user", "human"):
        return _HumanMessage(text)
    if role in ("assistant", "ai"):
        return _AIMessage(text)
    if role == "tool":
        return _ToolMessage(text)
    raise ValueError(f"ruolo sconosciuto: {role}")


def _build(messages, new_topic: bool) -> str:
    if _REAL:
        return _real_build(messages, new_topic=new_topic)
    return _fallback_build(messages, new_topic=new_topic)


def main() -> None:
    # Ogni caso: (case_id, [(role, text), ...], new_topic).
    raw_cases = [
        # 1. History completamente vuota -> None/"".
        ("empty", [], False),
        # 2. Un solo messaggio AI, nessun human -> "".
        ("solo_ai", [("assistant", "ciao, come posso aiutarti?")], False),
        # 3. Un solo HumanMessage semplice.
        ("single_human", [("user", "crea un file index.html")], False),
        # 4. Multi-turn: deve estrarre l'ULTIMO human, non il primo.
        ("multi_turn_ultimo", [
            ("user", "lavora su bookingService.ts"),
            ("assistant", "fatto il booking service"),
            ("user", "ora crea index.html"),
        ], False),
        # 5. Multi-turn con tool in mezzo: ultimo human dopo un tool result.
        ("multi_turn_con_tool", [
            ("user", "prima richiesta"),
            ("assistant", "ok"),
            ("tool", "tool output qui"),
            ("user", "richiesta finale e definitiva"),
        ], False),
        # 6. Blocco <allegati_sessione> da rimuovere (incidente PL.make).
        ("allegati_sessione", [
            ("user", "<allegati_sessione>\nPL.make\n</allegati_sessione>quante tabelle nel db?"),
        ], False),
        # 7. Blocco <allegati> (singolare) con attributi.
        ("allegati_attr", [
            ("user", "<allegati count=\"2\">foo.pdf, bar.docx</allegati>analizza i documenti"),
        ], False),
        # 8. Blocco <task_playbook> da rimuovere.
        ("task_playbook_block", [
            ("user", "<task_playbook>guida figma</task_playbook>fai il login"),
        ], False),
        # 9. Solo blocchi di sistema -> dopo strip resta vuoto -> "".
        ("solo_blocchi", [
            ("user", "<allegati_sessione>solo questo</allegati_sessione>"),
        ], False),
        # 10. Whitespace puro -> "".
        ("solo_whitespace", [("user", "   \n\t  ")], False),
        # 11. Troncamento: richiesta esattamente 700 char -> 600 + " [...]".
        ("trunc_700", [("user", "x" * 700)], False),
        # 12. Esattamente 600 char -> NESSUN troncamento (<=600).
        ("len_600_no_trunc", [("user", "y" * 600)], False),
        # 13. 601 char -> troncamento al limite.
        ("len_601_trunc", [("user", "z" * 601)], False),
        # 14. Troncamento con whitespace a cavallo del taglio (rstrip dopo [:600]).
        ("trunc_rstrip", [("user", "a" * 595 + "     " + "coda")], False),
        # 15. Unicode (accenti/emoji): conteggio per code point, non byte.
        ("unicode", [("user", "crea una pagina con caffe' e citta' europee aeiou")], False),
        # 16. new_topic=True aggiunge la riga di rinforzo.
        ("new_topic_true", [("user", "task completamente nuovo")], True),
        # 17. new_topic=True su multi-turn.
        ("new_topic_multi", [
            ("user", "vecchio argomento"),
            ("assistant", "risposta"),
            ("user", "argomento nuovo e diverso"),
        ], True),
        # 18. Testo con virgolette interne (vanno nell'excerpt cosi' come sono).
        ("virgolette", [("user", 'aggiungi il testo "Benvenuto" alla home')], False),
        # 19. Testo con newline interni.
        ("multiline", [("user", "riga uno\nriga due\nriga tre")], False),
        # 20. Allegato + testo lungo che insieme superano 600 ma il testo utile no.
        ("allegati_poi_lungo", [
            ("user", "<allegati>" + "Z" * 800 + "</allegati>richiesta breve"),
        ], False),
    ]

    cases = []
    for case_id, msgs, new_topic in raw_cases:
        rt_messages = [_mk(role, text) for role, text in msgs]
        out = _build(rt_messages, new_topic)
        # Mappa "" Python -> null (None lato Rust).
        output = out if out != "" else None
        cases.append({
            "case_id": case_id,
            "messages": [{"role": role, "text": text} for role, text in msgs],
            "new_topic": new_topic,
            "output": output,
        })

    out_path = "/tmp/golden_turn_focus.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    src = "brain reale" if _REAL else "replica byte-fedele"
    print(f"golden turn_focus: {len(cases)} casi scritti in {out_path} (fonte: {src})")


if __name__ == "__main__":
    main()
