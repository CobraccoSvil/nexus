"""Scenari E2E #10-12: admin orchestrator API + UI page reach + plan-first toggle.

Test sui nuovi endpoint admin-service (`/api/admin/orchestrator/*`) e sulla
raggiungibilita' delle pagine UI admin.
"""
import pytest
from _helpers import api, fetchone, cfg


# ── Scenario 10: admin API plans + subagents ────────────────────────────────

def test_admin_orchestrator_plans_list():
    # passa via web-ide proxy che ha cookie+rewrite verso admin-service
    r = api.get(cfg.web_ide_url, "/api/admin/orchestrator/plans?limit=5")
    if r.status_code in (401, 403):
        pytest.skip(f"auth richiesta (mint JWT?): {r.status_code}")
    assert r.status_code == 200, f"orchestrator/plans → {r.status_code}: {r.text[:200]}"
    body = r.json()
    assert "plans" in body and isinstance(body["plans"], list)


def test_admin_orchestrator_subagent_definitions_listate():
    r = api.get(cfg.web_ide_url, "/api/admin/orchestrator/subagents/definitions")
    if r.status_code in (401, 403):
        pytest.skip("auth richiesta")
    assert r.status_code == 200
    body = r.json()
    defs = body.get("definitions", [])
    kinds = {d.get("kind") for d in defs}
    for must in ("plan", "explore", "implement", "verify", "review"):
        assert must in kinds, f"sub-agent kind base '{must}' assente"


def test_admin_orchestrator_subagent_runs_list():
    r = api.get(cfg.web_ide_url, "/api/admin/orchestrator/subagents/runs?limit=20")
    if r.status_code in (401, 403):
        pytest.skip("auth richiesta")
    assert r.status_code == 200
    body = r.json()
    assert "runs" in body and isinstance(body["runs"], list)


# ── Scenario 11: UI pages reachable ──────────────────────────────────────────

def test_admin_orchestrator_pages_reachable():
    """Le route Next.js servono le pagine admin (redirect a /login se non auth)."""
    for path in ("/admin/orchestrator", "/admin/orchestrator/subagents"):
        r = api.get(cfg.web_ide_url, path, allow_redirects=False)
        # 200 (admin loggato) o 307/302 (redirect a /login) sono entrambi OK
        assert r.status_code in (200, 302, 307), \
            f"path {path} non raggiungibile: {r.status_code}"


# ── Scenario 12: plan-first toggle e cost breakdown structure ───────────────

def test_reset_cooldown_endpoint_via_proxy_web_ide():
    """L'endpoint /api/admin/providers/:name/reset-cooldown deve essere
    raggiungibile via web-ide proxy (fix rewrite Next 16)."""
    r = api.post(cfg.web_ide_url, "/api/admin/providers/anthropic/reset-cooldown")
    # 200 sempre, idempotente
    assert r.status_code == 200, f"proxy reset-cooldown → {r.status_code}: {r.text[:200]}"
    body = r.json()
    assert body.get("ok") is True


def test_ai_usage_ledger_query_compatibility_per_m71():
    """Query M71 (GROUP BY provider, model, status='finalized') gira senza errori."""
    from _helpers import fetchall
    rows = fetchall(
        """SELECT provider, model, SUM(prompt_tokens)::bigint AS pt, COUNT(*) AS calls
           FROM ai_usage_ledger WHERE status = 'finalized'
           GROUP BY provider, model ORDER BY MIN(created_at) ASC LIMIT 5"""
    )
    # Anche zero righe e' accettabile (DB pulito); l'importante e' che la query non esploda
    assert isinstance(rows, list)
