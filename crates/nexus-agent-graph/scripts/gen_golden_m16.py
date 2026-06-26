#!/usr/bin/env python3
"""Golden di parita' 1:1 per `decisions::m16` (gate validazione tool-in-list
discovery-first + parser dei tool scoperti) del crate `nexus-agent-graph`.

Replica i blocchi M16 del tool_dispatch_node in `brain/agents/nodes/__init__.py`:

  - _M16_ALLOWED = _M16_META_TOOLS | _df_whitelist | _ALWAYS_ON_TOOLS | {task_complete, run_notes}
    (build_m16_allowed: pura unione di insiemi gia' risolti)
  - validazione: ammesso se `name in _M16_ALLOWED or name in discovered`
    (is_tool_allowed)
  - parser nexus_mcp_tool_search (~4129-4164): JSON INTEGRO -> results -> per ogni dict
    name=tool_name|name, input_schema o default, cap len(json.dumps(schema))>max_bytes,
    description[:500], DEDUP per nome (prima occorrenza vince).
  - accumulo run (~4187-4196): dict per nome, ultimo schema vince (merge_discovered_run).

Questa logica e' embedded nel nodo (non funzioni isolate importabili senza DB), quindi
la riproduciamo 1:1 nello script come fonte di verita' del comportamento Python.

INCIDENTE STORICO (loop M16 da truncation): il parser legge il raw_content INTEGRO
(pre-troncamento). CASO CRUCIALE nel golden: JSON TRONCATO -> 0 tool, senza eccezione
propagata (Python: try/except -> continue). Gli schemi nei casi sono ASCII puro: per
ASCII `len(json.dumps(schema))` (ensure_ascii=True, default) == lunghezza byte della
serializzazione py_json_dumps(ensure_ascii=False) usata in Rust.

Output: /tmp/golden_m16.json — lista di {group, case_id, input, output}.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_m16.py
  cargo test -p nexus-agent-graph --lib golden_m16 -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

M16_META_TOOLS = {"nexus_mcp_tool_search", "nexus_mcp_tool_call"}


def build_m16_allowed(meta, whitelist, always_on, brain_tools):
    return set(meta) | set(whitelist) | set(always_on) | set(brain_tools)


def is_tool_allowed(name, allowed, discovered):
    return name in allowed or name in discovered


def parse_discovered_tools(raw_json, schema_max_bytes):
    discovered_next = []
    try:
        payload = json.loads(raw_json or "{}")
    except Exception:
        return discovered_next
    for _res in (payload.get("results") or []):
        if not isinstance(_res, dict):
            continue
        _name = _res.get("tool_name") or _res.get("name")
        if not _name:
            continue
        _schema = _res.get("input_schema") or {"type": "object", "properties": {}}
        try:
            if len(json.dumps(_schema)) > schema_max_bytes:
                _schema = {"type": "object", "properties": {}}
        except Exception:
            _schema = {"type": "object", "properties": {}}
        if not any(d.get("name") == _name for d in discovered_next):
            discovered_next.append({
                "name": _name,
                "description": (_res.get("description") or "")[:500],
                "input_schema": _schema,
            })
    return discovered_next


def merge_discovered_run(previous, discovered_next):
    run_tools = {
        t.get("name"): t
        for t in (previous or [])
        if isinstance(t, dict) and t.get("name")
    }
    for _t in discovered_next:
        if isinstance(_t, dict) and _t.get("name"):
            run_tools[_t["name"]] = _t
    return list(run_tools.values())


def main() -> None:
    cases = []

    # ── build_m16_allowed (output ordinato per confronto deterministico) ──────
    allowed_inputs = [
        ("base", ["nexus_mcp_tool_search", "nexus_mcp_tool_call"],
         ["read_file", "list_files"], ["write_file", "edit_file"],
         ["task_complete", "nexus_run_notes"]),
        ("dup_across_sets", ["nexus_mcp_tool_search"], ["read_file", "read_file"],
         ["read_file"], ["task_complete"]),
        ("empty_whitelist", ["nexus_mcp_tool_search"], [], ["write_file"], ["task_complete"]),
    ]
    for cid, meta, wl, ao, bt in allowed_inputs:
        allowed = build_m16_allowed(meta, wl, ao, bt)
        cases.append({
            "group": "build_m16_allowed", "case_id": cid,
            "input": {"meta": meta, "whitelist": wl, "always_on": ao, "brain_tools": bt},
            "output": sorted(allowed),
        })

    # ── is_tool_allowed ───────────────────────────────────────────────────────
    allowed_set = ["read_file", "task_complete"]
    disc_set = ["nexus_foo"]
    is_allowed_inputs = [
        ("in_allowed", "read_file"),
        ("in_discovered", "nexus_foo"),
        ("neither", "nexus_bar"),
    ]
    for cid, name in is_allowed_inputs:
        out = is_tool_allowed(name, set(allowed_set), set(disc_set))
        cases.append({
            "group": "is_tool_allowed", "case_id": cid,
            "input": {"name": name, "allowed": allowed_set, "discovered": disc_set},
            "output": out,
        })

    # ── parse_discovered_tools ────────────────────────────────────────────────
    big_props = {f"p{i}": {} for i in range(2000)}
    parse_inputs = [
        ("normale", json.dumps({"results": [
            {"tool_name": "a", "description": "da", "input_schema": {"type": "object"}},
            {"name": "b"},
        ]}), 8192),
        ("vuoto", json.dumps({"results": []}), 8192),
        ("no_results_key", json.dumps({"other": 1}), 8192),
        ("troncato", '{"results":[{"tool_name":"a","input_sch', 8192),
        ("malformato", "non json affatto", 8192),
        ("dedup_prima_vince", json.dumps({"results": [
            {"tool_name": "dup", "description": "primo"},
            {"tool_name": "dup", "description": "secondo"},
        ]}), 8192),
        ("name_vuoto_skip", json.dumps({"results": [
            {"tool_name": "", "description": "vuoto"},
            {"name": "valido"},
        ]}), 8192),
        ("non_dict_skip", json.dumps({"results": ["stringa", {"tool_name": "ok"}]}), 8192),
        ("schema_oversize", json.dumps({"results": [
            {"tool_name": "big", "input_schema": {"properties": big_props}},
        ]}), 100),
        ("desc_truncata", json.dumps({"results": [
            {"tool_name": "long", "description": "x" * 600},
        ]}), 8192),
    ]
    for cid, raw, mx in parse_inputs:
        out = parse_discovered_tools(raw, mx)
        cases.append({
            "group": "parse_discovered_tools", "case_id": cid,
            "input": {"raw_json": raw, "schema_max_bytes": mx},
            "output": out,
        })

    # ── merge_discovered_run (ultimo vince, insertion-order) ──────────────────
    merge_inputs = [
        ("ultimo_vince",
         [{"name": "a", "description": "vecchio a", "input_schema": {}},
          {"name": "b", "description": "b", "input_schema": {}}],
         [{"name": "a", "description": "nuovo a", "input_schema": {}},
          {"name": "c", "description": "c", "input_schema": {}}]),
        ("prev_vuoto", [],
         [{"name": "x", "description": "x", "input_schema": {}}]),
        ("next_vuoto",
         [{"name": "y", "description": "y", "input_schema": {}}], []),
    ]
    for cid, prev, nxt in merge_inputs:
        out = merge_discovered_run(prev, nxt)
        cases.append({
            "group": "merge_discovered_run", "case_id": cid,
            "input": {"previous": prev, "discovered_next": nxt},
            "output": out,
        })

    out_path = "/tmp/golden_m16.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden m16: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
