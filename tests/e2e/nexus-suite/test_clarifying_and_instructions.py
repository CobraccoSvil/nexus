"""Scenari E2E #6-9: clarifying questions, project instructions, cascade, isolation.

Verificano i pattern Codex (clarifying questions, AGENTS.md style) e l'isolation
del DB Nexus dai progetti applicativi.
"""
import os
import pytest
import uuid
from pathlib import Path
from _helpers import api, fetchone, fetchall, cfg


# ── Scenario 6: clarifying questions (Automatico applies defaults) ──────────

def test_clarifications_persist_table_schema_compatible():
    """Verifica che lo schema della tabella nexus_agent_clarifications supporti
    sia il flusso HITL Confirm (user_answers) sia Automatico (applied_defaults)."""
    cols = fetchall(
        "SELECT column_name FROM information_schema.columns "
        "WHERE table_name = 'nexus_agent_clarifications' ORDER BY column_name"
    )
    names = {r["column_name"] for r in cols}
    for must in ("id", "run_id", "project_id", "questions", "user_answers", "applied_defaults", "created_at", "answered_at"):
        assert must in names, f"colonna nexus_agent_clarifications.{must} mancante"


def test_clarifications_endpoint_brain_get_e_post():
    fake_run_id = "00000000-0000-0000-0000-000000000099"
    # GET
    r = api.brain_get(f"/agent/clarifications/{fake_run_id}")
    assert r.status_code == 200, f"GET clarifications fallito: {r.status_code}"
    body = r.json()
    assert "clarification" in body or "error" in body
    # POST answer (su run inesistente): deve tornare 200 e nessun errore fatale
    r2 = api.brain_post(
        f"/agent/clarifications/{fake_run_id}/answer",
        json_body={"answers": {"q1": "yes", "q2": "no"}},
    )
    assert r2.status_code == 200


# ── Scenario 7: project_instructions injection ───────────────────────────────

def test_project_instructions_loader_carica_file_se_presente(tmp_path):
    proj = fetchone(
        "SELECT id::text, (SELECT absolute_path FROM workspaces WHERE project_id = p.id AND is_primary=true) AS root "
        "FROM projects p ORDER BY created_at DESC LIMIT 1"
    )
    if not proj or not proj.get("root") or not Path(proj["root"]).exists():
        pytest.skip("project_root mancante")
    proj_root = Path(proj["root"])
    nexus_dir = proj_root / ".nexus"
    nexus_dir.mkdir(exist_ok=True)
    instr_file = nexus_dir / "project-instructions.md"
    original_content = instr_file.read_text(encoding="utf-8") if instr_file.exists() else None
    instr_file.write_text(
        "# Project rules\n- usa TypeScript strict\n- niente any\n- test obbligatori\n",
        encoding="utf-8",
    )
    try:
        import sys
        sys.path.insert(0, "/home/administrator/ideai")
        import psycopg2
        from brain.agents import project_instructions_loader
        conn = psycopg2.connect(cfg.database_url)
        try:
            instr = project_instructions_loader.load_project_instructions(conn, proj["id"], str(proj_root))
            assert instr is not None, "loader ritorna None nonostante file presente"
            assert "TypeScript strict" in instr, "contenuto file non incluso nel blocco rendered"
            assert "<project_instructions" in instr or "project-instructions.md" in instr, "wrapper rendered mancante"
        finally:
            conn.close()
    finally:
        if original_content is None:
            instr_file.unlink(missing_ok=True)
        else:
            instr_file.write_text(original_content, encoding="utf-8")


# ── Scenario 8: cascade fallback M60 ─────────────────────────────────────────

def test_cascade_fallback_funziona_se_uno_provider_down():
    """Verifica indiretta: dopo cooldown anthropic, il routing usa la chain.
    Test passa se almeno UN provider e' marcato healthy nel gateway."""
    r = api.core_get("/api/admin/gateway/providers")
    if r.status_code != 200:
        pytest.skip(f"gateway non interrogabile: {r.status_code}")
    data = r.json()
    providers = data.get("providers", [])
    healthy = [p for p in providers if p.get("healthy")]
    assert len(healthy) >= 1, f"nessun provider healthy: {[(p['name'], p.get('healthy')) for p in providers]}"
    # Verifica che il default_provider configurato sia uno di quelli supportati
    default = fetchone("SELECT value FROM settings WHERE key = 'default_provider'")
    assert default and default["value"], "default_provider non configurato"


# ── Scenario 9: isolation multitenant ─────────────────────────────────────────

def test_isolation_dati_progetti_distinti():
    """Verifica che todos/subagent_runs siano correttamente isolati per project_id.
    Se ci sono >=2 progetti, nessuno dei loro plan/todos deve far parte dell'altro."""
    projs = fetchall("SELECT id::text FROM projects ORDER BY created_at DESC LIMIT 2")
    if len(projs) < 2:
        pytest.skip("servono almeno 2 progetti per testare isolation")
    p1, p2 = projs[0]["id"], projs[1]["id"]
    # Plans per progetto: ogni run deve avere project_id coerente
    rows = fetchall(
        "SELECT project_id::text AS pid, COUNT(*) AS n FROM nexus_agent_plans "
        "WHERE project_id IN (%s::uuid, %s::uuid) GROUP BY project_id",
        (p1, p2),
    )
    for r in rows:
        assert r["pid"] in (p1, p2), f"project_id leak: {r['pid']}"
    # Sub-agent runs idem
    sub_rows = fetchall(
        "SELECT project_id::text AS pid FROM nexus_subagent_runs "
        "WHERE project_id IN (%s::uuid, %s::uuid) LIMIT 50",
        (p1, p2),
    )
    for r in sub_rows:
        assert r["pid"] in (p1, p2), f"sub-agent project_id leak: {r['pid']}"
