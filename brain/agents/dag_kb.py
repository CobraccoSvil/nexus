"""dag_kb (Comp.3a): deriva il contesto delle dipendenze dal grafo KB.

Prima di pianificare, interroga il grafo di conoscenza del progetto (note +
relazioni) tramite il tool MCP `knowledge_get_subgraph` (Componente 0) e ne
estrae le DIPENDENZE di esecuzione (rel_type blocks/blocked_by, e refinement
come ordine debole). Il risultato viene iniettato nel planner come blocco
`<grafo_dipendenze_kb>`: il planner lo usa per assegnare node_key/dep_keys ai
todo, cosi' l'esecuzione (verifier_node._pick_next_todo) puo' rispettare
l'ordine topologico invece del solo seq.

Riusa SOLO servizi esistenti: il tool MCP via tool_runner gRPC (niente client
nuovo). Best-effort: qualunque errore -> stringa vuota (il planner procede
senza, comportamento storico). Gated da orchestrator.dag_topological_enabled.
"""
from __future__ import annotations

import json
import logging
import uuid
from typing import Any

logger = logging.getLogger(__name__)

# Relazioni del grafo KB che esprimono un ordine di esecuzione.
# blocks/blocked_by = dipendenza HARD; refinement = ordine debole (il raffinato
# segue il raffinante). relates NON e' una dipendenza: e' solo contesto.
_DEP_REL_TYPES = ["blocks", "blocked_by", "refinement"]


def _last_user_message(state: dict) -> str:
    for m in reversed(state.get("messages", []) or []):
        if getattr(m, "type", None) in ("human", "user"):
            content = getattr(m, "content", "") or ""
            return content if isinstance(content, str) else str(content)
    return ""


async def build_dependency_context(
    state: dict, tool_runner: Any, max_chars: int = 1500
) -> str:
    """Ritorna un blocco testuale `<grafo_dipendenze_kb>` o "" se non applicabile.

    Interroga knowledge_get_subgraph con la richiesta utente come seed e filtra
    le sole relazioni di dipendenza. Best-effort.
    """
    if tool_runner is None:
        return ""
    user_msg = _last_user_message(state).strip()
    if len(user_msg) < 10:
        return ""

    try:
        res = await tool_runner.execute_tool(
            tool_name="knowledge_get_subgraph",
            tool_input={
                "query": user_msg[:2000],
                "rel_types": _DEP_REL_TYPES,
                "depth": 2,
                "max_nodes": 25,
            },
            session_id=str(state.get("session_id") or ""),
            tool_use_id=str(uuid.uuid4()),
        )
        raw = getattr(res, "result_json", None) or "{}"
        data = json.loads(raw)
    except Exception as exc:
        logger.debug("dag_kb: knowledge_get_subgraph fallito (%s)", exc)
        return ""

    nodes = data.get("nodes") or []
    edges = data.get("edges") or []
    if not nodes or not edges:
        # Senza archi di dipendenza non c'e' DAG da suggerire.
        return ""

    # Mappa id -> titolo per rendere leggibili gli archi.
    title_by_id = {n.get("note_id"): (n.get("title") or "") for n in nodes}
    dep_lines: list[str] = []
    for e in edges:
        rel = e.get("rel_type")
        if rel not in _DEP_REL_TYPES:
            continue
        src = title_by_id.get(e.get("from"), "?")
        dst = title_by_id.get(e.get("to"), "?")
        if rel == "blocked_by":
            dep_lines.append(f"- \"{src}\" dipende da \"{dst}\" (fai prima \"{dst}\")")
        elif rel == "blocks":
            dep_lines.append(f"- \"{src}\" blocca \"{dst}\" (fai prima \"{src}\")")
        else:  # refinement
            dep_lines.append(f"- \"{dst}\" raffina \"{src}\" (ordine debole)")
    if not dep_lines:
        return ""

    body = "\n".join(dep_lines)
    if len(body) > max_chars:
        body = body[:max_chars].rsplit("\n", 1)[0]
    return (
        "<grafo_dipendenze_kb>\n"
        "Dal grafo di conoscenza del progetto emergono queste dipendenze tra "
        "aree di lavoro. Usale per ordinare i todo: assegna node_key ai todo e "
        "dep_keys ai todo che dipendono da altri, cosi' l'esecuzione rispetta "
        "l'ordine corretto.\n"
        f"{body}\n"
        "</grafo_dipendenze_kb>"
    )
