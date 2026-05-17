"""project_instructions_loader (PR-3, Codex AGENTS.md pattern).

Carica il contenuto di `<project_root>/.nexus/project-instructions.md` e lo
inietta nel system_text di ogni run sul progetto.

Sorgente principale: tabella `nexus_project_instructions` (cache DB-driven
aggiornata dal file watcher Rust). Fallback: lettura diretta del file se la
cache e' mancante (boot scenario).

Troncamento configurabile via setting `orchestrator.project_instructions_max_chars`
(default 8000) per evitare di gonfiare il system_text.
"""
from __future__ import annotations

import hashlib
import logging
import os
from typing import Optional

from . import orchestrator_config, prompt_registry, prompt_renderer

logger = logging.getLogger(__name__)


def load_project_instructions(
    db_conn,
    project_id: str,
    project_root: Optional[str] = None,
) -> Optional[str]:
    """Ritorna il blocco rendered da iniettare nel system_text, oppure None.

    `db_conn` accetta sia psycopg2 connection sia sqlx (usiamo solo cursor()).
    """
    if not project_id:
        return None
    cfg = orchestrator_config.get()
    max_chars = int(cfg.get("project_instructions_max_chars", 8000))
    file_rel = cfg.get("project_instructions_file", ".nexus/project-instructions.md")

    content: Optional[str] = None
    content_hash: Optional[str] = None
    try:
        cur = db_conn.cursor()
        cur.execute(
            "SELECT content_cache, content_hash, file_path "
            "FROM nexus_project_instructions WHERE project_id = %s LIMIT 1",
            (project_id,),
        )
        row = cur.fetchone()
        cur.close()
        if row:
            content, content_hash, file_rel_db = row
            if file_rel_db:
                file_rel = file_rel_db
    except Exception as exc:
        logger.debug("project_instructions: lookup DB fallita per %s: %s", project_id, exc)

    # Fallback FS se la cache DB e' vuota.
    if not content and project_root:
        fs_path = os.path.join(project_root, file_rel)
        if os.path.isfile(fs_path):
            try:
                with open(fs_path, "r", encoding="utf-8") as f:
                    content = f.read()
                content_hash = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
                # Best-effort: persisti la cache per i prossimi caller.
                try:
                    cur = db_conn.cursor()
                    cur.execute(
                        """
                        INSERT INTO nexus_project_instructions
                            (project_id, file_path, content_cache, content_hash, updated_at)
                        VALUES (%s, %s, %s, %s, NOW())
                        ON CONFLICT (project_id) DO UPDATE
                        SET content_cache = EXCLUDED.content_cache,
                            content_hash = EXCLUDED.content_hash,
                            file_path = EXCLUDED.file_path,
                            updated_at = NOW()
                        """,
                        (project_id, file_rel, content, content_hash),
                    )
                    db_conn.commit()
                    cur.close()
                except Exception:
                    pass
            except Exception as exc:
                logger.debug("project_instructions: lettura FS fallita per %s: %s", fs_path, exc)

    if not content or not content.strip():
        return None

    if len(content) > max_chars:
        content = content[: max_chars - 30] + "\n[... truncated by orchestrator]"

    template = prompt_registry.get_prompt("system.project_instructions_block") or ""
    if not template:
        # Fallback "lite" se prompt non in DB.
        return f"<project_instructions file={file_rel}>\n{content.strip()}\n</project_instructions>"
    rendered = prompt_renderer.render(
        template,
        {
            "file_path": file_rel,
            "content_hash": content_hash or "n/a",
            "content": content.strip(),
        },
    )
    return rendered or None
