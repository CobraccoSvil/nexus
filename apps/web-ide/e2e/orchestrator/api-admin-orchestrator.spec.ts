/**
 * UI E2E #7: API admin orchestrator (smoke via Playwright request fixture).
 *
 * Verifica gli endpoint admin-service proxati da web-ide:
 *  - GET /api/admin/orchestrator/plans
 *  - GET /api/admin/orchestrator/subagents/definitions
 *  - GET /api/admin/orchestrator/subagents/runs
 */
import { test, expect } from "@playwright/test";
import { loadJwt } from "./_setup";

const cookie = `token=${loadJwt()}`;

test("GET /api/admin/orchestrator/plans ritorna array", async ({ request }) => {
  const r = await request.get("/api/admin/orchestrator/plans?limit=5", { headers: { Cookie: cookie } });
  expect(r.status()).toBe(200);
  const body = await r.json();
  expect(Array.isArray(body.plans)).toBe(true);
});

test("GET /api/admin/orchestrator/subagents/definitions include kind base", async ({ request }) => {
  const r = await request.get("/api/admin/orchestrator/subagents/definitions", { headers: { Cookie: cookie } });
  expect(r.status()).toBe(200);
  const body = await r.json();
  const kinds = (body.definitions || []).map((d: { kind: string }) => d.kind);
  for (const must of ["plan", "explore", "implement", "verify", "review"]) {
    expect(kinds).toContain(must);
  }
});

test("GET /api/admin/orchestrator/subagents/runs accetta filtri", async ({ request }) => {
  const r = await request.get("/api/admin/orchestrator/subagents/runs?limit=10", { headers: { Cookie: cookie } });
  expect(r.status()).toBe(200);
  const body = await r.json();
  expect(Array.isArray(body.runs)).toBe(true);
});
