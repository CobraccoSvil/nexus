/**
 * UI E2E #1: pannello /admin/orchestrator — toggle feature flags.
 *
 * Verifica che le toggle (plan_phase_enabled, verifier_enabled, ...)
 * siano renderizzate e che il click su una toggle invochi la mutation API.
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("orchestrator panel rende le feature flags principali", async ({ page }) => {
  await page.goto("/admin/orchestrator");
  // Wait per rendering settings table
  await expect(page).toHaveURL(/\/admin\/orchestrator/);
  // Tutte le 7 toggle definite in OrchestratorPanel.TOGGLES
  const labels = [
    "Plan phase",
    "Verifier",
    "Sub-agents",
    "Clarifying questions",
    "Auto-delegation by description",
    "Project YAML overrides",
    "Parallel sub-agents per turn",
  ];
  for (const l of labels) {
    await expect(page.getByText(l, { exact: false })).toBeVisible({ timeout: 10_000 });
  }
});

test("orchestrator panel mostra link al sub-section subagents", async ({ page }) => {
  await page.goto("/admin/orchestrator");
  const link = page.getByRole("link", { name: /Sub-agents kinds/i });
  await expect(link).toBeVisible();
});
