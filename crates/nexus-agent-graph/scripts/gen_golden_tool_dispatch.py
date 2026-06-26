#!/usr/bin/env python3
"""Golden di parita' 1:1 per `nodes::tool_dispatch::ToolDispatchNode` (la META'
del loop agentico che esegue i tool_use pendenti) del crate `nexus-agent-graph`.

Replica la LOGICA DETERMINISTICA del `tool_dispatch_node`
(`brain/agents/nodes/__init__.py:3525-4221`) data:
  - lo stato (pending, attachment_read_bytes, discovered_tools_next_turn,
    blocked_cap_rejected, declared_done_count, run_notes, ...),
  - la config DB-driven (predictive_cap_ratio, context_window, budget allegati,
    discovery_first_*, schema_max_bytes, ...),
  - l'ESITO STUBATO di ogni tool eseguito (content/is_error/exit_code), cosi' la
    parte I/O (ToolExecutor) e' un INPUT e la logica resta deterministica.

NON replica l'esecuzione reale dei tool ne' l'offload RAG (I/O, testati con stub
Rust). Replica: il GATE per ogni pending (predictive cap col SENTINEL / M16 /
budget allegati), la RICOMPOSIZIONE nell'ordine ORIGINALE, l'exit_code che fluisce
nel blocco, la guard blocked-da-cap, il parse discovered (sempre scritto, anche []),
l'accumulo discovered_run, e i campi chiave del delta.

Le funzioni pure (predictive_cap_check, is_cap_exempt, estimate_*, M16 parse,
normalize_declared_outcome, apply_run_notes) sono IMPORTATE dal brain reale per
non duplicare la fonte di verita'; la decision-machine del nodo e' replicata qui
1:1 (e' embedded nel nodo async, non isolabile senza DB/gRPC).

Output: /tmp/golden_tool_dispatch.json — lista di {case_id, input, output}.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_tool_dispatch.py
  cargo test -p nexus-agent-graph --lib golden_tool_dispatch -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

# Funzioni PURE reali dal brain (fonte unica di verita' — niente duplicazione).
from brain.agents.nodes.helpers import (  # noqa: E402
    PREDICTIVE_CAP_SENTINEL,
    _estimate_tool_result_size_bytes,
    _extract_returned_bytes,
    _estimate_context_chars,
    _current_context_token_estimate,
    apply_run_notes,
    normalize_declared_outcome,
)
from brain.agents.nodes.helpers import (  # noqa: E402
    TASK_COMPLETE_TOOL_NAME,
    RUN_NOTES_TOOL_NAME,
    _CAP_EXEMPT_TOOLS,
)

_ATTACHMENT_READ_TOOLS = {"nexus_read_attachment", "nexus_read_archive_entry"}
_M16_META_TOOLS = {"nexus_mcp_tool_search", "nexus_mcp_tool_call"}


def _is_cap_exempt(name: str) -> bool:
    return name in _CAP_EXEMPT_TOOLS or name.startswith("dispatcher_")


def _build_m16_allowed(whitelist, always_on):
    return _M16_META_TOOLS | set(whitelist) | set(always_on) | {
        TASK_COMPLETE_TOOL_NAME, RUN_NOTES_TOOL_NAME
    }


def dispatch_decision(state: dict, cfg: dict, tool_results: dict) -> dict:
    """Replica deterministica del tool_dispatch_node.

    `tool_results` mappa tool_use_id -> {content, is_error, exit_code?, raw_content?}
    per i tool KEPT (esito stubato dell'esecuzione). I synthetic non lo usano.
    Ritorna un delta normalizzato confrontabile col Rust.
    """
    pending = list(state.get("pending_tool_uses") or [])
    # (2) pending vuoto.
    if not pending:
        return {"pending_tool_uses": [], "stop_reason": "end_turn"}

    # (1) superseded: l'input lo segnala direttamente (il golden non fa I/O DB).
    if state.get("_superseded"):
        return {"pending_tool_uses": [], "stop_reason": "superseded"}

    ctx_chars = _estimate_context_chars(list(state.get("messages") or []))
    current_bytes = int(state.get("attachment_read_bytes") or 0)
    budget_total = int(cfg["attachment_budget_bytes"])
    window = int(cfg["context_window"])
    ratio = float(cfg["predictive_cap_ratio"])
    allowed = _build_m16_allowed(cfg["discovery_first_whitelist"], cfg["always_on_tools"])
    disc_now = {
        d.get("name")
        for d in (state.get("discovered_tools_next_turn") or [])
        if isinstance(d, dict)
    }
    pred_msgs = list(state.get("messages") or [])
    pred_system = state.get("system_text") or ""
    pred_tokens = _current_context_token_estimate(pred_msgs, pred_system)

    slots: list[dict | None] = []
    kept_ids: list[str] = []
    declared_outcomes: list[dict] = []
    task_complete_ids: list[str] = []
    run_notes_holder = [state.get("run_notes")]
    infra_error = False

    # ── (3) gate per pending ──────────────────────────────────────────────────
    for b in pending:
        name = b.get("name", "")
        tid = b.get("id", "")
        tin = b.get("input", {}) or {}
        # (a) predictive cap (priorita'). Solo se window>0 (parita' col nodo Rust,
        #     che non applica il cap a window ignota). Il Python lo applica sempre
        #     ma con window risolta dal modello; per il golden passiamo window>0.
        cap_msg = None
        if window > 0 and not _is_cap_exempt(name):
            expected = _estimate_tool_result_size_bytes(name, tin)
            # Replica numerica di _predictive_cap_check (puro): la versione brain
            # fa il lookup window/contesto internamente; qui usiamo la formula con
            # i numeri gia' risolti per restare deterministici.
            cap_tokens = int(window * ratio)
            expected_tokens = int(expected / 3.5)
            if pred_tokens + expected_tokens > cap_tokens:
                pct = int(pred_tokens / max(window, 1) * 100)
                ratio_pct = int(ratio * 100)
                cap_msg = (
                    f"{PREDICTIVE_CAP_SENTINEL}\n"
                    "ATTENZIONE: e' stata bloccata SOLO questa chiamata, NON il task. "
                    "Se questo tool non e' essenziale per la RICHIESTA CORRENTE dell'utente "
                    "(es. l'hai chiamato per via di contenuti storici della conversazione), "
                    "IGNORALO e prosegui col task usando i dati che hai gia' raccolto. "
                    "NON dichiarare il task bloccato per questo motivo.\n"
                    f"Dettaglio: context a {pred_tokens} token ({pct}% del budget {window}); il "
                    f"risultato atteso aggiungerebbe ~{expected_tokens} token oltre il "
                    f"{ratio_pct}% (cap={cap_tokens}).\n"
                    "Solo se il tool e' DAVVERO necessario alla richiesta corrente:\n"
                    "- Riduci i parametri (es. length piu' piccolo).\n"
                    "- Usa estrazione strutturata (nexus_extract_figma_structure, "
                    "nexus_extract_pdf_text, nexus_extract_docx_text).\n"
                    "- Oppure dichiara con task_complete outcome=needs_input cosa serve dall'utente."
                )
        if cap_msg is not None:
            slots.append({"tool_use_id": tid, "content": cap_msg, "is_error": True, "exit_code": None})
            continue
        # (b) M16.
        if cfg["discovery_first_enabled"] and name not in allowed and name not in disc_now:
            err = json.dumps({
                "error": (
                    f"Il tool '{name}' non e' disponibile direttamente in questo turno. "
                    f"Usa prima nexus_mcp_tool_search (query: \"{name}\") per scoprirlo, "
                    f"poi richiamalo al turno successivo."
                )
            })
            slots.append({"tool_use_id": tid, "content": err, "is_error": True, "exit_code": None})
            continue
        # (c) budget allegati.
        if name in _ATTACHMENT_READ_TOOLS and current_bytes >= budget_total:
            err = json.dumps({
                "error": (
                    f"budget letture allegati esaurito ({current_bytes} byte gia' letti su "
                    f"{budget_total} budget). Usa un tool di estrazione strutturata "
                    f"(nexus_extract_pdf_text, nexus_extract_figma_structure, "
                    f"nexus_extract_docx_text, nexus_extract_xlsx_data) oppure chiedi "
                    f"all'utente una versione testuale del file."
                ),
                "budget_bytes": budget_total,
                "already_read": current_bytes,
            }, ensure_ascii=False)
            slots.append({"tool_use_id": tid, "content": err, "is_error": True, "exit_code": None})
            continue
        # KEPT.
        slots.append(None)
        kept_ids.append(tid)

    # ── (4) "esecuzione" KEPT dagli esiti stubati + brain-only ────────────────
    def run_one(b: dict) -> dict:
        name = b.get("name", "")
        tid = b.get("id", "")
        tin = b.get("input", {}) or {}
        if name == RUN_NOTES_TOOL_NAME:
            new_notes = apply_run_notes(run_notes_holder[0], tin)
            if new_notes is not None:
                run_notes_holder[0] = new_notes
            return {
                "tool_use_id": tid,
                "content": json.dumps(
                    {"acknowledged": new_notes is not None, "notes_chars": len(new_notes or "")},
                    ensure_ascii=False,
                ),
                "is_error": new_notes is None,
                "exit_code": None,
            }
        if name == TASK_COMPLETE_TOOL_NAME:
            task_complete_ids.append(tid)
            decl = normalize_declared_outcome(tin)
            if decl is not None:
                declared_outcomes.append(decl)
            return {
                "tool_use_id": tid,
                "content": json.dumps(
                    {"acknowledged": decl is not None, "outcome": (decl or {}).get("outcome")},
                    ensure_ascii=False,
                ),
                "is_error": decl is None,
                "exit_code": None,
            }
        stub = tool_results.get(tid, {"content": "{}", "is_error": False})
        return {
            "tool_use_id": tid,
            "content": stub.get("content", "{}"),
            "is_error": bool(stub.get("is_error", False)),
            "exit_code": stub.get("exit_code"),
        }

    kept_iter = iter([run_one(b) for b in pending if b.get("id") in kept_ids])
    results: list[dict] = []
    for slot in slots:
        if slot is None:
            results.append(next(kept_iter))
        else:
            results.append(slot)

    # ── (6) attachment bytes ──────────────────────────────────────────────────
    added = 0
    for b, r in zip(pending, results):
        if b.get("name", "") in _ATTACHMENT_READ_TOOLS and not r["is_error"]:
            added += _extract_returned_bytes(r.get("content", ""))
    new_attachment_read_bytes = current_bytes + added

    # ── (7) guard blocked-da-cap ──────────────────────────────────────────────
    blocked_cap_rejected_now = False
    if (
        declared_outcomes
        and declared_outcomes[-1].get("outcome") == "blocked"
        and not state.get("blocked_cap_rejected")
        and any(PREDICTIVE_CAP_SENTINEL in str(r.get("content") or "") for r in results)
    ):
        for r in results:
            if r["tool_use_id"] in task_complete_ids:
                r["content"] = json.dumps({
                    "acknowledged": False,
                    "reason": (
                        "Dichiarazione 'blocked' RIFIUTATA: l'unico blocco di "
                        "questo turno e' il predictive context cap su una singola "
                        "chiamata, NON un blocco del task. Prosegui col task usando "
                        "i dati gia' raccolti e rispondi alla richiesta corrente "
                        "dell'utente."
                    ),
                }, ensure_ascii=False)
                r["is_error"] = True
        declared_outcomes.clear()
        blocked_cap_rejected_now = True

    # ── (11) discovered (SEMPRE scritto, anche []) ────────────────────────────
    discovered_next: list[dict] = []
    max_bytes = int(cfg["discovery_schema_max_bytes"])
    for b, r in zip(pending, results):
        if b.get("name") != "nexus_mcp_tool_search" or r["is_error"]:
            continue
        raw = r.get("raw_content") or r.get("content") or "{}"
        try:
            payload = json.loads(raw)
        except Exception:
            continue
        for _res in (payload.get("results") or []):
            if not isinstance(_res, dict):
                continue
            _name = _res.get("tool_name") or _res.get("name")
            if not _name:
                continue
            _schema = _res.get("input_schema") or {"type": "object", "properties": {}}
            try:
                if len(json.dumps(_schema)) > max_bytes:
                    _schema = {"type": "object", "properties": {}}
            except Exception:
                _schema = {"type": "object", "properties": {}}
            if not any(d["name"] == _name for d in discovered_next):
                discovered_next.append({
                    "name": _name,
                    "description": (_res.get("description") or "")[:500],
                    "input_schema": _schema,
                })

    # ── blocchi tool_result finali (senza raw_content, exit_code se presente) ──
    blocks = []
    for r in results:
        blk = {
            "type": "tool_result",
            "tool_use_id": r["tool_use_id"],
            "content": r["content"],
            "is_error": r["is_error"],
        }
        if r.get("exit_code") is not None:
            blk["exit_code"] = int(r["exit_code"])
        blocks.append(blk)

    # ── meta_steps "tool_executed" (live UX, py:4065-4114) ────────────────────
    # Uno per OGNI pending (KEPT/synthetic/brain-only), allineato per posizione a
    # `results`. provider/model dal turno (catena fallback provider_used ->
    # sticky -> override). created_at OMESSO: non deterministico (lo assegna
    # l'integrazione), il Rust lo lascia None e non viene confrontato.
    _exec_provider = (
        state.get("provider_used")
        or state.get("sticky_provider")
        or state.get("provider_override")
    )
    _exec_model = (
        state.get("model_used")
        or state.get("sticky_model")
        or state.get("model_override")
    )
    _target_keys = ("path", "file_path", "abs_path", "command", "query", "pattern", "name", "tool_name")
    meta_steps_out = []
    for b, r in zip(pending, results):
        _tool = b.get("name", "?")
        _input = b.get("input", {}) or {}
        _target = ""
        if isinstance(_input, dict):
            for _k in _target_keys:
                _v = _input.get(_k)
                if isinstance(_v, str) and _v:
                    _target = _v if len(_v) <= 80 else (_v[:77] + "...")
                    break
        _err = bool(r.get("is_error"))
        _title = f"{'errore' if _err else 'tool'} {_tool}" + (f" — {_target}" if _target else "")
        meta_steps_out.append({
            "kind": "tool_executed",
            "title": _title,
            "payload": {
                "tool": _tool,
                "target": _target,
                "is_error": _err,
                "tool_use_id": b.get("id"),
                "provider": _exec_provider,
                "model": _exec_model,
            },
        })

    delta = {
        "pending_tool_uses": [],
        "stop_reason": "tool_use",
        "attachment_read_bytes": new_attachment_read_bytes,
        "discovered_tools_next_turn": discovered_next,
        "blocks": blocks,
        "meta_steps": meta_steps_out,
    }
    if declared_outcomes:
        delta["declared_outcome"] = declared_outcomes[-1]
        done_now = sum(1 for d in declared_outcomes if d.get("outcome") == "done")
        if done_now:
            delta["declared_done_count"] = int(state.get("declared_done_count") or 0) + done_now
    if infra_error:
        delta["tool_infra_error"] = True
    if blocked_cap_rejected_now:
        delta["blocked_cap_rejected"] = True
    if run_notes_holder[0] != state.get("run_notes"):
        delta["run_notes"] = run_notes_holder[0]
    return delta


def main() -> None:
    cases = []
    base_cfg = {
        "predictive_cap_ratio": 0.8,
        "context_window": 0,
        "attachment_budget_bytes": 500_000,
        "discovery_first_enabled": False,
        "discovery_first_whitelist": ["nexus_mcp_tool_search", "nexus_mcp_tool_call"],
        "always_on_tools": [],
        "discovery_schema_max_bytes": 8192,
    }

    def case(cid, state, cfg, tool_results):
        out = dispatch_decision(state, cfg, tool_results)
        cases.append({
            "case_id": cid,
            "input": {"state": state, "cfg": cfg, "tool_results": tool_results},
            "output": out,
        })

    # pending vuoto.
    case("pending_vuoto", {"pending_tool_uses": []}, dict(base_cfg), {})

    # superseded.
    case("superseded",
         {"pending_tool_uses": [{"id": "a", "name": "read_file", "input": {}}], "_superseded": True},
         dict(base_cfg), {})

    # kept ordine preservato + exit_code.
    case("kept_ordine_exit_code",
         {"pending_tool_uses": [
             {"id": "c1", "name": "read_file", "input": {"path": "a"}},
             {"id": "c2", "name": "run_command", "input": {"command": "build"}},
         ]},
         dict(base_cfg),
         {"c1": {"content": "{\"text\":\"file\"}", "is_error": False},
          "c2": {"content": "{\"stdout\":\"ok\"}", "is_error": False, "exit_code": 0}})

    # predictive cap blocca (window>0, ratio bassa).
    cfg_cap = dict(base_cfg, context_window=1000, predictive_cap_ratio=0.1)
    case("predictive_cap_blocca",
         {"pending_tool_uses": [
             {"id": "c1", "name": "nexus_read_attachment", "input": {"length": 100000}},
         ]},
         cfg_cap, {})

    # guard blocked-da-cap.
    case("guard_blocked_da_cap",
         {"pending_tool_uses": [
             {"id": "c1", "name": "nexus_read_attachment", "input": {"length": 100000}},
             {"id": "c2", "name": "task_complete", "input": {"outcome": "blocked", "summary": "stop"}},
         ]},
         cfg_cap, {})

    # M16 tool non scoperto.
    cfg_m16 = dict(base_cfg, discovery_first_enabled=True,
                   discovery_first_whitelist=["nexus_mcp_tool_search"])
    case("m16_rifiutato",
         {"pending_tool_uses": [{"id": "c1", "name": "read_file", "input": {"path": "a"}}]},
         cfg_m16, {})

    # budget allegati esaurito.
    cfg_bud = dict(base_cfg, attachment_budget_bytes=1000)
    case("budget_allegati",
         {"pending_tool_uses": [{"id": "c1", "name": "nexus_read_attachment", "input": {}}],
          "attachment_read_bytes": 2000},
         cfg_bud, {})

    # brain-only run_notes + task_complete done.
    case("brain_only",
         {"pending_tool_uses": [
             {"id": "c1", "name": "nexus_run_notes", "input": {"action": "set", "content": "appunto"}},
             {"id": "c2", "name": "task_complete", "input": {"outcome": "done", "summary": "ok"}},
         ]},
         dict(base_cfg), {})

    # discovered parse da search + sempre scritto.
    search_payload = json.dumps({"results": [
        {"tool_name": "nexus_foo", "description": "fa foo", "input_schema": {"type": "object"}},
        {"name": "nexus_bar"},
    ]})
    case("discovered_da_search",
         {"pending_tool_uses": [{"id": "c1", "name": "nexus_mcp_tool_search", "input": {"query": "foo"}}]},
         dict(base_cfg),
         {"c1": {"content": search_payload, "is_error": False, "raw_content": search_payload}})

    # discovered SEMPRE scritto anche [] (nessun search).
    case("discovered_vuoto_scritto",
         {"pending_tool_uses": [{"id": "c1", "name": "read_file", "input": {}}]},
         dict(base_cfg), {})

    # attachment bytes aggiornati da length nel tool_result.
    case("attachment_bytes_aggiornati",
         {"pending_tool_uses": [{"id": "c1", "name": "nexus_read_attachment", "input": {}}],
          "attachment_read_bytes": 100},
         dict(base_cfg),
         {"c1": {"content": json.dumps({"length": 512}), "is_error": False}})

    out_path = "/tmp/golden_tool_dispatch.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden tool_dispatch: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
