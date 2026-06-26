#!/usr/bin/env python3
"""Genera il golden di parita' 1:1 per la parte PURA di `context_reduction` Rust.

Esercita le funzioni di riduzione contesto del brain
(`brain/agents/nodes/helpers.py` + `__init__.py`) su casi rappresentativi e
salva `/tmp/golden_context_reduction.json` — lista di {group, case_id, input,
output} consumata dal test Rust
`decisions::context_reduction::golden::golden_context_reduction`.

Strategia di parita' (allineata a gen_golden_turn_focus.py):
  - Per le funzioni SENZA dipendenze I/O (should_compress_now, dedup x2,
    looks_like_base64, drop_unused_base64_payloads, i 5 _inject_*) si tenta
    l'import REALE dal brain con monkeypatch dei soli LOADER DB (config passata
    esplicita, regola G) e fallback a replica byte-fedele se langchain/brain non
    sono importabili.
  - Per `compress_old_tool_results` e `apply_token_brake` l'oracolo e' la
    REPLICA byte-fedele dell'algoritmo (allineata 1:1 al sorgente): la loro parte
    I/O (offload RAG via `_compress_marker`, conteggio token via
    `_estimate_context_tokens`/tiktoken) e' fuori dalla parte pura. Nel golden
    l'offload e' DISABILITATO (marker "degraded") e il token estimator e'
    DETERMINISTICO (somma dei char dei content stringa), esattamente come la
    callback iniettata nel test Rust. Questo rende il confronto 1:1 sulla
    DECISIONE/TRONCAMENTO puri, che e' cio' che il modulo Rust porta.

Output messaggi: ogni messaggio prodotto/atteso e' serializzato come
{is_human, content, anthropic_content, nexus_summary, rolling_summary} leggendo
`m.type == "human"`, `m.content`, `m.additional_kwargs`. I costruttori di dedup/
drop/compress ricreano `HumanMessage(content, additional_kwargs={anthropic_content})`.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_context_reduction.py
  cargo test -p nexus-agent-graph --lib golden_context_reduction -- --ignored
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)


# ── Classi messaggio minimali (forma BaseMessage usata dalle funzioni) ───────
class _Msg:
    def __init__(self, content=None, additional_kwargs=None, mtype="ai"):
        self.content = content if content is not None else ""
        self.additional_kwargs = additional_kwargs or {}
        self.type = mtype


def _human(content, anth=None, nexus_summary=False, rolling_summary=False):
    extra = {}
    if anth is not None:
        extra["anthropic_content"] = anth
    if nexus_summary:
        extra["nexus_summary"] = True
    if rolling_summary:
        extra["rolling_summary"] = True
    return _Msg(content=content, additional_kwargs=extra, mtype="human")


def _ai(content, anth=None):
    extra = {}
    if anth is not None:
        extra["anthropic_content"] = anth
    return _Msg(content=content, additional_kwargs=extra, mtype="ai")


def _spec_to_msg(spec: dict) -> _Msg:
    """Spec JSON -> _Msg. {is_human, content, anthropic_content, nexus_summary,
    rolling_summary}."""
    extra = {}
    anth = spec.get("anthropic_content")
    if anth is not None:
        extra["anthropic_content"] = anth
    if spec.get("nexus_summary"):
        extra["nexus_summary"] = True
    if spec.get("rolling_summary"):
        extra["rolling_summary"] = True
    return _Msg(
        content=spec.get("content", ""),
        additional_kwargs=extra,
        mtype="human" if spec.get("is_human") else "ai",
    )


def _msg_to_spec(m: _Msg) -> dict:
    extra = getattr(m, "additional_kwargs", {}) or {}
    return {
        "is_human": getattr(m, "type", None) == "human",
        "content": getattr(m, "content", ""),
        "anthropic_content": extra.get("anthropic_content"),
        "nexus_summary": bool(extra.get("nexus_summary")),
        "rolling_summary": bool(extra.get("rolling_summary")),
    }


# ── Replica byte-fedele dell'algoritmo (oracolo deterministico, DB/IO-free) ──
# Allineata 1:1 a helpers.py / __init__.py. Le funzioni che il modulo Rust porta
# sono pure; qui ne riproduciamo la semantica senza DB/offload/tiktoken.

def _fb_should_compress_now(iteration, cfg):
    start = int(cfg["compress_start_iter"])
    if iteration < start:
        return False, {"keep_recent": 0, "max_content_chars": 0}
    boundaries = list(cfg["compress_phase_boundaries"])
    keeps = list(cfg["compress_phase_keep_recent"])
    chars = list(cfg["compress_phase_max_chars"])
    idx = 0
    for i, b in enumerate(boundaries):
        if iteration >= b:
            idx = i
        else:
            break
    return True, {"keep_recent": int(keeps[idx]), "max_content_chars": int(chars[idx])}


def _fb_tool_use_signature(tool_name, args):
    args_json = json.dumps(args, sort_keys=True, default=str, ensure_ascii=False)
    payload = f"{tool_name}|{args_json}".encode("utf-8", errors="ignore")
    return hashlib.sha256(payload).hexdigest()[:16]


def _fb_dedup_tool_results_history(messages):
    tool_use_id_to_sig = {}
    for m in messages:
        content = getattr(m, "content", None)
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tid = str(block.get("id", "") or "")
                    if tid:
                        tool_use_id_to_sig[tid] = _fb_tool_use_signature(
                            str(block.get("name", "") or ""), block.get("input", {}) or {})
        extra = getattr(m, "additional_kwargs", {}) or {}
        anth = extra.get("anthropic_content")
        if isinstance(anth, list):
            for block in anth:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tid = str(block.get("id", "") or "")
                    if tid:
                        tool_use_id_to_sig[tid] = _fb_tool_use_signature(
                            str(block.get("name", "") or ""), block.get("input", {}) or {})
    last_pos_for_sig = {}
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            continue
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            tid = str(block.get("tool_use_id", "") or "")
            sig = tool_use_id_to_sig.get(tid)
            if not sig:
                continue
            last_pos_for_sig[sig] = (mi, bi)
    new_messages = []
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            tid = str(block.get("tool_use_id", "") or "")
            sig = tool_use_id_to_sig.get(tid)
            if sig is None:
                new_blocks.append(block)
                continue
            last = last_pos_for_sig.get(sig)
            if last is None or last == (mi, bi):
                new_blocks.append(block)
                continue
            new_blocks.append({
                "type": "tool_result",
                "tool_use_id": tid,
                "content": ("[dedup: stesso tool con stessi args, vedi risultato "
                            f"piu' recente in msg #{last[0]}]"),
            })
            changed = True
        if changed:
            new_messages.append(_human(getattr(m, "content", ""), anth=new_blocks))
        else:
            new_messages.append(m)
    return new_messages


def _fb_dedup_tool_results(messages):
    last_indices = {}
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            continue
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            content = block.get("content", "")
            if isinstance(content, list):
                content = " ".join(str(b.get("text", "")) for b in content
                                   if isinstance(b, dict) and b.get("type") == "text")
            if not isinstance(content, str) or len(content) < 200:
                continue
            normalized = content.strip()[:256]
            h = hashlib.sha1(normalized.encode("utf-8", errors="ignore")).hexdigest()[:16]
            last_indices[h] = (mi, bi)
    new_messages = []
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if isinstance(content, list):
                serialized = " ".join(str(b.get("text", "")) for b in content
                                      if isinstance(b, dict) and b.get("type") == "text")
            elif isinstance(content, str):
                serialized = content
            else:
                new_blocks.append(block)
                continue
            if not serialized or len(serialized) < 200:
                new_blocks.append(block)
                continue
            normalized = serialized.strip()[:256]
            h = hashlib.sha1(normalized.encode("utf-8", errors="ignore")).hexdigest()[:16]
            last_mi, last_bi = last_indices.get(h, (mi, bi))
            if (mi, bi) != (last_mi, last_bi):
                new_blocks.append({
                    **block,
                    "content": f"[deduped: contenuto identico al tool_result piu' recente in msg #{last_mi}]",
                })
                changed = True
            else:
                new_blocks.append(block)
        if changed:
            new_messages.append(_human(getattr(m, "content", ""), anth=new_blocks))
        else:
            new_messages.append(m)
    return new_messages


_FB_BASE64_RE = re.compile(r"[A-Za-z0-9+/=]")


def _fb_looks_like_base64(s, min_len=200):
    if not isinstance(s, str) or len(s) < min_len:
        return False
    if "\n" in s[:min_len]:
        return False
    sample = s if len(s) <= 4096 else s[:4096]
    valid = sum(1 for c in sample if _FB_BASE64_RE.match(c))
    return valid / max(len(sample), 1) >= 0.9


def _fb_drop_unused_base64_payloads(messages, max_age, keep_recent=2):
    if max_age <= 0 or len(messages) <= keep_recent:
        return messages
    boundary = len(messages) - keep_recent
    new_messages = []
    text_per_msg = []
    for m in messages:
        parts = []
        c = getattr(m, "content", "")
        if isinstance(c, str):
            parts.append(c)
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get("type") == "text":
                    parts.append(str(b.get("text", "")))
        extra = getattr(m, "additional_kwargs", {}) or {}
        anth = extra.get("anthropic_content")
        if isinstance(anth, list):
            for b in anth:
                if isinstance(b, dict):
                    bc = b.get("content")
                    if isinstance(bc, str):
                        parts.append(bc)
        text_per_msg.append(" ".join(parts))
    for mi, m in enumerate(messages):
        if mi >= boundary:
            new_messages.append(m)
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if not isinstance(content, str) or not _fb_looks_like_base64(content):
                new_blocks.append(block)
                continue
            prefix = content[:16]
            window_hi = min(len(messages), mi + 1 + max_age)
            cited = False
            for j in range(mi + 1, window_hi):
                if prefix in text_per_msg[j]:
                    cited = True
                    break
            if cited:
                new_blocks.append(block)
                continue
            orig_len = len(content)
            new_blocks.append({
                **block,
                "content": (f"[contenuto base64 originale di {orig_len} byte rimosso "
                            f"dalla history per ottimizzazione context. Se serve "
                            f"rileggilo con il tool originale.]"),
            })
            changed = True
        if changed:
            new_messages.append(_human(getattr(m, "content", ""), anth=new_blocks))
        else:
            new_messages.append(m)
    return new_messages


# Marker "degraded" (offload disabilitato), come la callback Rust degraded_marker.
def _fb_compress_marker(content):
    return f"\n[... compresso: {len(content)} char originali ...]"


def _fb_compress_old_tool_results(messages, keep_recent=6, max_content_chars=500, cutoff_index=None):
    if cutoff_index is None:
        messages = _fb_dedup_tool_results(messages)
        if len(messages) <= keep_recent:
            return messages
        boundary = len(messages) - keep_recent
    else:
        boundary = max(0, min(cutoff_index, len(messages)))
        if boundary == 0:
            return messages
    compressed = []
    recent_threshold = max_content_chars * 2
    for i, m in enumerate(messages):
        if i >= boundary:
            if cutoff_index is not None:
                compressed.append(m)
                continue
            extra = getattr(m, "additional_kwargs", {}) or {}
            blocks = extra.get("anthropic_content")
            if blocks is None or not isinstance(blocks, list):
                compressed.append(m)
                continue
            changed = False
            new_blocks = []
            for block in blocks:
                if not isinstance(block, dict) or block.get("type") != "tool_result":
                    new_blocks.append(block)
                    continue
                content = block.get("content", "")
                if isinstance(content, str) and len(content) > recent_threshold:
                    kept = max(recent_threshold // 2, 200)
                    new_blocks.append({**block, "content": content[:kept] + _fb_compress_marker(content)})
                    changed = True
                else:
                    new_blocks.append(block)
            if changed:
                compressed.append(_human(getattr(m, "content", ""), anth=new_blocks))
            else:
                compressed.append(m)
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if blocks is None or not isinstance(blocks, list):
            compressed.append(m)
            continue
        changed = False
        new_blocks = []
        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if isinstance(content, str) and len(content) > max_content_chars:
                kept = max(max_content_chars // 2, 100)
                new_blocks.append({**block, "content": content[:kept] + _fb_compress_marker(content)})
                changed = True
            else:
                new_blocks.append(block)
        if changed:
            compressed.append(_human(getattr(m, "content", ""), anth=new_blocks))
        else:
            compressed.append(m)
    return compressed


_FB_AGGRESSIVE_TRUNC_MARKER = "[...troncato per limite contesto...]"


def _fb_first_human_index(messages):
    for i, m in enumerate(messages):
        if getattr(m, "type", None) == "human":
            return i
    return -1


def _fb_is_summary_message(m):
    extra = getattr(m, "additional_kwargs", {}) or {}
    if extra.get("nexus_summary") or extra.get("rolling_summary"):
        return True
    content = getattr(m, "content", "")
    return isinstance(content, str) and content.lstrip().startswith("[RIASSUNTO")


def _fb_truncate_message_content(m, max_content_chars):
    changed = False
    extra = getattr(m, "additional_kwargs", {}) or {}
    blocks = extra.get("anthropic_content")
    if blocks is not None and isinstance(blocks, list):
        new_blocks = []
        for block in blocks:
            if not isinstance(block, dict):
                new_blocks.append(block)
                continue
            btype = block.get("type")
            if btype in ("text", "tool_result"):
                content_key = "content"
                if btype == "text":
                    content = block.get("text")
                    if isinstance(content, str):
                        content_key = "text"
                    else:
                        content = block.get("content", "")
                else:
                    content = block.get("content", "")
                if isinstance(content, str) and len(content) > max_content_chars:
                    kept = max(max_content_chars - len(_FB_AGGRESSIVE_TRUNC_MARKER), 50)
                    truncated = content[:kept] + _fb_compress_marker(content) + _FB_AGGRESSIVE_TRUNC_MARKER
                    new_blocks.append({**block, content_key: truncated})
                    changed = True
                    continue
                new_blocks.append(block)
            elif btype == "tool_use":
                tin = block.get("input")
                try:
                    tin_str = json.dumps(tin, ensure_ascii=False, default=str)
                except Exception:
                    tin_str = str(tin)
                if len(tin_str) > max_content_chars:
                    new_blocks.append({**block, "input": {"_truncated": tin_str[:max_content_chars] + _FB_AGGRESSIVE_TRUNC_MARKER}})
                    changed = True
                else:
                    new_blocks.append(block)
            else:
                new_blocks.append(block)
        if changed:
            nm = _human(getattr(m, "content", ""), anth=new_blocks)
            nm.type = getattr(m, "type", "ai")  # preserva is_human come la callback Rust
            return nm, True
        return m, False
    content = getattr(m, "content", "")
    if isinstance(content, str) and len(content) > max_content_chars:
        kept = max(max_content_chars - len(_FB_AGGRESSIVE_TRUNC_MARKER), 50)
        new_content = content[:kept] + _fb_compress_marker(content) + _FB_AGGRESSIVE_TRUNC_MARKER
        nm = _Msg(content=new_content, additional_kwargs=extra, mtype=getattr(m, "type", "ai"))
        return nm, True
    return m, False


def _fb_compress_aggressive_token_based(messages, keep_recent, max_content_chars):
    n = len(messages)
    if n <= keep_recent + 1:
        return messages, False
    first_human = _fb_first_human_index(messages)
    boundary = n - keep_recent
    out = []
    any_changed = False
    for i, m in enumerate(messages):
        if i >= boundary or i == first_human or _fb_is_summary_message(m):
            out.append(m)
            continue
        new_m, changed = _fb_truncate_message_content(m, max_content_chars)
        out.append(new_m)
        any_changed = any_changed or changed
    return out, any_changed


# Token estimator DETERMINISTICO (somma dei char dei content stringa), identico
# alla callback iniettata nel test Rust. NON tiktoken (I/O fuori dalla parte pura).
def _fb_token_estimator(messages):
    total = 0
    for m in messages:
        c = getattr(m, "content", "")
        if isinstance(c, str):
            total += len(c)
    return total


def _fb_apply_token_brake(messages, window, cfg, estimator):
    ratio = float(cfg["max_context_ratio"])
    keep_recent = int(cfg["aggressive_keep_recent"])
    max_chars = int(cfg["aggressive_max_chars"])
    threshold_tokens = int(window * ratio)
    est_tokens = estimator(messages)
    if est_tokens < threshold_tokens:
        return messages
    max_passes = 5
    for _ in range(max_passes):
        messages, changed = _fb_compress_aggressive_token_based(messages, keep_recent, max_chars)
        est_tokens = estimator(messages)
        if est_tokens < threshold_tokens or not changed:
            break
    if est_tokens >= window:
        first_human = _fb_first_human_index(messages)
        keep_idx = set(range(max(0, len(messages) - 2), len(messages)))
        if first_human >= 0:
            keep_idx.add(first_human)
        messages = [m for i, m in enumerate(messages) if i in keep_idx]
    return messages


# ── Iniezioni system (pure dato flag/text) ───────────────────────────────────
_LANG_REMINDER_MARKER = "[[NEXUS_LANG_REMINDER]]"
_TURN_FOCUS_MARKER = "[[NEXUS_TURN_FOCUS]]"
_VERIFY_DIRECTIVE_MARKER = "[[NEXUS_VERIFY_DIRECTIVE]]"
_RAG_REMINDER_MARKER = "[[NEXUS_FORCED_RAG_REMINDER]]"


def _fb_inject_language_reminder(system_text, enabled, reminder_text):
    if not enabled or not reminder_text:
        return system_text
    base_system = system_text or ""
    if _LANG_REMINDER_MARKER not in base_system:
        lang_block = f"### LINGUA RISPOSTA OBBLIGATORIA ###\n{reminder_text}"
        return f"{_LANG_REMINDER_MARKER}\n{lang_block}\n\n{base_system}\n\n{lang_block}"
    return system_text


def _fb_inject_turn_focus(system_text, directive):
    if not directive:
        return system_text
    base_system = system_text or ""
    if _TURN_FOCUS_MARKER in base_system:
        return system_text
    return f"{_TURN_FOCUS_MARKER}\n{directive}\n\n{base_system}"


def _fb_inject_verification_directive(system_text, detected, enabled, directive):
    if not detected:
        return system_text
    if not enabled or not directive:
        return system_text
    base = system_text or ""
    if _VERIFY_DIRECTIVE_MARKER in base:
        return system_text
    block = f"### AUTO-VERIFICA RICHIESTA DALL'UTENTE ###\n{directive}"
    return f"{base}\n\n{_VERIFY_DIRECTIVE_MARKER}\n{block}"


def _fb_inject_forced_rag_reminder(messages, system_text, est_tokens, window, ratio, reminder_text):
    if window <= 0 or est_tokens <= 0:
        return messages, system_text
    if ratio <= 0 or not reminder_text:
        return messages, system_text
    threshold = int(window * ratio)
    if est_tokens < threshold:
        return messages, system_text
    for msg in messages[-8:]:
        content = getattr(msg, "content", None)
        if isinstance(content, str) and _RAG_REMINDER_MARKER in content:
            return messages, system_text
    reminder_msg = _human(f"{_RAG_REMINDER_MARKER} ### RECUPERO ON-DEMAND DEL CONTESTO ###\n{reminder_text}")
    return list(messages) + [reminder_msg], system_text


# ── Tentativo di import REALE (solo funzioni IO-free), fallback alla replica ──
_REAL = {}
try:
    import langchain_core.messages  # noqa: F401  (verifica disponibilita')
    from brain.agents.nodes import helpers as _h  # type: ignore

    # Le funzioni reali leggono la config dal parametro o dai loader DB. Per
    # le funzioni che accettano la config esplicita (should_compress_now con
    # settings) la passiamo; per drop_unused_base64 passiamo max_age esplicito.
    _REAL["should_compress_now"] = _h._should_compress_now
    _REAL["dedup_tool_results_history"] = _h._dedup_tool_results_history
    _REAL["dedup_tool_results"] = _h._dedup_tool_results
    _REAL["looks_like_base64"] = _h._looks_like_base64
    _REAL["drop_unused_base64_payloads"] = _h._drop_unused_base64_payloads
    _REAL["inject_language_reminder"] = _h._inject_language_reminder
    _REAL["inject_turn_focus"] = _h._inject_turn_focus
    _REAL["inject_verification_directive"] = _h._inject_verification_directive
    _REAL["inject_forced_rag_reminder"] = _h._inject_forced_rag_reminder
    print("[gen_golden_context_reduction] uso funzioni REALI del brain (IO-free)")
except Exception as _exc:  # noqa: BLE001
    print(f"[gen_golden_context_reduction] import brain non disponibile ({_exc}); replica byte-fedele")


# ── Dispatcher: reale se disponibile e IO-free, altrimenti fallback ──────────
def _call_real_or_fb(name, fb_fn, *args, **kwargs):
    fn = _REAL.get(name)
    if fn is None:
        return fb_fn(*args, **kwargs)
    return fn(*args, **kwargs)


def main() -> None:
    cases = []

    # ── group: should_compress_now ───────────────────────────────────────────
    cfg = {
        "compress_start_iter": 5,
        "compress_phase_boundaries": [5, 10, 20, 50],
        "compress_phase_keep_recent": [8, 5, 3, 2],
        "compress_phase_max_chars": [2000, 1000, 500, 150],
    }
    for it in [0, 4, 5, 9, 10, 19, 20, 49, 50, 100]:
        if _REAL.get("should_compress_now"):
            comp, params = _REAL["should_compress_now"](it, cfg)
        else:
            comp, params = _fb_should_compress_now(it, cfg)
        cases.append({
            "group": "should_compress_now",
            "case_id": f"iter_{it}",
            "input": {"iteration": it, "config": cfg},
            "output": {"compress": comp, "keep_recent": params["keep_recent"],
                       "max_content_chars": params["max_content_chars"]},
        })

    # ── group: dedup_tool_results_history ─────────────────────────────────────
    def tu(tid, name="read_file", inp=None):
        return {"type": "tool_use", "id": tid, "name": name, "input": inp or {"path": "a.rs"}}

    def tr(tid, body):
        return {"type": "tool_result", "tool_use_id": tid, "content": body}

    dedup_hist_cases = [
        ("nessun_dup", [
            {"is_human": False, "anthropic_content": [tu("t1", inp={"path": "a.rs"})]},
            {"is_human": False, "anthropic_content": [tr("t1", "uno")]},
            {"is_human": False, "anthropic_content": [tu("t2", inp={"path": "b.rs"})]},
            {"is_human": False, "anthropic_content": [tr("t2", "due")]},
        ]),
        ("dup_stessi_args", [
            {"is_human": False, "anthropic_content": [tu("t1", inp={"path": "a.rs"})]},
            {"is_human": False, "anthropic_content": [tr("t1", "vecchio")]},
            {"is_human": False, "anthropic_content": [tu("t2", inp={"path": "a.rs"})]},
            {"is_human": False, "anthropic_content": [tr("t2", "recente")]},
        ]),
        ("args_ordine_diverso_stessa_sig", [
            {"is_human": False, "anthropic_content": [tu("t1", inp={"path": "a.rs", "offset": 1})]},
            {"is_human": False, "anthropic_content": [tr("t1", "x")]},
            {"is_human": False, "anthropic_content": [tu("t2", inp={"offset": 1, "path": "a.rs"})]},
            {"is_human": False, "anthropic_content": [tr("t2", "y")]},
        ]),
        # content-LISTA: dedup_tool_results_history dedupa per signature del
        # tool_use (non guarda il content). Il tool_result precedente con
        # content-lista deve essere sostituito dal marker, l'ultimo preservato.
        ("dup_stessi_args_content_lista", [
            {"is_human": False, "anthropic_content": [tu("t1", inp={"path": "a.rs"})]},
            {"is_human": False, "anthropic_content": [
                {"type": "tool_result", "tool_use_id": "t1",
                 "content": [{"type": "text", "text": "vecchio " + "x" * 220}]}]},
            {"is_human": False, "anthropic_content": [tu("t2", inp={"path": "a.rs"})]},
            {"is_human": False, "anthropic_content": [
                {"type": "tool_result", "tool_use_id": "t2",
                 "content": [{"type": "text", "text": "recente " + "y" * 220}]}]},
        ]),
        ("vuoto", []),
        ("solo_human", [{"is_human": True, "content": "ciao"}]),
    ]
    for cid, specs in dedup_hist_cases:
        msgs = [_spec_to_msg(s) for s in specs]
        out = _call_real_or_fb("dedup_tool_results_history", _fb_dedup_tool_results_history, msgs)
        cases.append({
            "group": "dedup_tool_results_history",
            "case_id": cid,
            "input": {"messages": specs},
            "output": [_msg_to_spec(m) for m in out],
        })

    # ── group: dedup_tool_results (legacy per content) ────────────────────────
    big = "z" * 300
    small = "k" * 100

    # content-LISTA (formato Anthropic standard: [{"type":"text","text":...}]).
    # Il Python serializza la lista con " ".join dei text block in ENTRAMBE le
    # passate (helpers.py:2862-2872 e 2889-2904), quindi una lista identica >=200
    # char dedupa esattamente come una stringa. Regression guard per il bug della
    # guardia is_str nella passata 1 Rust.
    def tr_list(tid, *texts):
        return {"type": "tool_result", "tool_use_id": tid,
                "content": [{"type": "text", "text": t} for t in texts]}

    big_list_text = "w" * 300  # serializzato = 300 char >= 200
    small_list_text = "q" * 100  # serializzato = 100 char < 200
    dedup_cases = [
        ("dup_content_grande", [
            {"is_human": False, "anthropic_content": [tr("t1", big)]},
            {"is_human": False, "anthropic_content": [tr("t2", big)]},
        ]),
        ("content_piccolo_no_dedup", [
            {"is_human": False, "anthropic_content": [tr("t1", small)]},
            {"is_human": False, "anthropic_content": [tr("t2", small)]},
        ]),
        ("content_diverso", [
            {"is_human": False, "anthropic_content": [tr("t1", "a" * 250)]},
            {"is_human": False, "anthropic_content": [tr("t2", "b" * 250)]},
        ]),
        # (a) due tool_result con content-LISTA identico >=200 char: il primo
        # diventa [deduped], l'ultimo resta intatto (lista).
        ("dup_content_lista_grande", [
            {"is_human": False, "anthropic_content": [tr_list("t1", big_list_text)]},
            {"is_human": False, "anthropic_content": [tr_list("t2", big_list_text)]},
        ]),
        # (b) content-LISTA <200 char: nessuna dedup.
        ("content_lista_piccolo_no_dedup", [
            {"is_human": False, "anthropic_content": [tr_list("t1", small_list_text)]},
            {"is_human": False, "anthropic_content": [tr_list("t2", small_list_text)]},
        ]),
        # (c) content-LISTA diverso: nessuna dedup.
        ("content_lista_diverso", [
            {"is_human": False, "anthropic_content": [tr_list("t1", "a" * 250)]},
            {"is_human": False, "anthropic_content": [tr_list("t2", "b" * 250)]},
        ]),
        # Mix: stringa e lista che serializzano allo STESSO testo >=200 char ->
        # stesso hash -> la prima (stringa) e' deduplicata, l'ultima (lista) resta.
        ("dup_mix_stringa_e_lista", [
            {"is_human": False, "anthropic_content": [tr("t1", "m" * 300)]},
            {"is_human": False, "anthropic_content": [tr_list("t2", "m" * 300)]},
        ]),
    ]
    for cid, specs in dedup_cases:
        msgs = [_spec_to_msg(s) for s in specs]
        out = _call_real_or_fb("dedup_tool_results", _fb_dedup_tool_results, msgs)
        cases.append({
            "group": "dedup_tool_results",
            "case_id": cid,
            "input": {"messages": specs},
            "output": [_msg_to_spec(m) for m in out],
        })

    # ── group: looks_like_base64 ──────────────────────────────────────────────
    b64 = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5" * 5
    lb64_cases = [
        ("base64_valido", b64),
        ("troppo_corto", "QUJD"),
        ("con_newline", "riga\n" + "A" * 300),
        ("prosa", "questo e' un testo normale con spazi, niente base64 affatto. " * 5),
        ("esattamente_min_len_b64", "A" * 200),
    ]
    for cid, s in lb64_cases:
        if _REAL.get("looks_like_base64"):
            out = _REAL["looks_like_base64"](s)
        else:
            out = _fb_looks_like_base64(s)
        cases.append({
            "group": "looks_like_base64",
            "case_id": cid,
            "input": {"s": s, "min_len": 200},
            "output": bool(out),
        })

    # ── group: drop_unused_base64_payloads ────────────────────────────────────
    prefix16 = b64[:16]
    drop_cases = [
        ("orfano_sostituito", 3, 2, [
            {"is_human": False, "anthropic_content": [tr("t1", b64)]},
            {"is_human": True, "content": "nessuna citazione"},
            {"is_human": True, "content": "recente1"},
            {"is_human": True, "content": "recente2"},
        ]),
        ("referenziato_intatto", 3, 2, [
            {"is_human": False, "anthropic_content": [tr("t1", b64)]},
            {"is_human": True, "content": f"riferimento {prefix16} qui"},
            {"is_human": True, "content": "recente1"},
            {"is_human": True, "content": "recente2"},
        ]),
        ("keep_recent_protegge", 3, 2, [
            {"is_human": False, "anthropic_content": [tr("t1", b64)]},
            {"is_human": True, "content": "recente1"},
        ]),
        ("max_age_zero_noop", 0, 2, [
            {"is_human": False, "anthropic_content": [tr("t1", b64)]},
            {"is_human": True, "content": "a"},
            {"is_human": True, "content": "b"},
            {"is_human": True, "content": "c"},
        ]),
    ]
    for cid, max_age, keep_recent, specs in drop_cases:
        msgs = [_spec_to_msg(s) for s in specs]
        out = _call_real_or_fb("drop_unused_base64_payloads", _fb_drop_unused_base64_payloads,
                               msgs, max_age, keep_recent=keep_recent)
        cases.append({
            "group": "drop_unused_base64_payloads",
            "case_id": cid,
            "input": {"messages": specs, "max_age": max_age, "keep_recent": keep_recent},
            "output": [_msg_to_spec(m) for m in out],
        })

    # ── group: compress_old_tool_results (oracolo replica, offload degraded) ──
    big2 = "y" * 1200
    compress_cases = [
        ("gen_sotto_cutoff", 6, 500, 1, [
            {"is_human": False, "anthropic_content": [tr("t1", big2)]},
            {"is_human": False, "anthropic_content": [tr("t2", big2)]},
        ]),
        ("gen_cutoff_zero", 6, 500, 0, [
            {"is_human": False, "anthropic_content": [tr("t1", big2)]},
        ]),
        ("gen_sotto_soglia", 6, 500, 1, [
            {"is_human": False, "anthropic_content": [tr("t1", "k" * 100)]},
        ]),
        ("legacy_dedup_e_compress", 1, 500, None, [
            {"is_human": False, "anthropic_content": [tr("t1", big2)]},
            {"is_human": False, "anthropic_content": [tr("t2", big2)]},
            {"is_human": True, "content": "recente"},
        ]),
    ]
    for cid, keep_recent, max_chars, cutoff, specs in compress_cases:
        msgs = [_spec_to_msg(s) for s in specs]
        out = _fb_compress_old_tool_results(msgs, keep_recent=keep_recent,
                                            max_content_chars=max_chars, cutoff_index=cutoff)
        cases.append({
            "group": "compress_old_tool_results",
            "case_id": cid,
            "input": {"messages": specs, "keep_recent": keep_recent,
                      "max_content_chars": max_chars, "cutoff_index": cutoff},
            "output": [_msg_to_spec(m) for m in out],
        })

    # ── group: apply_token_brake (oracolo replica, estimator deterministico) ──
    brake_cfg = {"max_context_ratio": 0.5, "aggressive_keep_recent": 1, "aggressive_max_chars": 50}
    brake_cases = [
        ("sotto_soglia_noop", 1000, brake_cfg, [
            {"is_human": True, "content": "breve"},
        ]),
        ("comprime_aggressivo", 200, brake_cfg, [
            {"is_human": True, "content": "a" * 100},
            {"is_human": False, "content": "b" * 400},
            {"is_human": False, "content": "c" * 400},
            {"is_human": True, "content": "d" * 50},
        ]),
        ("cap_hard", 100, {"max_context_ratio": 0.1, "aggressive_keep_recent": 1, "aggressive_max_chars": 50}, [
            {"is_human": True, "content": "x" * 200},
            {"is_human": False, "content": "y" * 200},
            {"is_human": False, "content": "z" * 200},
        ]),
        # truncate_message_content con content-LISTA: a differenza di dedup, il
        # path di troncamento tronca SOLO content/text quando e' isinstance str
        # (helpers.py:1413). Un text-block o un tool_result con content LISTA non
        # e' stringa -> NON viene toccato. Caso che lo dimostra: il messaggio AI
        # con content stringa lungo viene compresso (estimator lo conta), mentre i
        # blocchi-lista nel messaggio successivo restano intatti.
        ("comprime_ma_lista_intatta", 200, brake_cfg, [
            {"is_human": True, "content": "p" * 100},
            {"is_human": False, "content": "s" * 400},
            {"is_human": False, "content": "t" * 100, "anthropic_content": [
                {"type": "text", "text": ["nonstringa", "lista"]},
                {"type": "tool_result", "tool_use_id": "tL", "content": [
                    {"type": "text", "text": "blocco lista lungo " + "L" * 400}]},
            ]},
            {"is_human": True, "content": "u" * 50},
        ]),
    ]
    for cid, window, bcfg, specs in brake_cases:
        msgs = [_spec_to_msg(s) for s in specs]
        out = _fb_apply_token_brake(msgs, window, bcfg, _fb_token_estimator)
        cases.append({
            "group": "apply_token_brake",
            "case_id": cid,
            "input": {"messages": specs, "window": window, "config": bcfg},
            "output": [_msg_to_spec(m) for m in out],
        })

    # ── group: inject_language_reminder ───────────────────────────────────────
    lang_cases = [
        ("attivo", "SYSTEM BASE", True, "rispondi in italiano"),
        ("disabilitato", "SYSTEM BASE", False, "rispondi in italiano"),
        ("testo_vuoto", "SYSTEM BASE", True, ""),
        ("idempotente", f"{_LANG_REMINDER_MARKER}\ngia presente", True, "rispondi in italiano"),
    ]
    for cid, sys_text, enabled, text in lang_cases:
        out = _call_real_or_fb("inject_language_reminder", _fb_inject_language_reminder,
                               [], sys_text, enabled, text) if _REAL.get("inject_language_reminder") \
            else _fb_inject_language_reminder(sys_text, enabled, text)
        # La reale ritorna (messages, system_text); la fb ritorna solo system_text.
        if isinstance(out, tuple):
            out = out[1]
        cases.append({
            "group": "inject_language_reminder",
            "case_id": cid,
            "input": {"system_text": sys_text, "enabled": enabled, "reminder_text": text},
            "output": out,
        })

    # ── group: inject_turn_focus ──────────────────────────────────────────────
    directive = ("### FOCUS DEL TURNO CORRENTE ###\nLa richiesta da portare a "
                 "termine ADESSO e' l'ultimo messaggio dell'utente:\n\"crea index.html\"")
    turn_focus_cases = [
        ("inietta", "SYS", directive),
        ("directive_vuota", "SYS", ""),
        ("idempotente", f"{_TURN_FOCUS_MARKER}\n{directive}\n\nSYS", directive),
    ]
    for cid, sys_text, direc in turn_focus_cases:
        if _REAL.get("inject_turn_focus"):
            out = _REAL["inject_turn_focus"]([], sys_text, direc)
        else:
            out = _fb_inject_turn_focus(sys_text, direc)
        if isinstance(out, tuple):
            out = out[1]
        cases.append({
            "group": "inject_turn_focus",
            "case_id": cid,
            "input": {"system_text": sys_text, "directive": direc},
            "output": out,
        })

    # ── group: inject_verification_directive ──────────────────────────────────
    vdir = "esegui la verifica reale e riporta l'esito"
    verif_cases = [
        ("rilevato_attivo", "SYS", True, True, vdir),
        ("non_rilevato", "SYS", False, True, vdir),
        ("disabilitato", "SYS", True, False, vdir),
        ("directive_vuota", "SYS", True, True, ""),
        ("idempotente", f"SYS\n\n{_VERIFY_DIRECTIVE_MARKER}\nx", True, True, vdir),
    ]
    for cid, sys_text, detected, enabled, direc in verif_cases:
        if _REAL.get("inject_verification_directive"):
            # La reale accetta (system_text, first_human_text): NON e' la stessa
            # firma pura. Usiamo SEMPRE il fallback (parita' sulla parte pura).
            out = _fb_inject_verification_directive(sys_text, detected, enabled, direc)
        else:
            out = _fb_inject_verification_directive(sys_text, detected, enabled, direc)
        cases.append({
            "group": "inject_verification_directive",
            "case_id": cid,
            "input": {"system_text": sys_text, "detected": detected, "enabled": enabled, "directive": direc},
            "output": out,
        })

    # ── group: inject_forced_rag_reminder ─────────────────────────────────────
    rag_text = "usa nexus_search_semantic prima di rispondere"
    rag_cases = [
        ("sopra_soglia_appende", [{"is_human": True, "content": "ciao"}], "SYS", 80, 100, 0.5, rag_text),
        ("sotto_soglia_noop", [{"is_human": True, "content": "ciao"}], "SYS", 10, 100, 0.5, rag_text),
        ("ratio_zero", [{"is_human": True, "content": "ciao"}], "SYS", 80, 100, 0.0, rag_text),
        ("text_vuoto", [{"is_human": True, "content": "ciao"}], "SYS", 80, 100, 0.5, ""),
        ("window_zero", [{"is_human": True, "content": "ciao"}], "SYS", 80, 0, 0.5, rag_text),
        ("idempotente_marker_recente", [
            {"is_human": True, "content": f"{_RAG_REMINDER_MARKER} gia iniettato"},
        ], "SYS", 80, 100, 0.5, rag_text),
    ]
    for cid, specs, sys_text, est, window, ratio, text in rag_cases:
        msgs = [_spec_to_msg(s) for s in specs]
        if _REAL.get("inject_forced_rag_reminder"):
            out_msgs, out_sys = _REAL["inject_forced_rag_reminder"](msgs, sys_text, est, window)
            # La reale legge ratio/text dal DB: per la parita' pura usiamo il fb,
            # che riceve ratio/text espliciti (stessa decisione, no DB).
            out_msgs, out_sys = _fb_inject_forced_rag_reminder(msgs, sys_text, est, window, ratio, text)
        else:
            out_msgs, out_sys = _fb_inject_forced_rag_reminder(msgs, sys_text, est, window, ratio, text)
        cases.append({
            "group": "inject_forced_rag_reminder",
            "case_id": cid,
            "input": {"messages": specs, "system_text": sys_text, "est_tokens": est,
                      "window": window, "ratio": ratio, "reminder_text": text},
            "output": {"messages": [_msg_to_spec(m) for m in out_msgs], "system_text": out_sys},
        })

    out_path = "/tmp/golden_context_reduction.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden context_reduction: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
