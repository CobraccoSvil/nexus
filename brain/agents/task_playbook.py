"""task_playbook — Task-Playbook Engine (regole G + L).

Inietta nel `system_text` dell'agente una "guida" (playbook) riusabile quando il
contesto del turno corrisponde al trigger del playbook. La conoscenza di dominio
(es. "come implementare un'app da un file Figma Make") vive in DB
(`nexus_task_playbooks`, mig 0366), NON nel prompt che l'utente deve scrivere.

Punto UNICO di iniezione: `router_node` (gira sempre, anche in modalita'
automatica, prima di planner/executor). Best-effort: ogni errore -> nessun blocco
(no-op), non blocca mai il run.

Matcher DETERMINISTICO sugli assi di `trigger_json`:
  - `intent`: lista di intent ammessi (gate; assente = qualsiasi).
  - `keywords`: almeno una presente nel testo utente.
  - `attachment_kind`: kind allegato presente nel contesto.
  - `project_markers`: marcatore presente nella root del progetto.
Un playbook matcha se l'intent (se vincolato) e' compatibile E almeno un asse
"positivo" tra quelli VALUTABILI col contesto disponibile e' soddisfatto. Gli assi
per cui il contesto non fornisce dati non contribuiscono (estendibile: passando piu'
segnali nel context il matcher li usa senza modifiche). Niente LLM: prevedibile e a
costo zero per turno; il classificatore LLM e' un'estensione futura (nuovo asse).
"""
from __future__ import annotations

import logging
import os
import time
from typing import Any

logger = logging.getLogger(__name__)

# Cache TTL allineata agli altri config DB-driven del brain (60s).
_CACHE_TTL = 60.0
_pb_cache: list[dict[str, Any]] | None = None
_pb_cache_ts: float = 0.0
_enabled_cache: tuple[float, bool] | None = None

# Cap difensivo: al massimo N playbook iniettati per turno (evita di gonfiare il
# prompt se per errore molti trigger combaciano).
_MAX_PLAYBOOKS = 2


def _db_url() -> str | None:
    return os.environ.get("DATABASE_URL")


def is_enabled() -> bool:
    """Kill switch globale via settings.orchestrator.task_playbook.enabled (cache 60s).
    Default True se il DB e' irraggiungibile o la chiave manca (la migrazione 0366
    la inserisce a 'true')."""
    global _enabled_cache
    now = time.monotonic()
    if _enabled_cache is not None and (now - _enabled_cache[0]) < _CACHE_TTL:
        return _enabled_cache[1]
    enabled = True
    url = _db_url()
    if url:
        try:
            from brain.utils.db_pool import connect as _db_connect
            with _db_connect() as conn:
                with conn.cursor() as cur:
                    cur.execute(
                        "SELECT value FROM settings WHERE key = %s",
                        ("orchestrator.task_playbook.enabled",),
                    )
                    row = cur.fetchone()
                    if row and row[0] is not None:
                        enabled = str(row[0]).strip().lower() not in ("false", "0", "off", "no")
        except Exception as exc:  # noqa: BLE001 — best-effort
            logger.debug("task_playbook: is_enabled fallback true (%s)", exc)
    _enabled_cache = (now, enabled)
    return enabled


def load_enabled_playbooks() -> list[dict[str, Any]]:
    """Carica i playbook abilitati da DB (cache 60s). Ritorna lista di dict
    {key, trigger, guidance, priority}. Lista vuota se DB down o tabella assente."""
    global _pb_cache, _pb_cache_ts
    now = time.monotonic()
    if _pb_cache is not None and (now - _pb_cache_ts) < _CACHE_TTL:
        return _pb_cache
    out: list[dict[str, Any]] = []
    url = _db_url()
    if url:
        try:
            import json as _json

            from brain.utils.db_pool import connect as _db_connect
            with _db_connect() as conn:
                with conn.cursor() as cur:
                    cur.execute(
                        "SELECT key, trigger_json, guidance_text, priority, "
                        "       COALESCE(steps_json, '[]'::jsonb) "
                        "FROM nexus_task_playbooks WHERE enabled = TRUE "
                        "ORDER BY priority DESC, key ASC"
                    )
                    for key, trigger_json, guidance, priority, steps_json in cur.fetchall():
                        # psycopg2 ritorna JSONB come dict (se registrato) o str.
                        trig = trigger_json
                        if isinstance(trig, str):
                            try:
                                trig = _json.loads(trig)
                            except Exception:  # noqa: BLE001
                                trig = {}
                        steps = steps_json
                        if isinstance(steps, str):
                            try:
                                steps = _json.loads(steps)
                            except Exception:  # noqa: BLE001
                                steps = []
                        out.append({
                            "key": key,
                            "trigger": trig or {},
                            "guidance": guidance or "",
                            "priority": int(priority or 100),
                            "steps": [str(s) for s in steps] if isinstance(steps, list) else [],
                        })
        except Exception as exc:  # noqa: BLE001 — best-effort (tabella assente, DB down)
            logger.debug("task_playbook: load fallita (%s)", exc)
    _pb_cache = out
    _pb_cache_ts = now
    return out


def _as_list(value: Any) -> list[str]:
    if isinstance(value, list):
        return [str(v).strip().lower() for v in value if str(v).strip()]
    if isinstance(value, str) and value.strip():
        return [value.strip().lower()]
    return []


import re as _re

# Blocchi di SISTEMA iniettati da mcp-core nel messaggio (convenzione chiusa
# nostra, parser di formato fisso): vanno RIMOSSI prima del match keywords.
# Incidente run 5df5cef2/5ec12cad: il blocco <allegati_sessione> (prepended a
# OGNI turno) contiene il filename "PL.make" -> la keyword ".make" del playbook
# implement.figma_make matchava su QUALSIASI domanda della sessione (anche
# "quante tabelle ci sono nel db"), iniettando la guida all'estrazione figma e
# facendo deragliare i modelli su task non pertinenti.
_SYSTEM_BLOCK_RE = _re.compile(
    r"<(allegati|allegati_sessione|task_playbook)[^>]*>.*?</\1>",
    _re.DOTALL | _re.IGNORECASE,
)


def _user_text_only(text: str) -> str:
    """Testo utente PULITO per il match keywords: senza i blocchi di sistema."""
    return _SYSTEM_BLOCK_RE.sub("", text)


def match(context: dict[str, Any]) -> list[dict[str, Any]]:
    """Ritorna i playbook che matchano il contesto, ordinati per priority desc.

    context: {
      "intent": str,
      "text": str,                      # testo utente (i blocchi <allegati*> di
                                        # sistema vengono RIMOSSI prima del match)
      "attachment_kinds": set[str]|list,# opzionale
      "project_markers": set[str]|list, # opzionale
    }
    """
    intent = str(context.get("intent") or "").strip().lower()
    text = _user_text_only(str(context.get("text") or "")).lower()
    akinds = {str(k).strip().lower() for k in (context.get("attachment_kinds") or [])}
    markers = {str(m).strip().lower() for m in (context.get("project_markers") or [])}

    matched: list[dict[str, Any]] = []
    for pb in load_enabled_playbooks():
        trig = pb.get("trigger") or {}

        # Gate intent: se specificato, l'intent corrente deve esservi incluso.
        intents = _as_list(trig.get("intent"))
        if intents and intent and intent not in intents:
            continue

        # Almeno un asse positivo tra quelli valutabili col contesto disponibile.
        hit = False
        keywords = _as_list(trig.get("keywords"))
        if keywords and any(k in text for k in keywords):
            hit = True
        if not hit:
            ak = str(trig.get("attachment_kind") or "").strip().lower()
            if ak and ak in akinds:
                hit = True
        if not hit:
            pms = _as_list(trig.get("project_markers"))
            if pms and any(m in markers for m in pms):
                hit = True

        if hit:
            matched.append(pb)

    matched.sort(key=lambda p: p.get("priority", 100), reverse=True)
    return matched


def build_block(playbooks: list[dict[str, Any]]) -> str:
    """Incapsula le guide dei playbook in un blocco XML per il system prompt."""
    parts: list[str] = []
    for pb in playbooks[:_MAX_PLAYBOOKS]:
        guidance = (pb.get("guidance") or "").strip()
        if not guidance:
            continue
        key = pb.get("key", "")
        parts.append(f'<task_playbook key="{key}">\n{guidance}\n</task_playbook>')
    return "\n\n".join(parts)


def matched_steps(context: dict[str, Any]) -> tuple[str, list[str]] | None:
    """Ritorna (key, steps) del primo playbook matchato con passi STRUTTURATI
    (steps_json, mig 0395), o None. Usato dal planner per generare i todos
    deterministicamente quando il modello non emette nexus_todo_write: i passi
    del playbook diventano il piano, senza sperare che l'LLM li trascriva.
    Best-effort, mai solleva."""
    try:
        if not is_enabled():
            return None
        for pb in match(context):
            steps = pb.get("steps") or []
            if steps:
                return str(pb.get("key", "")), [str(s) for s in steps]
    except Exception as exc:  # noqa: BLE001 — best-effort
        logger.debug("task_playbook: matched_steps skip (%s)", exc)
    return None


def guidance_for(context: dict[str, Any]) -> str:
    """Entry point per il router: ritorna il blocco da appendere al system_text,
    oppure "" se disabilitato/nessun match/errore. Best-effort, mai solleva."""
    try:
        if not is_enabled():
            return ""
        matched = match(context)
        if not matched:
            return ""
        block = build_block(matched)
        if block:
            logger.info(
                "task_playbook: match %d playbook (%s)",
                len(matched), ",".join(p.get("key", "?") for p in matched[:_MAX_PLAYBOOKS]),
            )
        return block
    except Exception as exc:  # noqa: BLE001 — best-effort, mai blocca il run
        logger.debug("task_playbook: guidance_for skip (%s)", exc)
        return ""
