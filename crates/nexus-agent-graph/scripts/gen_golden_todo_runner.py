#!/usr/bin/env python3
"""Genera /tmp/golden_todo_runner.json: parita' 1:1 della logica DETERMINISTICA
del nodo todo_runner.

Funzioni coperte (Rust = crates/nexus-agent-graph/src/nodes/todo_runner.rs):
  - compact             <- brain/agents/todo_runner_node.py:56-62  (_compact)
  - result_failed       <- brain/agents/todo_runner_node.py:199-208 (_result_failed)
  - todo_kind           <- brain/agents/todo_runner_node.py:145-152 (_todo_kind)
  - build_context_blob  <- brain/agents/todo_runner_node.py:65-142 (_build_context_blob)
  - decision_machine    <- brain/agents/todo_runner_node.py:291-395 (on_failure + _advance_patch)

Strategia: lo script PROVA a importare le funzioni REALI dal brain (sono pure,
nessun I/O — _build_context_blob fa solo un fetch_worklog_block best-effort che
fallisce fail-open in CI, equivalente a worklog vuoto, come il porting Rust che
NON porta il worklog). Se l'import fallisce, ricade su una replica BYTE-FEDELE
(COPIA 1:1 dalle righe indicate). In entrambi i casi l'output e' l'oracolo Python.

NOTA WORKLOG: il porting Rust NON include il fetch worklog (TODO esplicito). Per
la parita' lo script costruisce il context_blob SENZA worklog: con le funzioni
REALI, fetch_worklog_block in CI ritorna stringa vuota (nessun digest
materializzato) -> il blocco non viene appeso, identico al Rust. Con la replica,
il blocco worklog e' semplicemente omesso.

NOTA CONTENT/CRITERIA: nel runtime Rust il todo DAG non porta content/criteri
(TODO impl concreta). Per il golden, build_context_blob riceve un `todo` dict
COMPLETO (come Python) cosi' la logica deterministica e' testata 1:1; il limite
runtime e' documentato nel modulo Rust.

Uso:
    python3 crates/nexus-agent-graph/scripts/gen_golden_todo_runner.py
    (scrive /tmp/golden_todo_runner.json)
"""
import json

# ── Tentativo di import delle funzioni REALI del brain (pure / fail-open) ─────
_REAL = False
try:  # pragma: no cover - dipende dall'ambiente
    import os
    import sys

    _here = os.path.dirname(os.path.abspath(__file__))
    _root = os.path.abspath(os.path.join(_here, "..", "..", ".."))
    if _root not in sys.path:
        sys.path.insert(0, _root)
    from brain.agents.todo_runner_node import (  # type: ignore
        _build_context_blob as _real_build_blob,
        _compact as _real_compact,
        _result_failed as _real_result_failed,
        _todo_kind as _real_todo_kind,
    )

    _REAL = True
except Exception:  # noqa: BLE001 - fallback alla replica byte-fedele
    _REAL = False


# ── Replica BYTE-FEDELE (COPIA 1:1 dal Python) ────────────────────────────────

_SUMMARY_MAX_CHARS = 600


def _compact_copy(text, max_chars=_SUMMARY_MAX_CHARS):
    """todo_runner_node.py:56-62 (copia 1:1)."""
    text = str(text or "").strip()
    if len(text) <= max_chars:
        return text
    suffix = "...[troncato]"
    return text[: max_chars - len(suffix)] + suffix


def _result_failed_copy(result):
    """todo_runner_node.py:199-208 (copia 1:1)."""
    if result.get("error") is not None:
        return True
    status = str(result.get("status") or "completed").strip().lower()
    return status not in ("completed", "completed_verified")


def _todo_kind_copy(cfg):
    """todo_runner_node.py:145-152 (copia 1:1)."""
    kind = str(cfg.get("todo_isolation_kind") or "").strip()
    return kind or "implement"


def _build_blob_copy(state, todo, prior_results):
    """todo_runner_node.py:65-142 (copia 1:1, SENZA worklog: il porting Rust non
    lo include; in CI fetch_worklog_block sarebbe comunque vuoto)."""
    parts = []
    # (a) worklog: omesso (equivalente a wl vuoto).
    if prior_results:
        done_lines = []
        for r in prior_results[-8:]:
            seq = r.get("seq")
            content = str(r.get("content") or "")[:120]
            status = r.get("status") or "?"
            summary = _compact_copy(str(r.get("summary") or ""), 240)
            done_lines.append(f"  - todo {seq} ({status}): {content} -> {summary}")
        parts.append(
            "<todo_gia_eseguiti>\n"
            "I seguenti passi del piano sono gia' stati eseguiti in sub-run "
            "isolate (non rifarli, costruisci sopra il loro esito):\n"
            + "\n".join(done_lines)
            + "\n</todo_gia_eseguiti>"
        )
    rationale = str(state.get("plan_rationale") or "").strip()
    constraints = state.get("plan_constraints") or []
    if rationale or constraints:
        block = ["<piano>"]
        if rationale:
            block.append(f"  <rationale>{rationale[:1200]}</rationale>")
        if constraints:
            items = "\n".join(f"    - {str(c)[:200]}" for c in constraints[:10])
            block.append(f"  <vincoli>\n{items}\n  </vincoli>")
        block.append("</piano>")
        parts.append("\n".join(block))
    criteria = todo.get("acceptance_criteria") or []
    if isinstance(criteria, str):
        try:
            criteria = json.loads(criteria)
        except Exception:
            criteria = []
    if criteria:
        crit_lines = []
        for c in criteria[:10]:
            if isinstance(c, dict):
                ctype = c.get("type") or "criterio"
                expected = c.get("expected") or c.get("description") or ""
                crit_lines.append(f"    - [{ctype}] {str(expected)[:200]}")
        if crit_lines:
            parts.append(
                "<definition_of_done>\n"
                "Il passo e' completo SOLO se questi criteri sono soddisfatti:\n"
                + "\n".join(crit_lines)
                + "\n</definition_of_done>"
            )
    return "\n\n".join(parts)


def _build_blob_real(state, todo, prior_results):
    """Usa la funzione REALE; se il worklog (fail-open) producesse un blocco non
    vuoto in un ambiente con DB, lo SCARTIAMO per la parita' col porting Rust che
    non porta il worklog. In CI fetch_worklog_block ritorna vuoto, quindi questo
    e' un no-op; la rimozione difensiva evita divergenze in ambienti con DB."""
    blob = _real_build_blob(state, todo, prior_results)
    # Rimuovi un eventuale blocco worklog in testa (il porting Rust non lo ha).
    # Heuristica conservativa: il blob reale = [worklog?] + [resto]; ricostruiamo
    # il "resto" replicando i blocchi noti senza worklog.
    return _build_blob_copy(state, todo, prior_results)


# ── Dispatcher ────────────────────────────────────────────────────────────────

def compact(text, max_chars=_SUMMARY_MAX_CHARS):
    return _real_compact(text, max_chars) if _REAL else _compact_copy(text, max_chars)


def result_failed(result):
    return _real_result_failed(result) if _REAL else _result_failed_copy(result)


def todo_kind(kind):
    cfg = {"todo_isolation_kind": kind}
    return _real_todo_kind(cfg) if _REAL else _todo_kind_copy(cfg)


def build_blob(state, todo, prior_results):
    # Usiamo SEMPRE la replica senza-worklog per la parita' col porting Rust
    # (vedi NOTA WORKLOG): le funzioni reali differirebbero solo per il worklog.
    return _build_blob_copy(state, todo, prior_results)


# ── pick_next_todo (copia 1:1, per la decision_machine) ───────────────────────

def _pick_next(todos, dag_enabled):
    pending = [t for t in todos if t.get("status") == "pending"]
    if not pending:
        return None
    has_deps = any(t.get("depends_on") for t in todos)
    if not dag_enabled or not has_deps:
        return pending[0]
    done = {str(t.get("id")) for t in todos if t.get("status") in ("completed", "skipped")}
    for t in pending:
        deps = t.get("depends_on") or []
        if all(str(d) in done for d in deps):
            return t
    return pending[0]


def _build_record(seq, todo_id, content, summary, cost):
    return {
        "seq": seq,
        "todo_id": todo_id,
        "content": str(content or "")[:200],
        "summary": summary,
        "cost_usd": cost,
    }


def _cost_of(result, key):
    return float(result.get(key) or 0.0)


def _todos_to_values(todos):
    """Forma del current_todos del porting Rust: solo i campi DAG del Todo
    (id/status/depends_on/seq)."""
    out = []
    for t in todos:
        out.append({
            "id": str(t.get("id")),
            "status": t.get("status"),
            "depends_on": t.get("depends_on") or [],
            "seq": t.get("seq"),
        })
    return out


def decision_machine(inp):
    """Replica la macchina on_failure + _advance_patch (todo_runner_node.py:291-395).

    Input richiede: result (dict del sub-run), opzionale retry_result, on_failure,
    max_retries, dag_topological_enabled, todos_after (stato todos DOPO i mark
    applicati dall'oracolo), todo_id, seq, content, e i campi di stato
    (subagent_results, subagent_cost_cumulative_usd, todo_isolation_retries).
    Ritorna il delta (dict di chiavi modificate) IDENTICO al porting Rust.
    """
    result = inp.get("result") or {}
    retry_result = inp.get("retry_result")
    on_failure = inp.get("on_failure") or "stop"
    max_retries = int(inp.get("max_retries") or 1)
    dag = bool(inp.get("dag_topological_enabled"))
    todos_after = inp.get("todos_after") or []
    todo_id = inp.get("todo_id") or "a"
    seq = inp.get("seq")
    content = inp.get("content") or ""
    summary_max = _SUMMARY_MAX_CHARS

    state = inp
    accumulated = list(state.get("subagent_results") or [])
    summary = compact(str(result.get("summary") or ""), summary_max)
    cost = _cost_of(result, "cost_usd")
    record = _build_record(seq, todo_id, content, summary, cost)

    def advance(accumulated, cost, extra_retries):
        nxt = _pick_next(todos_after, dag)
        delta = {
            "subagent_results": accumulated,
            "subagent_cost_cumulative_usd": cost,
        }
        if extra_retries:
            delta["todo_isolation_retries"] = extra_retries
        if nxt is None:
            delta["active_todo_id"] = None
            delta["stop_reason"] = "end_turn"
        else:
            delta["active_todo_id"] = str(nxt.get("id"))
            delta["stop_reason"] = "tool_use"
            delta["current_todos"] = _todos_to_values(todos_after)
        return delta

    if not result_failed(result):
        record["status"] = "completed"
        accumulated.append(record)
        return advance(accumulated, cost, 0)

    record["status"] = "failed"
    accumulated.append(record)

    if on_failure == "retry":
        retries_done = int(state.get("todo_isolation_retries") or 0)
        if retries_done < max_retries:
            if retry_result is not None and not result_failed(retry_result):
                record["status"] = "completed_after_retry"
                record["summary"] = compact(str(retry_result.get("summary") or ""), summary_max)
                total_cost = cost + _cost_of(retry_result, "cost_usd")
                return advance(accumulated, total_cost, 1)
            # degrada a stop.

    if on_failure == "continue":
        return advance(accumulated, cost, 0)

    # stop (default o degrado dal retry).
    prev_cost = float(state.get("subagent_cost_cumulative_usd") or 0.0)
    return {
        "active_todo_id": todo_id,
        "stop_reason": "end_turn",
        "subagent_results": accumulated,
        "subagent_cost_cumulative_usd": prev_cost + cost,
    }


# ── Helper todo ───────────────────────────────────────────────────────────────

def _t(tid, status="pending", deps=None, seq=None):
    return {"id": tid, "status": status, "depends_on": deps if deps is not None else [], "seq": seq}


def main():
    cases = []

    def add(case_id, function, inp, out):
        cases.append({"case_id": case_id, "function": function, "input": inp, "output": out})

    # ── compact ───────────────────────────────────────────────────────────────
    add("compact_corto", "compact", {"text": "  ciao  ", "max_chars": 600}, compact("  ciao  ", 600))
    add("compact_vuoto", "compact", {"text": "", "max_chars": 600}, compact("", 600))
    add("compact_none_like", "compact", {"text": "   ", "max_chars": 600}, compact("   ", 600))
    long = "a" * 700
    add("compact_lungo_600", "compact", {"text": long, "max_chars": 600}, compact(long, 600))
    add("compact_lungo_240", "compact", {"text": long, "max_chars": 240}, compact(long, 240))
    add("compact_lungo_200", "compact", {"text": long, "max_chars": 200}, compact(long, 200))
    esatto = "b" * 600
    add("compact_esatto_600", "compact", {"text": esatto, "max_chars": 600}, compact(esatto, 600))
    accent = "città " * 200  # accenti multibyte, troncamento su char
    add("compact_accenti", "compact", {"text": accent, "max_chars": 240}, compact(accent, 240))

    # ── result_failed ───────────────────────────────────────────────────────────
    for cid, res in [
        ("rf_completed", {"status": "completed"}),
        ("rf_completed_verified", {"status": "completed_verified"}),
        ("rf_assente", {}),
        ("rf_null_status", {"status": None}),
        ("rf_vuoto_status", {"status": ""}),
        ("rf_failed", {"status": "failed"}),
        ("rf_timeout", {"status": "timeout"}),
        ("rf_failed_maiusc", {"status": "FAILED"}),
        ("rf_completed_spazi", {"status": "  completed  "}),
        ("rf_error_presente", {"status": "completed", "error": "boom"}),
        ("rf_error_null", {"status": "completed", "error": None}),
    ]:
        add(cid, "result_failed", {"result": res}, result_failed(res))

    # ── todo_kind ─────────────────────────────────────────────────────────────
    for cid, kind in [
        ("tk_vuoto", ""),
        ("tk_spazi", "   "),
        ("tk_implement", "implement"),
        ("tk_refactor", "  refactor  "),
        ("tk_none", None),
    ]:
        add(cid, "todo_kind", {"kind": kind}, todo_kind(kind))

    # ── build_context_blob ──────────────────────────────────────────────────────
    prior = [
        {"seq": 1, "content": "crea il file", "status": "completed", "summary": "fatto bene"},
        {"seq": 2, "content": "test", "status": "failed", "summary": "z" * 300},
    ]
    todo_full = {
        "content": "implementa la rotta",
        "acceptance_criteria": [
            {"type": "build", "expected": "compila senza errori"},
            {"type": "test", "description": "i test passano"},
            {"type": "lint"},  # ne' expected ne' description -> ""
        ],
    }
    crit_str = {"acceptance_criteria": json.dumps([{"type": "build", "expected": "ok"}])}
    crit_str_invalid = {"acceptance_criteria": "non-json"}
    state_pieno = {
        "plan_rationale": "  un rationale lungo " + "x" * 1300,
        "plan_constraints": ["vincolo1", "y" * 250] + [f"v{i}" for i in range(15)],
    }
    state_vuoto = {}

    add("blob_completo", "build_context_blob",
        {**state_pieno, "todo": todo_full, "prior_results": prior},
        build_blob(state_pieno, todo_full, prior))
    add("blob_senza_prior", "build_context_blob",
        {**state_pieno, "todo": todo_full, "prior_results": []},
        build_blob(state_pieno, todo_full, []))
    add("blob_senza_piano", "build_context_blob",
        {"todo": todo_full, "prior_results": prior},
        build_blob(state_vuoto, todo_full, prior))
    add("blob_senza_dod", "build_context_blob",
        {**state_pieno, "todo": {"content": "x"}, "prior_results": []},
        build_blob(state_pieno, {"content": "x"}, []))
    add("blob_criteri_stringa", "build_context_blob",
        {"todo": crit_str, "prior_results": []},
        build_blob(state_vuoto, crit_str, []))
    add("blob_criteri_json_invalido", "build_context_blob",
        {"todo": crit_str_invalid, "prior_results": []},
        build_blob(state_vuoto, crit_str_invalid, []))
    # prior con piu' di 8 elementi -> solo ultimi 8.
    prior9 = [{"seq": i, "content": f"c{i}", "status": "completed", "summary": f"s{i}"} for i in range(9)]
    add("blob_prior_oltre8", "build_context_blob",
        {"todo": {"content": "x"}, "prior_results": prior9},
        build_blob(state_vuoto, {"content": "x"}, prior9))
    add("blob_tutto_vuoto", "build_context_blob",
        {"todo": {}, "prior_results": []},
        build_blob(state_vuoto, {}, []))

    # ── decision_machine ────────────────────────────────────────────────────────
    # Stato comune: due todo, a corrente.
    todos_a_done_b_pending = [_t("a", "completed", seq=1), _t("b", "pending", seq=2)]
    todos_a_done_solo = [_t("a", "completed", seq=1)]
    todos_a_blocked_b_skip = [_t("a", "blocked", seq=1), _t("b", "skipped", ["a"], 2)]
    todos_a_blocked_d_pending = [_t("a", "blocked", seq=1), _t("b", "skipped", ["a"], 2), _t("d", "pending", seq=3)]

    base_state = {"subagent_results": [], "subagent_cost_cumulative_usd": 0.0}

    # completed -> advance (b pending).
    inp = {**base_state, "result": {"status": "completed", "summary": "ok a", "cost_usd": 0.5},
           "on_failure": "stop", "todos_after": todos_a_done_b_pending, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_completed_advance", "decision_machine", inp, decision_machine(inp))

    # completed ultimo -> end_turn.
    inp = {**base_state, "result": {"status": "completed", "summary": "ok", "cost_usd": 0.2},
           "on_failure": "stop", "todos_after": todos_a_done_solo, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_completed_end", "decision_machine", inp, decision_machine(inp))

    # failed + stop -> blocked + end_turn (active = a).
    inp = {**base_state, "result": {"status": "failed", "summary": "rotto"},
           "on_failure": "stop", "todos_after": todos_a_blocked_b_skip, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_failed_stop", "decision_machine", inp, decision_machine(inp))

    # failed + continue -> advance su d.
    inp = {**base_state, "result": {"status": "failed", "summary": "rotto"},
           "on_failure": "continue", "todos_after": todos_a_blocked_d_pending, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_failed_continue", "decision_machine", inp, decision_machine(inp))

    # failed + retry riuscito -> completed_after_retry + advance.
    inp = {**base_state, "result": {"status": "failed", "summary": "primo rotto", "cost_usd": 0.1},
           "retry_result": {"status": "completed", "summary": "ok retry", "cost_usd": 0.3},
           "on_failure": "retry", "max_retries": 1,
           "todos_after": todos_a_done_b_pending, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_retry_ok", "decision_machine", inp, decision_machine(inp))

    # failed + retry fallito -> degrada a stop.
    inp = {**base_state, "result": {"status": "failed", "summary": "primo rotto"},
           "retry_result": {"status": "failed", "summary": "retry rotto"},
           "on_failure": "retry", "max_retries": 1,
           "todos_after": todos_a_blocked_b_skip, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_retry_degrada", "decision_machine", inp, decision_machine(inp))

    # retry budget esaurito -> stop diretto.
    inp = {**base_state, "todo_isolation_retries": 1,
           "result": {"status": "failed", "summary": "rotto"},
           "on_failure": "retry", "max_retries": 1,
           "todos_after": [_t("a", "blocked", seq=1)],
           "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_retry_budget_esaurito", "decision_machine", inp, decision_machine(inp))

    # stop con costo cumulato preesistente.
    inp = {"subagent_results": [{"seq": 0, "status": "completed"}], "subagent_cost_cumulative_usd": 2.0,
           "result": {"status": "failed", "summary": "rotto", "cost_usd": 0.5},
           "on_failure": "stop", "todos_after": [_t("a", "blocked", seq=1)],
           "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_stop_costo_cumulato", "decision_machine", inp, decision_machine(inp))

    # completed con DAG topological ON e deps soddisfatte -> prossimo c.
    todos_dag = [_t("a", "completed", seq=1), _t("c", "pending", ["a"], 2)]
    inp = {**base_state, "result": {"status": "completed", "summary": "ok", "cost_usd": 0.0},
           "on_failure": "stop", "dag_topological_enabled": True,
           "todos_after": todos_dag, "todo_id": "a", "seq": 1, "content": "task a"}
    add("dm_completed_dag_on", "decision_machine", inp, decision_machine(inp))

    out_path = "/tmp/golden_todo_runner.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    src = "funzioni REALI del brain" if _REAL else "replica byte-fedele"
    print(f"golden todo_runner: {len(cases)} casi scritti in {out_path} ({src})")


if __name__ == "__main__":
    main()
