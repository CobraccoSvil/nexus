#!/usr/bin/env python3
"""Genera il golden di parita' 1:1 per il VerifierNode Rust.

Importa le funzioni DETERMINISTICHE reali da brain/agents/verifier_node.py
(`_suggest_remediation`, `_render_failed_block`) e replica byte-fedele la parte
deterministica della decision machine di `verifier_node` (il conteggio
evaluable/inconclusive, `all_passed`, la branch decision pass/cap/retry e il
prefisso autonomy_hint), che NON e' estraibile da una funzione pura lato Python
(e' inline nella coroutine async che apre connessioni DB). I risultati dei
criteri sono INPUT stubati: il golden NON esegue criteria_runner.

Output: /tmp/golden_verifier.json — lista di {case_id, function, input, output}
consumata dal test Rust `golden::golden_verifier_parita`.

Nessun accesso al DB:
  - `_render_failed_block` chiama `prompt_registry.get_prompt(...)`, che legge
    una cache in-memory (vuota qui) e ritorna "" -> ramo FALLBACK INLINE (lo
    stesso ramo che il Rust replica). Monkeypatchiamo `get_prompt` a "" per
    rendere il golden deterministico e indipendente da una cache pre-popolata.
  - il conteggio evaluable/inconclusive + branch + autonomy sono replicati qui
    byte-fedele dal sorgente verifier_node.py (riga per riga indicata).

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_verifier.py
  cargo test -p nexus-agent-graph --lib golden_verifier_parita -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

# Rende importabile il package `brain` dalla root del repo.
_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

import brain.agents.prompt_registry as _pr  # noqa: E402

# Forza il ramo fallback inline di _render_failed_block (DB-free, deterministico).
_pr.get_prompt = lambda key: ""

from brain.agents import verifier_node as vn  # noqa: E402

MAX_VERIFY_CYCLES = 3
AUTONOMOUS = ("automatic", "automatico", "continuous", "continuo")


def _is_inconclusive(r: dict) -> bool:
    """Replica `(r.get("evidence") or {}).get("inconclusive")` truthy
    (verifier_node.py:163)."""
    ev = r.get("evidence") or {}
    return bool(ev.get("inconclusive"))


def _decision_machine(state, results, max_cycles, todo_content) -> dict:
    """Replica byte-fedele la parte DETERMINISTICA della decision machine di
    verifier_node (criteri gia' eseguiti = input `results`; gate enabled +
    plan_phase + todo trovato gia' decisi a monte; ramo esplorativo OFF + non
    portato). NON gestisce il ramo fail-closed (criteri assenti / tutti
    inconclusive su task software, che richiede run_general_gates): quel ramo e'
    testato dal lato Rust con criteri-input gia' accodati via la branch
    'all_inconclusive_*' qui sotto, dove i gate-result sono INPUT.

    Riproduce: conteggio evaluable/inconclusive (verifier_node.py:163-181),
    all_passed sugli evaluable, verify_cycle++ (182), branch pass/cap/retry
    (238-287), prefisso autonomy_hint (266-279)."""
    evaluable = [r for r in results if not _is_inconclusive(r)]
    if evaluable:
        all_passed = all(r["passed"] for r in evaluable)
    else:
        # Tutti inconcludenti: il lato Python eseguirebbe run_general_gates per i
        # task software. Nel golden questo ramo e' modellato passando i gate-result
        # come parte di `results` con un flag `gate_passed` esplicito nell'input
        # (vedi caso all_inconclusive_*): qui `all_passed` arriva dall'input.
        all_passed = state.get("_all_passed_when_inconclusive", True)
    cycle = int(state.get("verify_cycle", 0) or 0) + 1

    if all_passed:
        # PASSED: advance azzera i cicli. Il delta di advance (active_todo_id +
        # stop_reason + current_todos) dipende dallo stato DB dei todo, che il
        # golden NON modella; testiamo SOLO i campi deterministici azzerati +
        # l'esito 'passed' (l'avanzamento e' coperto dai test del nodo Rust).
        return {"branch": "passed", "verify_cycle": 0, "exploratory_verify_cycle": 0}

    if cycle >= max_cycles:
        return {
            "branch": "cap",
            "verify_cycle": 0,
            "verifier_last_result": {"passed": False, "cycle": cycle, "results": results},
        }

    # RETRY: HumanMessage <verification_failed> + autonomy_hint se autonomo.
    todo = {"content": todo_content}
    block = vn._render_failed_block(todo, cycle, max_cycles, results)
    behavior = (state.get("behavior_mode") or "").strip().lower()
    if behavior in AUTONOMOUS:
        prefix = (
            "<autonomy_hint mode=\"" + behavior + "\">\n"
            "L'utente ha selezionato la modalita' '" + behavior + "': procedi\n"
            "AUTONOMAMENTE col retry. NON chiedere conferma all'utente, NON\n"
            "scrivere domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui\n"
            "direttamente le azioni necessarie usando i tool disponibili per\n"
            "risolvere i criteri di accettazione falliti. Se non riesci dopo\n"
            "questo ciclo, l'agente verra' automaticamente bloccato dal verifier\n"
            "al raggiungimento del cap di " + str(max_cycles) + " cicli.\n"
            "</autonomy_hint>\n\n"
        )
        block = prefix + block
    return {
        "branch": "retry",
        "messages": [block],
        "verify_cycle": cycle,
        "verifier_last_result": {"passed": False, "cycle": cycle, "results": results},
        "stop_reason": "tool_use",
        "pending_tool_uses": [],
    }


def main() -> None:
    cases: list[dict] = []

    def add(case_id, function, inp, out):
        cases.append({"case_id": case_id, "function": function, "input": inp, "output": out})

    # ── _suggest_remediation: ogni tipo criterion + default ─────────────────────
    sr_cases = [
        ("sr_empty", []),
        ("sr_http_down", [{"type": "http", "passed": False, "evidence": {}}]),
        ("sr_http_500", [{"type": "http", "passed": False, "evidence": {"status": 500}}]),
        ("sr_http_503", [{"type": "http", "passed": False, "evidence": {"status": 503}}]),
        ("sr_http_404", [{"type": "http", "passed": False, "evidence": {"status": 404}}]),
        ("sr_http_403", [{"type": "http", "passed": False, "evidence": {"status": 403}}]),
        ("sr_run_command", [{"type": "run_command", "passed": False, "evidence": {"exit_code": 2}}]),
        ("sr_run_command_none", [{"type": "run_command", "passed": False, "evidence": {}}]),
        ("sr_file_exists", [{"type": "file_exists", "passed": False, "evidence": {}}]),
        ("sr_db_query_notes", [{"type": "db_query", "passed": False,
                                "evidence": {"notes": ["tabella mancante", "indice assente"]}}]),
        ("sr_db_query_no_notes", [{"type": "db_query", "passed": False, "evidence": {}}]),
        ("sr_regex", [{"type": "regex_in_output", "passed": False, "evidence": {}}]),
        ("sr_unknown", [{"type": "qualcosa_altro", "passed": False, "evidence": {}}]),
    ]
    for cid, failed in sr_cases:
        add(cid, "suggest_remediation", {"failed": failed}, vn._suggest_remediation(failed))

    # ── _render_failed_block: evidence trunc 300, diagnostic trunc 800, varie ────
    long_evidence_value = "x" * 400  # > 300 char: json.dumps trunca a 300.
    long_diag = "D" * 1000  # > 800 char: diagnostic[:800].
    # NOTA (parita' serde): con la feature `preserve_order` ATTIVA (Cargo.toml
    # root, FIX B), serde_json::Value usa un IndexMap che conserva l'ordine
    # d'inserimento/deserializzazione delle chiavi, identico a json.dumps di
    # Python (che emette le chiavi in ordine d'inserimento). Poiche' la stringa
    # json.dumps(evidence)[:300] e' confrontata 1:1 con py_json_dumps del Rust,
    # le evidence multi-chiave NON devono piu' essere riordinate alfabeticamente:
    # vedi `rfb_http_non_alfabetico` sotto, che usa apposta un ordine NON
    # alfabetico (status, expected_status, method, url, body_excerpt) per
    # dimostrare la parita' senza il workaround dell'ordine alfabetico.
    rfb_cases = [
        ("rfb_http_500", {
            "todo_content": "Crea endpoint clienti", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "http", "passed": False,
                         "evidence": {"output_excerpt": "Internal Server Error", "status": 500}}],
        }),
        ("rfb_run_command_diag", {
            "todo_content": "Build app", "cycle": 2, "max_cycles": 3,
            "results": [{"type": "run_command", "passed": False,
                         "evidence": {"exit_code": 1, "output_excerpt": "error TS2304: x"}}],
        }),
        ("rfb_evidence_trunc_300", {
            "todo_content": "Todo lungo", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "db_query", "passed": False,
                         "evidence": {"detail": long_evidence_value}}],
        }),
        ("rfb_diag_trunc_800", {
            "todo_content": "Todo diag", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "run_command", "passed": False,
                         "evidence": {"exit_code": 1, "output_excerpt": long_diag}}],
        }),
        ("rfb_error_diag", {
            "todo_content": "Todo err", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "file_exists", "passed": False,
                         "evidence": {"error": "file non scritto"}}],
        }),
        ("rfb_multi_failed", {
            "todo_content": "Todo multi", "cycle": 2, "max_cycles": 3,
            "results": [
                {"type": "http", "passed": False, "evidence": {"status": 404}},
                {"type": "file_exists", "passed": False, "evidence": {}},
            ],
        }),
        ("rfb_no_evidence", {
            "todo_content": "Todo vuoto", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "regex_in_output", "passed": False, "evidence": {}}],
        }),
        # output_excerpt = "" (falsy) cade su error (semantica `or`).
        ("rfb_excerpt_empty_falls_error", {
            "todo_content": "Todo falsy", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "run_command", "passed": False,
                         "evidence": {"error": "stderr boom", "output_excerpt": ""}}],
        }),
        # FIX B: evidence HTTP con chiavi in ordine NON alfabetico (l'ordine
        # reale prodotto da criteria_runner per il criterio http). Con
        # `preserve_order` ATTIVA, py_json_dumps del Rust replica byte-fedele
        # json.dumps Python (ordine d'inserimento: status, expected_status,
        # method, url, body_excerpt), non l'ordine alfabetico. Senza
        # preserve_order questo caso divergerebbe (Rust ordinerebbe
        # body_excerpt < expected_status < method < status < url).
        ("rfb_http_non_alfabetico", {
            "todo_content": "Login deve rispondere 200", "cycle": 1, "max_cycles": 3,
            "results": [{"type": "http", "passed": False,
                         "evidence": {
                             "status": 500,
                             "expected_status": 200,
                             "method": "POST",
                             "url": "http://localhost:3000/api/login",
                             "body_excerpt": "Internal Server Error",
                         }}],
        }),
    ]
    for cid, inp in rfb_cases:
        todo = {"content": inp["todo_content"]}
        out = vn._render_failed_block(todo, inp["cycle"], inp["max_cycles"], inp["results"])
        add(cid, "render_failed_block", inp, out)

    # ── decision_machine: conteggio evaluable/inconclusive + branch + autonomy ──
    dm_cases = [
        # tutti evaluable, tutti pass -> branch passed.
        ("dm_all_pass", {
            "verify_cycle": 0, "behavior_mode": "", "todo_content": "T",
            "results": [{"type": "file_exists", "passed": True, "evidence": {}},
                        {"type": "http", "passed": True, "evidence": {"status": 200}}],
        }),
        # misti inconclusive: l'inconclusive e' escluso, gli evaluable passano -> passed.
        ("dm_mixed_inconclusive_pass", {
            "verify_cycle": 0, "behavior_mode": "", "todo_content": "T",
            "results": [{"type": "http", "passed": True, "evidence": {"status": 200}},
                        {"type": "db_query", "passed": False,
                         "evidence": {"inconclusive": True}}],
        }),
        # un evaluable fallisce, cycle<max -> retry (non autonomo).
        ("dm_one_fail_retry", {
            "verify_cycle": 0, "behavior_mode": "bilanciata", "todo_content": "Endpoint",
            "results": [{"type": "http", "passed": False, "evidence": {"status": 500}}],
        }),
        # un evaluable fallisce, cycle<max, autonomo -> retry con autonomy_hint.
        ("dm_one_fail_retry_autonomous", {
            "verify_cycle": 1, "behavior_mode": "automatico", "todo_content": "Endpoint",
            "results": [{"type": "run_command", "passed": False,
                         "evidence": {"exit_code": 1, "output_excerpt": "boom"}}],
        }),
        # fallimento al cap -> blocked (branch cap).
        ("dm_fail_cap", {
            "verify_cycle": 2, "behavior_mode": "", "todo_content": "T",
            "results": [{"type": "file_exists", "passed": False, "evidence": {}}],
        }),
        # inconclusive su un solo criterio inconcludente + flag fail-closed gate
        # passato (modellato via _all_passed_when_inconclusive=True) -> passed.
        ("dm_all_inconclusive_gate_pass", {
            "verify_cycle": 0, "behavior_mode": "", "todo_content": "T",
            "_all_passed_when_inconclusive": True,
            "results": [{"type": "http", "passed": False,
                         "evidence": {"inconclusive": True}}],
        }),
        # tutti inconcludenti + gate fail-closed FALLITO -> retry (cycle<max).
        ("dm_all_inconclusive_gate_fail", {
            "verify_cycle": 0, "behavior_mode": "continuo", "todo_content": "T",
            "_all_passed_when_inconclusive": False,
            "results": [{"type": "no_orphan_imported", "passed": False,
                         "evidence": {"verdict": "placeholder"}}],
        }),
    ]
    for cid, inp in dm_cases:
        state = {
            "verify_cycle": inp.get("verify_cycle", 0),
            "behavior_mode": inp.get("behavior_mode", ""),
            "_all_passed_when_inconclusive": inp.get("_all_passed_when_inconclusive", True),
        }
        out = _decision_machine(state, inp["results"], MAX_VERIFY_CYCLES, inp["todo_content"])
        add(cid, "decision_machine", inp, out)

    out_path = "/tmp/golden_verifier.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden verifier: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
