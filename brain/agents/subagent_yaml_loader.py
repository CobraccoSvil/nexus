"""subagent_yaml_loader (PR-3, Cursor pattern).

Parser per `.nexus/agents/<kind>.md` nei progetti utente. Permette di shadow-are
le sub-agent definitions del DB centralizzato con varianti project-specific:

Esempio `.nexus/agents/explore.md`:

    ---
    kind: explore
    prompt_key: subagent.explore.base
    tool_whitelist: [list_files, read_file, search_in_files, recall_context]
    model_purpose: explorer
    max_iterations: 25
    timeout_s: 240
    is_background: false
    ---
    # Override del prompt del sub-agent explore per questo progetto
    # (qui content markdown libero, opzionale)

Il body markdown DOPO il frontmatter sostituisce il template del prompt_key se
non vuoto.

Sicurezza:
  - Path validation: il file DEVE essere dentro `<project_root>/.nexus/agents/`
  - Whitelist degli attributi YAML: solo i campi noti, no path traversal
  - Nessuna esecuzione: parsing solo dati statici

Output: lista di dict compatibili con la struttura di `nexus_subagent_definitions`.
"""
from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import Any, Optional

logger = logging.getLogger(__name__)

# Solo questi campi accettati dal frontmatter.
_ALLOWED_FIELDS = {
    "kind", "description", "prompt_key", "tool_whitelist",
    "model_purpose", "max_iterations", "timeout_s", "is_background",
}


def load_project_overrides(project_root: str) -> dict[str, dict[str, Any]]:
    """Ritorna `{kind: definition_dict}` dei sub-agent override del progetto.

    Vuoto se la directory non esiste o nessun file valido.
    """
    if not project_root or not os.path.isdir(project_root):
        return {}
    overrides_dir = Path(project_root) / ".nexus" / "agents"
    if not overrides_dir.is_dir():
        return {}
    out: dict[str, dict[str, Any]] = {}
    for f in overrides_dir.glob("*.md"):
        if not f.is_file():
            continue
        try:
            parsed = _parse_yaml_md(f)
        except Exception as exc:
            logger.warning("subagent_yaml: parse fallito su %s: %s", f, exc)
            continue
        if not parsed:
            continue
        kind = parsed.get("kind") or f.stem
        parsed["kind"] = kind
        parsed["source"] = "project_override"
        out[kind] = parsed
    if out:
        logger.info(
            "subagent_yaml: caricati %d override da %s: %s",
            len(out), overrides_dir, sorted(out.keys()),
        )
    return out


_FRONTMATTER_RE = re.compile(r"^---\s*\n(.*?)\n---\s*\n?(.*)$", re.DOTALL)


def _parse_yaml_md(path: Path) -> Optional[dict[str, Any]]:
    """Parser minimale YAML frontmatter + body markdown.

    Implementazione senza dipendenza esterna (no pyyaml): supporta solo
    `key: value` e `key: [a, b, c]` line per line. Sufficiente per il
    nostro schema definito.
    """
    text = path.read_text(encoding="utf-8")
    m = _FRONTMATTER_RE.match(text)
    if not m:
        # Niente frontmatter — il file e' solo body markdown.
        body = text.strip()
        return {"prompt_body": body} if body else None
    fm_str = m.group(1)
    body = (m.group(2) or "").strip()
    fm: dict[str, Any] = {}
    for line in fm_str.splitlines():
        line = line.rstrip()
        if not line or line.startswith("#"):
            continue
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        key = key.strip()
        value = value.strip()
        if key not in _ALLOWED_FIELDS:
            logger.debug("subagent_yaml: campo non whitelisted ignorato: %s", key)
            continue
        # Tipo: array? bool? int?
        if value.startswith("[") and value.endswith("]"):
            inner = value[1:-1]
            items = [x.strip().strip("'\"") for x in inner.split(",") if x.strip()]
            fm[key] = items
        elif value.lower() in ("true", "false"):
            fm[key] = (value.lower() == "true")
        elif value.isdigit() or (value.startswith("-") and value[1:].isdigit()):
            fm[key] = int(value)
        else:
            fm[key] = value.strip("'\"")
    if body:
        fm["prompt_body"] = body
    return fm
