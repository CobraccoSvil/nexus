"""session_worklog — lettura del worklog di sessione (mig 0411).

Storia di lavoro canonica e provider-agnostica: il digest e' materializzato
da mcp-core (`crates/mcp-core/src/session_worklog.rs`, punto unico del
rendering — regola L) in `nexus_session_worklog.rendered_block`. Qui si fa
SOLO la lettura e l'avvolgimento nel tag `<session_worklog>` per l'iniezione
nel system_text (router_node, pattern task_playbook).

Essendo puro testo nel system, il blocco sopravvive identico a qualunque
cambio di provider/modello (cascade fallback, re-route, supersede, resume).

Fail-open by design: qualunque errore (DB giu', tabella assente, setting
mancante) ritorna stringa vuota — il worklog non deve MAI bloccare un run.
"""
from __future__ import annotations

import logging

from brain.utils.db_pool import connect as db_connect
from brain.utils.settings_db import get_setting_cached

logger = logging.getLogger(__name__)


def _enabled() -> bool:
    raw = get_setting_cached("agent.worklog.enabled", "true")
    return str(raw).strip().lower() in ("1", "true", "yes", "on")


def fetch_worklog_block(session_id: str) -> str:
    """Ritorna il blocco `<session_worklog>` pronto per il system_text.

    Stringa vuota se: worklog disabilitato, sessione senza worklog, digest
    vuoto, o qualunque errore di lettura (fail-open).
    """
    if not session_id:
        return ""
    try:
        if not _enabled():
            return ""
    except Exception:
        return ""
    try:
        with db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT rendered_block FROM nexus_session_worklog "
                "WHERE session_id = %s LIMIT 1",
                (session_id,),
            )
            row = cur.fetchone()
    except Exception as exc:
        logger.debug("session_worklog: lettura fallita per %s: %s", session_id, exc)
        return ""
    if not row or not row[0] or not str(row[0]).strip():
        return ""
    block = str(row[0]).strip()
    return f"<session_worklog>\n{block}\n</session_worklog>"


def _learned_enabled() -> bool:
    raw = get_setting_cached("orchestrator.learned_instructions_enabled", "true")
    return str(raw).strip().lower() in ("1", "true", "yes", "on")


def _learned_max_chars() -> int:
    raw = get_setting_cached("orchestrator.learned_instructions_max_chars", "1500")
    try:
        return max(200, int(str(raw).strip()))
    except (TypeError, ValueError):
        return 1500


def fetch_learned_instructions_block(project_id: str) -> str:
    """Ritorna il blocco `<learned_instructions>` (regole durature attive del
    progetto, mig 0412) pronto per il system_text.

    Livello 2 della continuita': mentre il worklog e' la storia operativa
    volatile, qui ci sono le regole stabili (convenzioni, preferenze, ambiente)
    distillate dal worker `learned_instructions.rs` e SEMPRE iniettate.

    Stringa vuota se: feature disabilitata, progetto senza regole attive, o
    qualunque errore (fail-open). Budget di caratteri DB-driven (riduzione
    token); il wrapper viene dal template `system.learned_instructions_block`
    con fallback inline.
    """
    if not project_id:
        return ""
    try:
        if not _learned_enabled():
            return ""
    except Exception:
        return ""
    try:
        with db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT category, rule_text FROM nexus_learned_instructions "
                "WHERE project_id = %s AND status = 'active' "
                "ORDER BY category, created_at",
                (project_id,),
            )
            rules = cur.fetchall()
            cur.execute(
                "SELECT content FROM nexus_prompt_templates "
                "WHERE key = 'system.learned_instructions_block' AND is_active = TRUE "
                "LIMIT 1",
                (),
            )
            tpl_row = cur.fetchone()
    except Exception as exc:
        logger.debug("learned_instructions: lettura fallita per %s: %s", project_id, exc)
        return ""
    if not rules:
        return ""

    # Elenco puntato raggruppato per categoria, troncato al budget.
    max_chars = _learned_max_chars()
    lines: list[str] = []
    current_cat = None
    used = 0
    for cat, text in rules:
        cat = str(cat or "").strip()
        text = str(text or "").strip()
        if not text:
            continue
        if cat != current_cat:
            header = f"[{cat}]"
            lines.append(header)
            used += len(header) + 1
            current_cat = cat
        entry = f"- {text}"
        if used + len(entry) > max_chars:
            lines.append("- (... altre regole troncate)")
            break
        lines.append(entry)
        used += len(entry) + 1
    rules_text = "\n".join(lines).strip()
    if not rules_text:
        return ""

    tpl = str(tpl_row[0]) if tpl_row and tpl_row[0] else ""
    if tpl and "{{rules}}" in tpl:
        return tpl.replace("{{rules}}", rules_text)
    # Fallback inline (fail-safe se il template non e' in DB).
    return (
        "<learned_instructions>\n"
        "Regole durature di questo progetto, apprese dall'esperienza "
        "(rispettale salvo indicazione contraria dell'utente):\n"
        f"{rules_text}\n"
        "</learned_instructions>"
    )
