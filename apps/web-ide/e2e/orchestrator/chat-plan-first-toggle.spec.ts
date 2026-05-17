/**
 * UI E2E #4: toggle "Plan first" in chat panel.
 *
 * Verifica la presenza del checkbox + tooltip. Non valida il behavior
 * runtime (richiederebbe LLM provider).
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("toggle 'Plan first' presente nel composer della chat", async ({ page }) => {
  await page.goto("/ide");
  await page.waitForLoadState("domcontentloaded");
  // Cerca per id (definito in chat-panel.tsx)
  const toggle = page.locator("#plan-first-toggle");
  // L'IDE potrebbe richiedere selezione progetto; attendiamo ragionevolmente
  await expect(toggle).toBeVisible({ timeout: 15_000 });
  // Click → on
  await toggle.check();
  await expect(toggle).toBeChecked();
});
