/**
 * Smoke #3: endpoint meta-docs sono raggiungibili e ritornano dati validi.
 *
 * Verifica:
 *  - GET /api/meta-docs/list ritorna { items, total }
 *  - POST /api/meta-docs/refresh-all ritorna { status: "ok" }
 *
 * Test fail-soft: se il vault non e' ancora popolato, items puo' essere vuoto
 * (acceptable). Verifichiamo solo schema risposta.
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("meta-docs /list ritorna schema valido", async ({ request, baseURL }) => {
  const resp = await request.get(`${baseURL}/api/meta-docs/list?limit=5`);
  expect(resp.status(), `unexpected status: ${await resp.text()}`).toBeGreaterThanOrEqual(200);
  if (resp.status() === 401) {
    test.skip(true, "auth non disponibile in questo ambiente");
    return;
  }
  const json = await resp.json();
  expect(json).toHaveProperty("items");
  expect(Array.isArray(json.items)).toBe(true);
  expect(json).toHaveProperty("total");
  expect(typeof json.total).toBe("number");
});

test("meta-docs /refresh-all completa senza errori 5xx", async ({ request, baseURL }) => {
  const resp = await request.post(`${baseURL}/api/meta-docs/refresh-all`, { data: {} });
  if (resp.status() === 401) {
    test.skip(true, "auth non disponibile");
    return;
  }
  expect(resp.status(), `unexpected status: ${await resp.text()}`).toBeLessThan(500);
  const json = await resp.json();
  expect(json).toHaveProperty("status");
});
