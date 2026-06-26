#!/usr/bin/env python3
"""Golden di parita' 1:1 per i detector strutturali portati in
`routing::signals` del crate `nexus-agent-graph`.

Importa le funzioni REALI da `brain/agents/nodes/helpers.py`:
  - has_filesystem_mutation_in_history
  - _has_tool_calls_in_history
  - _tool_result_outcome_after
  - _detect_repeated_failed_command
  - _detect_repeated_action
  - _count_recent_request_port
  - _has_active_resources_in_history
  - _detect_recent_tool_error

Se l'import del brain non e' disponibile (langchain_core mancante), usa una
replica BYTE-FEDELE delle funzioni + classi messaggio minimali, allineata 1:1 al
sorgente (helpers.py:1763-2293). In entrambi i casi il golden e' deterministico e
DB-free (le funzioni che leggono soglie dal DB ricevono il `lookback` di default
e non interrogano il DB).

Formato di ciascun caso: i messaggi sono descritti in una forma INTERMEDIA
(role + tool_use/tool_result) che il test Rust ricostruisce in `state::Message`:
  - {"kind":"ai_tool", "name":..., "input":{...}}  -> AIMessage anthropic_content tool_use
  - {"kind":"ai_text", "text":...}                  -> AIMessage testuale
  - {"kind":"tool", "text":...}                     -> ToolMessage (content str)
  - {"kind":"human_result", "exit_code":int|None, "is_error":bool, "text":...}
        -> HumanMessage anthropic_content tool_result (== tool_dispatch_node)

Output: /tmp/golden_executor_detectors.json. Test Rust:
  python3 crates/nexus-agent-graph/scripts/gen_golden_executor_detectors.py
  cargo test -p nexus-agent-graph --lib golden_executor_detectors -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

# ── Import REALE del brain (con fallback byte-fedele) ────────────────────────
_REAL = False
try:
    from langchain_core.messages import AIMessage, HumanMessage, ToolMessage  # type: ignore
    from brain.agents.nodes import helpers as _h  # type: ignore

    _REAL = True
except Exception as _exc:  # noqa: BLE001
    print(f"[gen_golden_executor_detectors] import brain non disponibile ({_exc}); replica byte-fedele")


# ── Replica byte-fedele (fallback) ───────────────────────────────────────────
if not _REAL:
    _TOOL_ERROR_HINTS = (
        "error:", "errore:", "[error", "exit code: 1", "exit code 1",
        "command failed", "comando fallito", "traceback", "exception:",
        "fatal:", "syntax error", "not found", "non trovato",
        "cannot find module", "module not found", "permission denied",
        "connection refused", "timed out", "timeout", "404 not found",
        "500 internal", "econnrefused", "enoent", "enotfound", "eperm",
        "no such file", "is_error", "[errno",
    )
    _FS_MUTATORS_DEFAULT = frozenset({
        "write_file", "edit_file", "delete_file", "rename_file", "file_write",
        "fs_copy", "fs_mkdir", "fs_move", "format_file", "run_lint_fix",
        "run_command", "command", "run_in_terminal", "git_command", "git_pull",
        "git_commit", "git_stage", "git_push", "nexus_extract_figma_code",
        "nexus_install_shadcn_components", "nexus_mcp_tool_call", "cargo_install",
        "run_service", "service_restart", "stop_service",
    })
    _EXPLORATION_ONLY_TOOLS = frozenset({
        "nexus_list_archive_entries", "nexus_read_archive_entry",
        "nexus_inspect_attachment", "nexus_extract_figma_structure",
        "nexus_list_attachments", "nexus_read_attachment",
        "nexus_extract_docx_text", "nexus_extract_xlsx_data",
        "nexus_extract_pdf_text", "nexus_describe_image_attachment",
        "read_file", "list_files", "grep", "read_file_lines", "search_in_files",
        "nexus_mcp_tool_search", "nexus_get_worklog",
    })
    _PORT_REQUEST_TOOL = "request_port"
    _REPEATED_ACTION_TOOLS = {
        "write_file": ("path", "file_path"),
        "edit_file": ("path", "file_path"),
        "run_command": ("command",),
        "run_service": ("command",),
        "run_in_terminal": ("command",),
    }

    class _Msg:
        def __init__(self, content="", additional_kwargs=None, status=None, tool_call_id=None):
            self.content = content
            self.additional_kwargs = additional_kwargs or {}
            self.status = status
            self.tool_call_id = tool_call_id

    class AIMessage(_Msg):  # type: ignore
        pass

    class HumanMessage(_Msg):  # type: ignore
        pass

    class ToolMessage(_Msg):  # type: ignore
        pass

    def _ai_blocks(m):
        if not isinstance(m, AIMessage):
            return []
        blocks = (m.additional_kwargs or {}).get("anthropic_content") or []
        return blocks if isinstance(blocks, list) else []

    def has_filesystem_mutation_in_history(messages):
        for m in messages:
            for b in _ai_blocks(m):
                if isinstance(b, dict) and b.get("type") == "tool_use" \
                        and (b.get("name") or "") in _FS_MUTATORS_DEFAULT:
                    return True
        return False

    def _has_tool_calls_in_history(messages):
        for m in messages:
            if any(isinstance(b, dict) and b.get("type") == "tool_use" for b in _ai_blocks(m)):
                return True
        return False

    def _tool_result_outcome_after(recent, idx, max_ahead=3):
        for j in range(idx + 1, min(idx + 1 + max_ahead, len(recent))):
            nm = recent[j]
            if isinstance(nm, ToolMessage):
                if getattr(nm, "status", "") == "error":
                    return True
                c = getattr(nm, "content", "")
                if isinstance(c, list):
                    for cc in c:
                        if isinstance(cc, dict):
                            if cc.get("is_error"):
                                return True
                            txt = str(cc.get("text", "") or cc.get("content", ""))
                            if any(h in txt.lower() for h in _TOOL_ERROR_HINTS):
                                return True
                    return False
                if any(h in str(c).lower() for h in _TOOL_ERROR_HINTS):
                    return True
                return False
            if isinstance(nm, HumanMessage):
                blocks = (nm.additional_kwargs or {}).get("anthropic_content") or []
                found = False
                for bb in blocks if isinstance(blocks, list) else []:
                    if not isinstance(bb, dict) or bb.get("type") != "tool_result":
                        continue
                    found = True
                    ec = bb.get("exit_code")
                    if isinstance(ec, int):
                        return ec != 0
                    if bb.get("is_error"):
                        return True
                    cont = bb.get("content")
                    txts = []
                    if isinstance(cont, list):
                        for cc in cont:
                            if isinstance(cc, dict):
                                txts.append(str(cc.get("text", "") or cc.get("content", "")))
                    elif cont is not None:
                        txts.append(str(cont))
                    if any(h in t.lower() for t in txts for h in _TOOL_ERROR_HINTS):
                        return True
                if found:
                    return False
        return None

    def _detect_repeated_failed_command(messages, lookback=12):
        if not messages:
            return (None, 0)
        failed = {}
        last_sig = None
        recent = messages[-lookback:] if len(messages) > lookback else messages
        for idx, m in enumerate(recent):
            for b in _ai_blocks(m):
                if not isinstance(b, dict) or b.get("type") != "tool_use":
                    continue
                name = b.get("name", "")
                if name not in ("run_command", "run_service", "run_in_terminal"):
                    continue
                inp = b.get("input", {}) or {}
                cmd = str(inp.get("command", "")).strip()
                wd = str(inp.get("working_dir", "")).strip()
                if not cmd:
                    continue
                signature = f"{cmd}|{wd}"
                next_is_error = False
                for j in range(idx + 1, min(idx + 4, len(recent))):
                    nm = recent[j]
                    if isinstance(nm, ToolMessage):
                        if getattr(nm, "status", "") == "error":
                            next_is_error = True
                        else:
                            c = getattr(nm, "content", "")
                            if isinstance(c, list):
                                for cc in c:
                                    if isinstance(cc, dict):
                                        if cc.get("is_error"):
                                            next_is_error = True
                                            break
                                        txt = str(cc.get("text", "") or cc.get("content", ""))
                                        if any(h in txt.lower() for h in _TOOL_ERROR_HINTS):
                                            next_is_error = True
                                            break
                            else:
                                if any(h in str(c).lower() for h in _TOOL_ERROR_HINTS):
                                    next_is_error = True
                        break
                if next_is_error:
                    failed[signature] = failed.get(signature, 0) + 1
                    last_sig = signature
        if not failed:
            return (None, 0)
        top = max(failed.items(), key=lambda kv: (kv[1], kv[0] == last_sig))
        return (top[0].split("|", 1)[0], top[1])

    def _detect_repeated_action(messages, lookback=24):
        if not messages:
            return (None, 0)
        counts = {}
        labels = {}
        succeeded = set()
        last_sig = None
        recent = messages[-lookback:] if len(messages) > lookback else messages
        for idx, m in enumerate(recent):
            for b in _ai_blocks(m):
                if not isinstance(b, dict) or b.get("type") != "tool_use":
                    continue
                name = b.get("name", "")
                keys = _REPEATED_ACTION_TOOLS.get(name)
                if not keys:
                    continue
                inp = b.get("input", {}) or {}
                value = ""
                for k in keys:
                    v = str(inp.get(k, "") or "").strip()
                    if v:
                        value = v
                        break
                if not value:
                    continue
                sig = f"{name}|{value}"
                counts[sig] = counts.get(sig, 0) + 1
                labels[sig] = f"{name}: {value[:120]}"
                last_sig = sig
                if _tool_result_outcome_after(recent, idx) is False:
                    succeeded.add(sig)
        for sig in succeeded:
            counts.pop(sig, None)
        if not counts:
            return (None, 0)
        top = max(counts.items(), key=lambda kv: (kv[1], kv[0] == last_sig))
        return (labels.get(top[0], top[0]), top[1])

    def _count_recent_request_port(messages, lookback=16):
        if not messages:
            return 0
        recent = messages[-lookback:] if len(messages) > lookback else messages
        count = 0
        for m in recent:
            for b in _ai_blocks(m):
                if isinstance(b, dict) and b.get("type") == "tool_use" \
                        and b.get("name", "") == _PORT_REQUEST_TOOL:
                    count += 1
        return count

    def _has_active_resources_in_history(messages, lookback=24):
        if not messages:
            return False
        recent = messages[-lookback:] if len(messages) > lookback else messages
        rtools = {_PORT_REQUEST_TOOL, "list_active_services", "service_restart"}
        for m in recent:
            for b in _ai_blocks(m):
                if isinstance(b, dict) and b.get("type") == "tool_use" \
                        and b.get("name", "") in rtools:
                    return True
        return False

    def _detect_recent_tool_error(messages, lookback=4):
        if not messages:
            return False
        checked = 0
        for m in reversed(messages):
            if checked >= lookback:
                break
            if not isinstance(m, ToolMessage):
                continue
            checked += 1
            if getattr(m, "status", "") == "error":
                return True
            content = getattr(m, "content", "")
            if isinstance(content, list):
                for c in content:
                    if isinstance(c, dict):
                        if c.get("is_error"):
                            return True
                        txt = str(c.get("text", "") or c.get("content", ""))
                        if any(h in txt.lower() for h in _TOOL_ERROR_HINTS):
                            return True
            else:
                if any(h in str(content).lower() for h in _TOOL_ERROR_HINTS):
                    return True
        return False
else:
    # Bind delle funzioni reali del modulo brain.
    has_filesystem_mutation_in_history = _h.has_filesystem_mutation_in_history
    _has_tool_calls_in_history = _h._has_tool_calls_in_history
    _tool_result_outcome_after = _h._tool_result_outcome_after
    _detect_repeated_failed_command = _h._detect_repeated_failed_command
    _detect_repeated_action = _h._detect_repeated_action
    _count_recent_request_port = _h._count_recent_request_port
    _has_active_resources_in_history = _h._has_active_resources_in_history
    _detect_recent_tool_error = _h._detect_recent_tool_error


# ── Costruttori dei messaggi dalla forma intermedia ──────────────────────────
def _mk(spec: dict):
    kind = spec["kind"]
    if kind == "ai_tool":
        block = {"type": "tool_use", "name": spec["name"], "input": spec.get("input", {})}
        return AIMessage(content="", additional_kwargs={"anthropic_content": [block]})
    if kind == "ai_text":
        return AIMessage(content=spec.get("text", ""))
    if kind == "tool":
        return ToolMessage(content=spec.get("text", ""), tool_call_id="golden")
    if kind == "human_result":
        block = {"type": "tool_result", "content": spec.get("text", "")}
        if spec.get("exit_code") is not None:
            block["exit_code"] = spec["exit_code"]
        if spec.get("is_error"):
            block["is_error"] = True
        return HumanMessage(content="", additional_kwargs={"anthropic_content": [block]})
    raise ValueError(f"kind sconosciuto: {kind}")


def main() -> None:
    cases = []

    def add(group, case_id, specs, output, lookback=None):
        cases.append({
            "group": group,
            "case_id": case_id,
            "messages": specs,
            "lookback": lookback,
            "output": output,
        })

    # Mattoni riusati.
    ai = lambda name, inp=None: {"kind": "ai_tool", "name": name, "input": inp or {}}  # noqa: E731
    txt = lambda t: {"kind": "ai_text", "text": t}  # noqa: E731
    tool = lambda t: {"kind": "tool", "text": t}  # noqa: E731
    hres = lambda ec=None, err=False, t="": {  # noqa: E731
        "kind": "human_result", "exit_code": ec, "is_error": err, "text": t,
    }

    # ── has_filesystem_mutation_in_history ───────────────────────────────────
    fs_cases = [
        ("write_si", [ai("write_file", {"path": "a"})], True),
        ("read_no", [ai("read_file", {"path": "a"})], False),
        ("rename_si", [ai("rename_file")], True),
        ("vuoto_no", [], False),
        ("misto", [ai("read_file"), txt("ok"), ai("run_command", {"command": "ls"})], True),
    ]
    for cid, specs, out in fs_cases:
        msgs = [_mk(s) for s in specs]
        add("has_filesystem_mutation", cid, specs, has_filesystem_mutation_in_history(msgs))

    # ── _has_tool_calls_in_history ───────────────────────────────────────────
    htc_cases = [
        ("ha_tool", [ai("read_file")], True),
        ("solo_testo", [txt("ciao")], False),
        ("vuoto", [], False),
    ]
    for cid, specs, _out in htc_cases:
        msgs = [_mk(s) for s in specs]
        add("has_tool_calls_in_history", cid, specs, _has_tool_calls_in_history(msgs))

    # ── _tool_result_outcome_after (idx=0, max_ahead=3) ──────────────────────
    out_cases = [
        ("exit0_ok", [ai("run_command", {"command": "ls"}), hres(0, False, "ok")]),
        ("exit2_err", [ai("run_command", {"command": "ls"}), hres(2, False, "tutto bene a parole")]),
        ("is_error", [ai("edit_file", {"path": "a"}), hres(None, True, "boh")]),
        ("lessicale_err", [ai("run_command", {"command": "x"}), hres(None, False, "bash: command not found")]),
        ("lessicale_ok", [ai("run_command", {"command": "x"}), hres(None, False, "Compilato con successo")]),
        ("nessun_result", [ai("run_command", {"command": "x"})]),
        ("toolmsg_err", [ai("run_command", {"command": "x"}), tool("error: failed")]),
        ("toolmsg_ok", [ai("run_command", {"command": "x"}), tool("done ok")]),
    ]
    for cid, specs in out_cases:
        msgs = [_mk(s) for s in specs]
        add("tool_result_outcome_after", cid, specs, _tool_result_outcome_after(msgs, 0))

    # ── _detect_repeated_failed_command (lookback=12) ────────────────────────
    rfc_cases = [
        ("ripetuto_2", [
            ai("run_command", {"command": "npm i", "working_dir": "/p"}), tool("error: build failed"),
            ai("run_command", {"command": "npm i", "working_dir": "/p"}), tool("error: build failed"),
        ]),
        ("riuscito_no", [
            ai("run_command", {"command": "npm i"}), tool("done ok"),
        ]),
        ("comandi_diversi", [
            ai("run_command", {"command": "a"}), tool("error: x"),
            ai("run_command", {"command": "b"}), tool("error: y"),
        ]),
        ("vuoto", []),
    ]
    for cid, specs in rfc_cases:
        msgs = [_mk(s) for s in specs]
        cmd, count = _detect_repeated_failed_command(msgs)
        add("detect_repeated_failed_command", cid, specs, {"command": cmd, "count": count})

    # ── _detect_repeated_action (lookback=24) ────────────────────────────────
    ra_cases = [
        ("falso_doppione", [
            ai("edit_file", {"path": "a.rs"}), hres(0, False, "applied"),
            ai("edit_file", {"path": "a.rs"}), hres(None, True, "old_string non trovato"),
        ]),
        ("stallo_mai_riuscito", [
            ai("write_file", {"path": "b.rs"}), hres(None, True, "permission denied"),
            ai("write_file", {"path": "b.rs"}), hres(None, True, "permission denied"),
        ]),
        ("singola_no_stallo", [ai("write_file", {"path": "c.rs"}), hres(0, False, "ok")]),
        ("non_tracciato", [ai("read_file", {"path": "d.rs"})]),
        ("vuoto", []),
    ]
    for cid, specs in ra_cases:
        msgs = [_mk(s) for s in specs]
        label, count = _detect_repeated_action(msgs)
        add("detect_repeated_action", cid, specs, {"label": label, "count": count})

    # ── _count_recent_request_port (lookback=16) ─────────────────────────────
    crp_cases = [
        ("due", [ai("request_port", {"label": "web"}), ai("request_port", {"label": "api"}), ai("read_file")], 2),
        ("zero", [ai("read_file")], 0),
    ]
    for cid, specs, _out in crp_cases:
        msgs = [_mk(s) for s in specs]
        add("count_recent_request_port", cid, specs, _count_recent_request_port(msgs))

    # ── _has_active_resources_in_history (lookback=24) ───────────────────────
    har_cases = [
        ("porta", [ai("request_port", {"label": "x"})], True),
        ("servizio", [ai("service_restart", {"name": "s"})], True),
        ("solo_read", [ai("read_file")], False),
    ]
    for cid, specs, _out in har_cases:
        msgs = [_mk(s) for s in specs]
        add("has_active_resources_in_history", cid, specs, _has_active_resources_in_history(msgs))

    # ── _detect_recent_tool_error (lookback=4) ───────────────────────────────
    dte_cases = [
        ("toolmsg_err", [tool("Error: build failed")]),
        ("toolmsg_ok", [tool("done ok")]),
        ("solo_ai_no", [ai("read_file")]),
        ("vuoto", []),
    ]
    for cid, specs in dte_cases:
        msgs = [_mk(s) for s in specs]
        add("detect_recent_tool_error", cid, specs, _detect_recent_tool_error(msgs))

    out_path = "/tmp/golden_executor_detectors.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    src = "brain reale" if _REAL else "replica byte-fedele"
    print(f"golden executor_detectors: {len(cases)} casi scritti in {out_path} (fonte: {src})")


if __name__ == "__main__":
    main()
