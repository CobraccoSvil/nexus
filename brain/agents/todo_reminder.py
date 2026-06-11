"""Render del blocco <todo_list> per system-reminder injection (PR-1).

Pattern Claude Code: dopo ogni N tool use (configurabile via DB), il
sistema appende un blocco compatto con lo stato della TODO list al
tool_result HumanMessage. Cosi' il modello vede sempre lo stato fresco
e non perde traccia degli obiettivi in run lunghi.

Il blocco NON viene iniettato come system_message separato perche'
Anthropic accetta solo system al primo turno; usiamo un blocco text
appeso al HumanMessage che contiene i tool_result del round.
"""
from __future__ import annotations

import logging
from typing import Any

from . import orchestrator_config, prompt_registry, todo_store

logger = logging.getLogger(__name__)


def _render_todo_lines(todos: list[dict[str, Any]], active_id: str | None) -> str:
    """Render della checklist con prefix per status e cursore sul todo attivo."""
    if not todos:
        return "(nessun todo)"
    lines: list[str] = []
    cursor_glyph = ">"  # ASCII, niente emoji (CLAUDE.md)
    for t in todos:
        seq = t.get("seq", "?")
        content = t.get("content", "").strip()
        status = t.get("status", "pending")
        if status == "completed":
            box = "[x]"
        elif status == "in_progress":
            box = "[~]"
        elif status == "blocked":
            box = "[!]"
        elif status == "skipped":
            box = "[-]"
        else:
            box = "[ ]"
        prefix = cursor_glyph if (active_id and t.get("id") == active_id) else " "
        lines.append(f"{prefix} {seq}. {box} {content}")
    return "\n".join(lines)


def build_reminder_text(run_id: str) -> str | None:
    """Costruisce il blocco testo del reminder per il run_id.

    Ritorna None se:
    - plan_phase_enabled = False
    - nessun todo per run_id
    - todos pending sotto la soglia todo_reminder_min_todos
    """
    cfg = orchestrator_config.get()
    if not cfg["plan_phase_enabled"]:
        return None

    todos = todo_store.list_todos(run_id)
    if not todos:
        return None

    pending = [t for t in todos if t.get("status") in ("pending", "in_progress")]
    if len(pending) < int(cfg["todo_reminder_min_todos"]):
        return None

    active = todo_store.active_todo(run_id)
    active_id = active.get("id") if active else None
    active_seq = active.get("seq") if active else None
    active_content = active.get("content") if active else ""

    # Render via prompt_registry (template agent.todo_reminder.tpl) con fallback inline
    template = prompt_registry.get_prompt("agent.todo_reminder.tpl") or ""
    todos_rendered = _render_todo_lines(todos, active_id)
    total = len(todos)

    if template:
        rendered = (
            template
            .replace("{{plan_version}}", "1")
            .replace("{{todos_rendered}}", todos_rendered)
            .replace("{{active_todo_seq}}", str(active_seq) if active_seq is not None else "?")
            .replace("{{total_todos}}", str(total))
            .replace("{{active_todo_content}}", active_content or "")
        )
        return rendered

    # Fallback (template non in DB): rendering minimale
    return (
        f"<todo_list>\n{todos_rendered}\n</todo_list>\n"
        f"Stai lavorando sul todo {active_seq}/{total}: \"{active_content}\". "
        f"Procedi voce per voce, aggiorna via nexus_todo_write action='check'."
    )


def append_reminder_block(anthropic_content_blocks: list, reminder_text: str) -> None:
    """Modifica in place la lista di blocchi anthropic_content aggiungendo
    un blocco text con il system-reminder.

    Anthropic accetta blocchi misti tool_result + text in uno stesso
    HumanMessage, quindi NON serve un secondo message.
    """
    if not reminder_text:
        return
    anthropic_content_blocks.append({
        "type": "text",
        "text": f"<system-reminder>\n{reminder_text}\n</system-reminder>",
    })
