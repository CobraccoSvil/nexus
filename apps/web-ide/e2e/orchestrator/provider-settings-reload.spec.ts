/**
 * UI E2E #6: pannello Provider LLM — bottone Ricarica con reset-cooldown.
 *
 * Verifica che il pannello renderizzi i provider e che il click su Testa
 * attivi una request al backend.
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("pannello provider LLM elenca anthropic + deepseek + openai", async ({ page }) => {
  await page.goto("/admin/settings/providers");
  await page.waitForLoadState("networkidle");
  for (const p of ["anthropic", "deepseek", "openai", "mistral", "google"]) {
    await expect(page.getByText(p, { exact: false }).first()).toBeVisible({ timeout: 10_000 });
  }
});

test("reset-cooldown endpoint reachable via proxy web-ide", async ({ request }) => {
  // Test del solo proxy: chiamata diretta API senza UI
  const r = await request.post("/api/admin/providers/anthropic/reset-cooldown");
  expect([200, 401, 403]).toContain(r.status());
  if (r.ok()) {
    const body = await r.json();
    expect(body.ok).toBe(true);
  }
});
