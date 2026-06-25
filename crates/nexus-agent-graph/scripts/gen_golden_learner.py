#!/usr/bin/env python3
"""Genera /tmp/golden_learner.json: parita' 1:1 della logica DETERMINISTICA del
`learner_node` (brain/agents/nodes/__init__.py:4466-4635).

Replica byte-fedele la logica inline del nodo Python (NON re-implementata in
modo creativo: le formule sono COPIATE dalle righe indicate). Per il reward
euristico Q-learning confronta col PUNTO UNICO Rust gia' validato (stessa
cascata di if del Python, __init__.py:4580-4587).

Casi (>=25): prelim_reward (4 rami), heuristic_reward (rami), fuse_reward
(final presente/assente), should_save_qdrant (sopra/sotto soglia, auto on/off),
interaction_text, troncamenti payload a 200 char (unicode + stringa lunga),
user_input (primo HumanMessage).

Uso:
    python3 crates/nexus-agent-graph/scripts/gen_golden_learner.py
    (scrive /tmp/golden_learner.json)
"""
import json

# MAX_AGENT_ITERATIONS = 60 (brain/agents/nodes/helpers.py:38).
MAX_AGENT_ITERATIONS = 60


# ── Logica inline del learner_node (COPIA 1:1 dal Python) ────────────────────

def prelim_reward(stop_reason: str, result_non_empty: bool) -> float:
    """__init__.py:4503-4508 (l'ordine e' load-bearing)."""
    result = "x" if result_non_empty else ""
    return (
        1.0 if stop_reason == "end_turn" and result
        else 0.4 if stop_reason == "end_turn"
        else 0.0 if stop_reason == "error"
        else 0.3
    )


def heuristic_reward(stop_reason: str, result_non_empty: bool, iterations: int,
                     iteration_budget: int) -> float:
    """__init__.py:4580-4587. Il learner confronta direttamente con
    MAX_AGENT_ITERATIONS; il punto unico Rust ricade su MAX quando budget=0.
    Per parita' col Rust replichiamo il floor `(budget or 0) or MAX`."""
    result = "x" if result_non_empty else ""
    floor = iteration_budget if iteration_budget else MAX_AGENT_ITERATIONS
    if stop_reason == "error":
        return 0.0
    elif iterations >= floor:
        return 0.3
    elif result:
        return 1.0
    else:
        return 0.4


def fuse_reward(final_reward_state, heuristic: float) -> float:
    """__init__.py:4590-4591."""
    return final_reward_state if final_reward_state is not None else heuristic


def should_save_qdrant(auto_extract: bool, min_confidence: float, prelim: float) -> bool:
    """__init__.py:4512."""
    return auto_extract and prelim >= min_confidence


def interaction_text(user_input: str, result: str) -> str:
    """__init__.py:4530."""
    return f"Input: {user_input}\nOutput: {result}"


def build_qdrant_payload(thread_id, task_type, behavior_mode, provider, model,
                         user_input, result):
    """__init__.py:4532-4540 (preview troncate a 200 CHAR)."""
    return {
        "thread_id": thread_id,
        "task_type": task_type,
        "behavior_mode": behavior_mode,
        "provider": provider,
        "model": model,
        "input_preview": user_input[:200],
        "output_preview": result[:200] if result else "",
    }


def user_input_from_messages(messages):
    """__init__.py:4491-4495 (primo HumanMessage)."""
    for msg in messages:
        if msg.get("role") in ("user", "human"):
            return msg.get("content", "")
    return ""


def main() -> None:
    cases = []

    def add(case_id, function, inp, out):
        cases.append({"case_id": case_id, "function": function, "input": inp, "output": out})

    # ── prelim_reward: 4 rami ────────────────────────────────────────────────
    for cid, sr, rne in [
        ("pr_endturn_result", "end_turn", True),
        ("pr_endturn_vuoto", "end_turn", False),
        ("pr_error", "error", False),
        ("pr_error_con_result", "error", True),   # error vince sul result
        ("pr_altro_loopabort", "loop_abort", True),
        ("pr_altro_stop", "stop", False),
    ]:
        add(cid, "prelim_reward",
            {"stop_reason": sr, "result_non_empty": rne},
            prelim_reward(sr, rne))

    # ── heuristic_reward: rami principali (budget=0 -> floor MAX) ────────────
    for cid, sr, rne, it, bud in [
        ("hr_error", "error", True, 1, 0),
        ("hr_cap_max", "end_turn", True, 60, 0),
        ("hr_sotto_cap", "end_turn", True, 59, 0),
        ("hr_result", "end_turn", True, 3, 0),
        ("hr_noresult", "end_turn", False, 3, 0),
        ("hr_budget_esplicito", "end_turn", True, 10, 10),
    ]:
        add(cid, "heuristic_reward",
            {"stop_reason": sr, "result_non_empty": rne, "iterations": it,
             "iteration_budget": bud},
            heuristic_reward(sr, rne, it, bud))

    # ── fuse_reward: final presente/assente ──────────────────────────────────
    add("fr_final_presente", "fuse_reward",
        {"final_reward_state": 0.94, "heuristic": 1.0}, fuse_reward(0.94, 1.0))
    add("fr_final_assente", "fuse_reward",
        {"final_reward_state": None, "heuristic": 0.4}, fuse_reward(None, 0.4))
    add("fr_final_zero", "fuse_reward",
        {"final_reward_state": 0.0, "heuristic": 1.0}, fuse_reward(0.0, 1.0))

    # ── should_save_qdrant: sopra/sotto soglia, auto on/off ──────────────────
    for cid, auto, minc, prelim in [
        ("gate_sopra", True, 0.6, 1.0),
        ("gate_uguale", True, 0.6, 0.6),
        ("gate_sotto", True, 0.6, 0.4),
        ("gate_auto_off", False, 0.6, 1.0),
        ("gate_auto_off_sotto", False, 0.6, 0.3),
    ]:
        add(cid, "should_save_qdrant",
            {"auto_extract": auto, "min_confidence": minc, "prelim_reward": prelim},
            should_save_qdrant(auto, minc, prelim))

    # ── interaction_text ──────────────────────────────────────────────────────
    add("it_base", "interaction_text",
        {"user_input": "dom", "result": "ris"}, interaction_text("dom", "ris"))
    add("it_vuoto", "interaction_text",
        {"user_input": "", "result": ""}, interaction_text("", ""))

    # ── build_qdrant_payload: troncamenti 200 char (unicode + lungo) ─────────
    add("payload_base", "build_qdrant_payload",
        {"thread_id": "tid", "task_type": "code_write", "behavior_mode": "bilanciata",
         "provider": "anthropic", "model": "m1", "user_input": "ciao", "result": "fatto"},
        build_qdrant_payload("tid", "code_write", "bilanciata", "anthropic", "m1",
                             "ciao", "fatto"))
    add("payload_provider_null", "build_qdrant_payload",
        {"thread_id": "tid", "task_type": "chat", "behavior_mode": "veloce",
         "provider": None, "model": None, "user_input": "x", "result": "y"},
        build_qdrant_payload("tid", "chat", "veloce", None, None, "x", "y"))
    lungo_in = "a" * 300
    lungo_out = "b" * 500
    add("payload_lungo", "build_qdrant_payload",
        {"thread_id": "t", "task_type": "code_write", "behavior_mode": "bilanciata",
         "provider": "p", "model": "m", "user_input": lungo_in, "result": lungo_out},
        build_qdrant_payload("t", "code_write", "bilanciata", "p", "m", lungo_in, lungo_out))
    uni_in = "à" * 300  # 300 code-point unicode
    uni_out = "漢" * 250
    add("payload_unicode", "build_qdrant_payload",
        {"thread_id": "t", "task_type": "code_write", "behavior_mode": "bilanciata",
         "provider": "p", "model": "m", "user_input": uni_in, "result": uni_out},
        build_qdrant_payload("t", "code_write", "bilanciata", "p", "m", uni_in, uni_out))
    add("payload_result_vuoto", "build_qdrant_payload",
        {"thread_id": "t", "task_type": "chat", "behavior_mode": "bilanciata",
         "provider": "p", "model": "m", "user_input": "x", "result": ""},
        build_qdrant_payload("t", "chat", "bilanciata", "p", "m", "x", ""))

    # ── user_input: primo HumanMessage ────────────────────────────────────────
    add("ui_primo", "user_input",
        {"messages": [{"role": "user", "content": "primo"},
                      {"role": "user", "content": "secondo"}]},
        user_input_from_messages([{"role": "user", "content": "primo"},
                                  {"role": "user", "content": "secondo"}]))
    add("ui_salta_ai", "user_input",
        {"messages": [{"role": "assistant", "content": "io ai"},
                      {"role": "user", "content": "vero input"}]},
        user_input_from_messages([{"role": "assistant", "content": "io ai"},
                                  {"role": "user", "content": "vero input"}]))
    add("ui_nessun_human", "user_input",
        {"messages": [{"role": "assistant", "content": "solo ai"}]},
        user_input_from_messages([{"role": "assistant", "content": "solo ai"}]))

    out_path = "/tmp/golden_learner.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden learner: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
