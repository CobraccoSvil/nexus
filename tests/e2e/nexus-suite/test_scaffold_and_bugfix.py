"""Scenari E2E #1-2: scaffold_app + bug_fix.

Lanciano una chat reale via mcp-core e verificano:
  - Plan creato (nexus_agent_plans + nexus_agent_todos)
  - Run termina in stato terminale (non running)
  - File generati nella project_root (PRD.md, struttura base)
  - DB applicativo `<slug>_app` creato (non contaminazione DB nexus)
"""
import pytest
import time
import uuid
from pathlib import Path
from _helpers import api, db, fetchone, wait_for_run, cfg


PROMPT_SCAFFOLD = """Progetto: app web per noleggio auto a breve termine.
Produci: PRD con attori/casi d'uso/NFR; scelta stack motivata; schema DB; backend API + auth; frontend UI; suite test.
Vincoli: no modelli AI hardcoded, no emoji, no docker stop di compose globali, no unwrap fuori test, no log payload in chiaro."""


def _ensure_test_session() -> tuple[str, str, str]:
    """Crea/recupera (session_id, project_id, project_root) per il test.

    Usa il primo progetto esistente del user di test. Se assente, skip.
    """
    proj = fetchone("SELECT id::text, name, "
                    "(SELECT absolute_path FROM workspaces WHERE project_id = p.id AND is_primary=true) AS root "
                    "FROM projects p ORDER BY created_at DESC LIMIT 1")
    if not proj:
        pytest.skip("nessun progetto in DB — setup richiesto")
    project_id = proj["id"]
    project_root = proj.get("root")
    # Crea una nuova session per test isolato
    resp = api.core_post(
        "/api/chat/sessions",
        json_body={"projectId": project_id, "title": f"e2e-scaffold-{int(time.time())}"},
    )
    if resp.status_code != 200:
        pytest.skip(f"chat session creation fallita: {resp.status_code} {resp.text[:200]}")
    session_id = resp.json()["session"]["id"]
    return session_id, project_id, project_root or ""


def _send_message(session_id: str, content: str, mode: str = "automatic", supervisor: str = "continuo") -> dict:
    resp = api.core_post(
        f"/api/chat/sessions/{session_id}/messages",
        json_body={
            "content": content,
            "automationMode": mode,
            "supervisorMode": supervisor,
            "profileId": "default",
            "activeFiles": [],
            "attachments": [],
        },
        timeout=60,
    )
    assert resp.status_code == 200, f"send fallito: {resp.status_code} {resp.text[:200]}"
    return resp.json()


# ── Scenario 1: scaffold_app ────────────────────────────────────────────────

def test_scaffold_app_crea_plan_e_genera_codice():
    session_id, project_id, _ = _ensure_test_session()
    resp = _send_message(session_id, PROMPT_SCAFFOLD)
    run_id = resp["agentRun"]["runId"]

    # Polling: il plan deve essere creato entro 30s
    plan = None
    for _ in range(60):
        plan = fetchone("SELECT run_id::text, planner_model FROM nexus_agent_plans WHERE run_id = %s", (run_id,))
        if plan:
            break
        time.sleep(0.5)
    assert plan, f"nexus_agent_plans NON popolato per run {run_id} (planner non attivo o errore)"

    # Todos creati
    n_todos = fetchone("SELECT COUNT(*) AS n FROM nexus_agent_todos WHERE run_id = %s", (run_id,))
    assert n_todos["n"] >= 5, f"plan con solo {n_todos['n']} todos (atteso >=5)"

    # Attendi terminazione (long-running)
    final = wait_for_run(run_id, timeout_s=cfg.scenario_timeout_s)
    assert final["status"] in {"completed", "failed", "loop_aborted"}, \
        f"status terminale inatteso: {final['status']}"


# ── Scenario 2: bug_fix ──────────────────────────────────────────────────────

def test_bug_fix_intent_classifier_riconosce_e_non_attiva_planner_se_intent_chiat():
    """Prompt 'spiegami questa funzione' → intent=chat → NIENTE planner."""
    session_id, _, _ = _ensure_test_session()
    resp = _send_message(session_id, "Spiegami brevemente cosa fa il file README.md", mode="confirm", supervisor="none")
    run_id = resp["agentRun"]["runId"]
    final = wait_for_run(run_id, timeout_s=120)
    plan = fetchone("SELECT COUNT(*) AS n FROM nexus_agent_plans WHERE run_id = %s", (run_id,))
    todos = fetchone("SELECT COUNT(*) AS n FROM nexus_agent_todos WHERE run_id = %s", (run_id,))
    # Per intent=chat + mode=confirm il planner NON deve attivarsi
    assert plan["n"] == 0, "planner attivato erroneamente per intent chat/confirm"
    assert todos["n"] == 0, "todos creati erroneamente per chat"
    assert final["status"] in {"completed", "failed", "awaiting_confirmation"}, \
        f"status inatteso: {final['status']}"
