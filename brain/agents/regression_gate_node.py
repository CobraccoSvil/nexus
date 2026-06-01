"""regression_gate_node: gate di non-regressione a fine run (M13.4 SOFT + M13.5 HARD).

Gira UNA volta a fine run, tra `reflection` e `learner`. Dato l'insieme di file
modificati dal run (tool write_file/edit_file persistiti in `agent_steps`),
chiede a mcp-core i test che coprono l'impact set
(`POST /api/internal/impact/tests-for-run`), li esegue via `criteria_runner`
(run_command deterministico) e ne valuta l'esito.

Tre rami (governati dal DB, regola G):
  - SOFT (DEFAULT, `regression_gate.hard_block`=false): se uno o piu' test
    falliscono emette SOLO warning + nota KB `regression_warning` +
    todo di follow-up + meta_step. NON blocca: non tocca `stop_reason`,
    prosegue verso `learner`. Comportamento identico a M13.4.
  - HARD block (`hard_block`=true, fallimento HARD-eligible, ciclo < max):
    ritorna all'executor con un blocco `<regression_detected>` per forzare il
    fix (pattern verifier_node), incrementa `regression_cycle`, registra
    gate_status='blocked'. Nessun todo (si ritenta).
  - HARD cap (`hard_block`=true, fallimento HARD-eligible, ciclo >= max):
    degrada a SOFT (warning+nota+todo) e registra gate_status='blocked_capped',
    cosi' l'auto-commit (Rust) salta comunque il commit. Non ritorna all'executor.

Un fallimento e' HARD-eligible solo se il test era mappato method IN
('import','naming') con confidence >= 0.6 (mapping affidabili); i fallimenti
semantici/euristici restano sempre SOFT.

In tutti i rami l'esito e' registrato in `project_impact_runs` via
`POST /api/internal/impact/record-run` (passed/warning/blocked/blocked_capped).

Tutti i parametri sono letti dal DB (regola G, settings `regression_gate.*`),
nessun fallback hardcoded di nomi modello. Errori del gate non rompono il run:
sono gestiti con try/except che logga sempre con contesto (regola H, niente
inghiottimento silenzioso). Scope al solo project del run (regola E).
"""
from __future__ import annotations

import logging
import os
import uuid
from typing import Any

from langchain_core.messages import HumanMessage

from . import criteria_runner, todo_store
from .state import AgentState

logger = logging.getLogger(__name__)

# Servizio iniettato (ToolRunnerClient gRPC). Riusa il singleton del verifier.
_tool_runner = None


def configure(tool_runner: Any) -> None:
    """Inject del ToolRunnerClient (stesso usato da criteria_runner)."""
    global _tool_runner
    _tool_runner = tool_runner


def _modified_files_from_steps(run_id: str) -> list[str]:
    """Estrae i path dei file modificati dal run leggendo `agent_steps`.

    L'executor/tool_dispatch_node persiste OGNI tool eseguito in `agent_steps`
    (tool_name + tool_input JSON). I file modificati sono i path dei tool
    write_file/edit_file/apply_patch/str_replace/create_file con status
    'completed'. Ritorna lista deduplicata e ordinata, vuota se DB down.
    """
    write_tools = ("write_file", "create_file", "edit_file", "apply_patch", "str_replace")
    paths: list[str] = []
    seen: set[str] = set()
    database_url = os.environ.get("DATABASE_URL", "")
    if not database_url:
        logger.debug("regression_gate: DATABASE_URL assente, impossibile leggere agent_steps")
        return paths
    try:
        import psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import RealDictCursor  # type: ignore[import-untyped]

        conn = psycopg2.connect(database_url, cursor_factory=RealDictCursor)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    """SELECT tool_name, tool_input
                       FROM agent_steps
                       WHERE run_id = %s
                         AND tool_name = ANY(%s)
                         AND status = 'completed'
                       ORDER BY step_index ASC""",
                    (run_id, list(write_tools)),
                )
                for row in cur.fetchall():
                    ti = row.get("tool_input") or {}
                    if isinstance(ti, str):
                        # tool_input puo' essere persistito come testo JSON grezzo.
                        import json as _json
                        try:
                            ti = _json.loads(ti)
                        except Exception:
                            ti = {}
                    if not isinstance(ti, dict):
                        continue
                    p = ti.get("path") or ti.get("file_path") or ti.get("filename")
                    if p and isinstance(p, str):
                        p = p.strip()
                        if p and p not in seen:
                            seen.add(p)
                            paths.append(p)
        finally:
            conn.close()
    except Exception as exc:
        logger.warning(
            "regression_gate: lettura agent_steps fallita per run_id=%s: %s", run_id, exc
        )
    return paths


def _fetch_impact_tests(project_id: str, seed_paths: list[str]) -> dict[str, Any]:
    """Chiama POST /api/internal/impact/tests-for-run su mcp-core.

    Sincrono con timeout breve. Ritorna il dict di risposta, oppure
    `{"ok": False, "error": ...}` se la chiamata fallisce. NON solleva.
    """
    try:
        import requests  # noqa: PLC0415

        mcp_core_url = os.environ.get("MCP_CORE_INTERNAL_URL", "http://localhost:4000")
        resp = requests.post(
            f"{mcp_core_url}/api/internal/impact/tests-for-run",
            json={"project_id": project_id, "seed_paths": seed_paths},
            timeout=10.0,
        )
        if resp.status_code != 200:
            logger.warning(
                "regression_gate: tests-for-run status=%d body=%s",
                resp.status_code,
                resp.text[:200],
            )
            return {"ok": False, "error": f"http {resp.status_code}"}
        return resp.json()
    except Exception as exc:
        logger.warning("regression_gate: chiamata tests-for-run fallita: %s", exc)
        return {"ok": False, "error": str(exc)}


def _test_command_for(test_path: str) -> str | None:
    """Determina un comando di test deterministico per il test_path.

    Best-effort per estensione/naming (M13.4): scegliamo il runner standard.
    Ritorna None se il tipo di test non e' riconoscibile (skip del singolo test).
    """
    p = test_path.strip()
    low = p.lower()
    if low.endswith((".spec.ts", ".spec.js", ".spec.tsx", ".spec.jsx", ".e2e.ts")):
        # Test Playwright/end-to-end TypeScript.
        return f"npx playwright test {p}"
    if low.endswith((".test.ts", ".test.js", ".test.tsx", ".test.jsx")):
        # Test unitari JS/TS (Jest/Vitest espongono entrambi il CLI `test`).
        return f"npx vitest run {p}"
    if low.endswith(".py") and ("test_" in low or low.endswith("_test.py")):
        return f"pytest {p}"
    if low.endswith(".rs") or low.startswith("tests/") and low.endswith(".rs"):
        # I test Rust non si eseguono per file singolo: lanciamo la suite.
        return "cargo test"
    logger.debug("regression_gate: test_path non riconosciuto, skip: %s", test_path)
    return None


async def _run_impact_tests(
    tests: list[dict[str, Any]], ctx: dict[str, Any], max_tests: int
) -> list[dict[str, Any]]:
    """Esegue fino a `max_tests` test via criteria_runner (run_command).

    Ritorna una lista di risultati {test_path, command, passed, evidence}.
    I test senza comando determinabile vengono saltati (non contati come fail).
    """
    results: list[dict[str, Any]] = []
    executed = 0
    for t in tests:
        if executed >= max_tests:
            break
        test_path = (t.get("test_path") or "").strip()
        if not test_path:
            continue
        # method/confidence dell'impact mapping: servono al gate HARD (M13.5)
        # per distinguere i fallimenti affidabili (import/naming, confidence>=0.6)
        # da quelli euristici/semantici che restano sempre SOFT.
        method = t.get("method")
        try:
            confidence = float(t.get("confidence")) if t.get("confidence") is not None else None
        except (TypeError, ValueError):
            confidence = None
        cmd = _test_command_for(test_path)
        if not cmd:
            results.append({
                "test_path": test_path,
                "command": None,
                "passed": None,
                "evidence": {"skipped": "comando non determinabile"},
                "method": method,
                "confidence": confidence,
            })
            continue
        criterion = {
            "id": str(uuid.uuid4()),
            "type": "run_command",
            "spec": {"command": cmd},
            "expected": {"exit_code": 0},
        }
        try:
            passed, evidence = await criteria_runner.run_criterion(criterion, ctx)
        except Exception as exc:
            logger.error(
                "regression_gate: esecuzione test %s exception: %s", test_path, exc
            )
            passed, evidence = False, {"error": str(exc)}
        results.append({
            "test_path": test_path,
            "command": cmd,
            "passed": passed,
            "evidence": evidence,
            "method": method,
            "confidence": confidence,
        })
        executed += 1
    return results


async def _emit_soft_warning(
    *,
    run_id: str,
    project_id: str,
    session_id: str | None,
    failed: list[dict[str, Any]],
    modified_files: list[str],
) -> None:
    """Crea nota KB `regression_warning` + todo di follow-up (SOFT).

    Best-effort: ogni passo e' isolato in try/except con log esplicito. Niente
    blocco del run. Usa i tool MCP gia' esistenti (knowledge_create_note,
    nexus_todo_write action='add') via il tool_runner iniettato.
    """
    failed_paths = [f.get("test_path", "?") for f in failed]
    short_run = str(run_id)[:8]

    # ── Nota KB regression_warning ──────────────────────────────────────────
    # Il tool knowledge_create_note non espone la colonna `kind`; marchiamo la
    # nota con intent='regression' + tag 'regression_warning' (cercabile). I
    # file impattati vanno in file_paths per il linking automatico.
    body_lines = [
        f"Il regression gate (SOFT, M13.4) ha rilevato {len(failed)} test falliti "
        f"a fine run {short_run}.",
        "",
        "## Test falliti",
    ]
    for f in failed:
        ev = f.get("evidence") or {}
        body_lines.append(
            f"- `{f.get('test_path', '?')}` (exit_code={ev.get('exit_code')}) "
            f"cmd: `{f.get('command', '?')}`"
        )
    body_lines.append("")
    body_lines.append("## File modificati dal run (impact set)")
    for p in modified_files:
        body_lines.append(f"- `{p}`")
    body_lines.append("")
    body_lines.append(
        "SOFT-only: il run NON e' stato bloccato. Verificare manualmente la "
        "regressione e correggere prima del merge."
    )
    body_md = "\n".join(body_lines)

    if _tool_runner is not None and session_id:
        try:
            await _tool_runner.execute_tool(
                tool_name="knowledge_create_note",
                tool_input={
                    "title": f"Regression warning (run {short_run}): {len(failed)} test falliti"[:200],
                    "body_md": body_md,
                    "intent": "regression",
                    "tags": ["regression_warning", "auto", "regression-gate"],
                    "file_paths": modified_files,
                },
                session_id=str(session_id),
                tool_use_id=str(uuid.uuid4()),
            )
            logger.info(
                "regression_gate: nota regression_warning creata (run=%s, %d test falliti)",
                short_run, len(failed),
            )
        except Exception as exc:
            logger.warning("regression_gate: creazione nota regression_warning fallita: %s", exc)
    else:
        logger.warning(
            "regression_gate: tool_runner o session_id assenti, nota regression_warning non creata "
            "(run=%s)", short_run,
        )

    # ── Todo di follow-up (action='add', non distrugge il plan) ─────────────
    # action='add' richiede che esista gia' un plan per il run; se non esiste,
    # la nota KB sopra resta comunque come record persistente del follow-up.
    if _tool_runner is not None and session_id:
        plan = todo_store.fetch_plan(run_id)
        if plan is None:
            logger.info(
                "regression_gate: nessun plan per run=%s, todo follow-up non creato "
                "(la nota KB resta come record)", short_run,
            )
        else:
            followup = (
                f"Regressione potenziale: verificare e correggere i test falliti "
                f"({', '.join(failed_paths[:5])}) dopo le modifiche a "
                f"{', '.join(modified_files[:5])}."
            )
            try:
                await _tool_runner.execute_tool(
                    tool_name="nexus_todo_write",
                    tool_input={
                        "action": "add",
                        "run_id": run_id,
                        "todos": [{
                            "content": followup,
                            "status": "pending",
                            "priority": "high",
                        }],
                    },
                    session_id=str(session_id),
                    tool_use_id=str(uuid.uuid4()),
                )
                logger.info("regression_gate: todo follow-up aggiunto (run=%s)", short_run)
            except Exception as exc:
                logger.warning("regression_gate: aggiunta todo follow-up fallita: %s", exc)


def _is_hard_eligible(result: dict[str, Any]) -> bool:
    """Un fallimento e' HARD-eligible solo se mappato in modo affidabile.

    M13.5: blocchiamo il run solo per i fallimenti dei test mappati con
    method IN ('import','naming') e confidence >= 0.6. I mapping semantici o
    euristici (confidence < 0.6, method diverso, o method/confidence assenti)
    restano sempre SOFT: non sono abbastanza affidabili per giustificare un
    HARD block (evitiamo falsi positivi che bloccano run validi).
    """
    method = (result.get("method") or "").strip().lower()
    confidence = result.get("confidence")
    if method not in ("import", "naming"):
        return False
    try:
        return confidence is not None and float(confidence) >= 0.6
    except (TypeError, ValueError):
        return False


def _record_run(
    *,
    run_id: str,
    project_id: str,
    seed_paths: list[str],
    gate_status: str,
    impact_paths: list[str] | None = None,
) -> None:
    """Registra l'esito del gate via POST /api/internal/impact/record-run.

    Sincrono con timeout breve (pattern di subagent_store). Best-effort: errori
    solo loggati, mai propagati (regola H: loggiamo sempre, niente
    inghiottimento silenzioso). gate_status IN
    ('passed','warning','blocked','blocked_capped').
    """
    try:
        import requests  # noqa: PLC0415

        mcp_core_url = os.environ.get("MCP_CORE_INTERNAL_URL", "http://localhost:4000")
        body: dict[str, Any] = {
            "run_id": run_id,
            "project_id": project_id,
            "seed_paths": seed_paths,
            "gate_status": gate_status,
        }
        if impact_paths is not None:
            body["impact_paths"] = impact_paths
        resp = requests.post(
            f"{mcp_core_url}/api/internal/impact/record-run",
            json=body,
            timeout=5.0,
        )
        if resp.status_code != 200:
            logger.warning(
                "regression_gate: record-run status=%d body=%s (run=%s, status=%s)",
                resp.status_code, resp.text[:200], str(run_id)[:8], gate_status,
            )
        else:
            logger.info(
                "regression_gate: record-run ok (run=%s, gate_status=%s)",
                str(run_id)[:8], gate_status,
            )
    except Exception as exc:
        logger.warning(
            "regression_gate: chiamata record-run fallita (run=%s, status=%s): %s",
            str(run_id)[:8], gate_status, exc,
        )


def _render_regression_block(
    *,
    hard_failed: list[dict[str, Any]],
    modified_files: list[str],
    cycle: int,
    max_cycles: int,
) -> str:
    """Costruisce il blocco <regression_detected> da iniettare all'executor.

    Elenca i test affidabili falliti + i file impattati + l'istruzione a
    correggere la regressione prima di concludere.
    """
    lines = [
        f'<regression_detected cycle="{cycle}" max_cycles="{max_cycles}">',
        "Il regression gate (HARD, M13.5) ha rilevato test di regressione falliti",
        "dopo le tue modifiche. Questi test sono mappati in modo affidabile",
        "(method import/naming, alta confidenza) all'impact set del run, quindi il",
        "fallimento indica con alta probabilita' una regressione REALE introdotta",
        "dalle modifiche di questo run.",
        "",
        "Test di regressione falliti:",
    ]
    for f in hard_failed:
        ev = f.get("evidence") or {}
        lines.append(
            f"- `{f.get('test_path', '?')}` "
            f"(method={f.get('method')}, confidence={f.get('confidence')}, "
            f"exit_code={ev.get('exit_code')}) cmd: `{f.get('command', '?')}`"
        )
    lines.append("")
    lines.append("File modificati dal run (impact set):")
    for p in modified_files:
        lines.append(f"- `{p}`")
    lines.append("")
    lines.append(
        "Correggi la regressione: analizza i test falliti, individua la causa nel "
        "codice che hai modificato e applica il fix con i tool disponibili. NON "
        "chiedere conferma. Al termine il gate rieseguira' i test. Se la regressione "
        "persiste oltre il numero massimo di cicli concessi, il run verra' bloccato "
        "definitivamente (l'auto-commit non committera' codice che rompe i test)."
    )
    lines.append("</regression_detected>")
    return "\n".join(lines)


async def regression_gate_node(state: AgentState) -> dict[str, Any]:
    """Gate di non-regressione a fine run (SOFT M13.4 + HARD M13.5).

    In SOFT (default, `regression_gate.hard_block`=false) il patch di state NON
    modifica stop_reason ne' forza re-execution: il flusso prosegue verso
    `learner` (warning + nota KB + todo + meta_step). In HARD block ritorna
    all'executor (`stop_reason='tool_use'`, `regression_cycle` incrementato);
    in HARD cap degrada a SOFT senza ritornare all'executor.
    """
    from brain.utils.settings_db import get_bool_setting, get_int_setting  # local import (cicli)

    run_id = state.get("thread_id") or ""
    short_run = str(run_id)[:8] if run_id else "?"

    try:
        # ── Gate: feature flag (DB, regola G) ───────────────────────────────
        if not get_bool_setting("regression_gate.enabled", True):
            logger.debug("regression_gate: disabilitato (regression_gate.enabled=false)")
            return {}

        # soft_only resta un freno di emergenza: se true forza SOFT anche con
        # hard_block=true. Default true (M13.4). Il blocco HARD richiede
        # soft_only=false E hard_block=true (vedi sotto).
        soft_only = get_bool_setting("regression_gate.soft_only", True)

        if not run_id:
            logger.debug("regression_gate: thread_id assente, skip")
            return {}

        project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
        if not project_id:
            logger.debug("regression_gate: project_id assente, skip (run=%s)", short_run)
            return {}

        # ── File modificati dal run ──────────────────────────────────────────
        modified_files = _modified_files_from_steps(run_id)
        if not modified_files:
            logger.debug("regression_gate: nessun file modificato, skip (run=%s)", short_run)
            return {}

        # ── Test che coprono l'impact set (mcp-core) ─────────────────────────
        impact = _fetch_impact_tests(project_id, modified_files)
        if not impact.get("ok"):
            logger.warning(
                "regression_gate: tests-for-run non ok (run=%s): %s",
                short_run, impact.get("error"),
            )
            return {}
        if impact.get("disabled"):
            logger.debug("regression_gate: impact analysis disabilitata lato mcp-core, skip")
            return {}
        tests = impact.get("tests") or []
        if not tests:
            logger.debug(
                "regression_gate: 0 test per l'impact set (run=%s, %d file), skip",
                short_run, len(modified_files),
            )
            return {}

        # ── Esecuzione test (cap da DB) ──────────────────────────────────────
        max_tests = get_int_setting("regression_gate.max_tests", 10)
        ctx = {
            "tool_runner": _tool_runner,
            "session_id": state.get("session_id"),
            "project_id": project_id,
            "timeout_s": float(get_int_setting("regression_gate.test_timeout_s", 120)),
        }
        results = await _run_impact_tests(tests, ctx, max_tests)
        failed = [r for r in results if r.get("passed") is False]

        if not failed:
            logger.info(
                "regression_gate: nessuna regressione (run=%s, %d test eseguiti su %d candidati)",
                short_run, len([r for r in results if r.get("passed") is not None]), len(tests),
            )
            _record_run(
                run_id=run_id, project_id=project_id, seed_paths=modified_files,
                gate_status="passed",
            )
            return {}

        # ── HARD block (M13.5): governato da DB, default-OFF (regola G). ─────
        # Richiede hard_block=true E soft_only=false: soft_only resta freno di
        # emergenza per riportare istantaneamente tutto a SOFT senza toccare
        # hard_block. Default (hard_block=false): comportamento identico a M13.4.
        hard_block = get_bool_setting("regression_gate.hard_block", False) and not soft_only
        hard_failed = [r for r in failed if _is_hard_eligible(r)]

        if hard_block and hard_failed:
            max_cycles = get_int_setting("regression_gate.max_cycles", 1)
            cycle = int(state.get("regression_cycle", 0) or 0)

            if cycle < max_cycles:
                # ── Ramo BLOCK: torna all'executor per il fix (pattern verifier). ─
                logger.warning(
                    "regression_gate HARD: %d test affidabili falliti, ritorno a executor "
                    "(run=%s, cycle=%d/%d): %s",
                    len(hard_failed), short_run, cycle, max_cycles,
                    [r.get("test_path") for r in hard_failed],
                )
                _record_run(
                    run_id=run_id, project_id=project_id, seed_paths=modified_files,
                    gate_status="blocked",
                )
                block_text = _render_regression_block(
                    hard_failed=hard_failed,
                    modified_files=modified_files,
                    cycle=cycle + 1,
                    max_cycles=max_cycles,
                )
                # Pattern verifier_node: forza un'altra iterazione di executor.
                # NESSUN todo qui: si ritenta il fix.
                return {
                    "messages": [HumanMessage(content=block_text)],
                    "regression_cycle": cycle + 1,
                    "stop_reason": "tool_use",
                    "pending_tool_uses": [],
                }

            # ── Ramo CAP: cicli esauriti -> degrada a SOFT + blocked_capped. ──
            logger.warning(
                "regression_gate HARD cap: cicli esauriti (cycle=%d>=max=%d), degrado a SOFT "
                "(run=%s, blocked_capped): %s",
                cycle, max_cycles, short_run,
                [r.get("test_path") for r in hard_failed],
            )
            await _emit_soft_warning(
                run_id=run_id,
                project_id=project_id,
                session_id=state.get("session_id"),
                failed=failed,
                modified_files=modified_files,
            )
            _record_run(
                run_id=run_id, project_id=project_id, seed_paths=modified_files,
                gate_status="blocked_capped",
            )
            meta_step = {
                "kind": "regression_warning",
                "title": (
                    f"Regression gate: {len(hard_failed)} test falliti "
                    f"(HARD cap raggiunto, commit bloccato)"
                ),
                "payload": {
                    "failed_tests": [r.get("test_path") for r in failed],
                    "modified_files": modified_files,
                    "mode": "blocked_capped",
                },
            }
            # CAP: NON ritorna a executor (prosegue verso learner). auto_commit
            # (Rust) salta comunque il commit per gate_status='blocked_capped'.
            return {"meta_steps": [meta_step]}

        # ── SOFT (DEFAULT, M13.4): warning + nota + todo + meta_step. ───────
        # Raggiunto quando hard_block=false (identico a M13.4) oppure quando i
        # fallimenti non sono HARD-eligible (mapping semantici/euristici).
        logger.warning(
            "regression_gate SOFT: %d test falliti dopo modifiche a %d file (run=%s): %s",
            len(failed), len(modified_files), short_run,
            [r.get("test_path") for r in failed],
        )
        await _emit_soft_warning(
            run_id=run_id,
            project_id=project_id,
            session_id=state.get("session_id"),
            failed=failed,
            modified_files=modified_files,
        )
        _record_run(
            run_id=run_id, project_id=project_id, seed_paths=modified_files,
            gate_status="warning",
        )

        meta_step = {
            "kind": "regression_warning",
            "title": f"Regression gate: {len(failed)} test falliti (SOFT, run non bloccato)",
            "payload": {
                "failed_tests": [r.get("test_path") for r in failed],
                "modified_files": modified_files,
                "mode": "soft",
            },
        }
        # SOFT: NON tocchiamo stop_reason ne' rimandiamo all'executor. Patch puro
        # additivo (meta_steps usa reducer `add`).
        return {"meta_steps": [meta_step]}

    except Exception as exc:
        # Regola H: il gate e' best-effort, non deve mai rompere il run, ma
        # l'errore va sempre loggato con contesto (niente inghiottimento muto).
        logger.error(
            "regression_gate: errore non gestito, gate saltato (run=%s): %s",
            short_run, exc, exc_info=True,
        )
        return {}
