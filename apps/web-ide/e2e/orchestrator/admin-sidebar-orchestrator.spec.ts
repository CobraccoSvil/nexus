/**
 * UI E2E #5: voci sidebar admin "Orchestrator" + "Sub-agents kinds".
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("sidebar admin contiene voci Orchestrator e Sub-agents kinds", async ({ page }) => {
  await page.goto("/admin");
  // Espandi gruppo "AI & Prompt" (le sotto-voci collassate non sono visibili)
  const group = page.getByRole("button", { name: /AI & Prompt/i }).first();
  if (await group.isVisible()) {
    await group.click();
  }
  const orchestrator = page.getByRole("link", { name: "Orchestrator", exact: true });
  await expect(orchestrator).toBeVisible({ timeout: 5_000 });
  const subagents = page.getByRole("link", { name: /Sub-agents kinds/i });
  await expect(subagents).toBeVisible();
});

test("click su Orchestrator naviga a /admin/orchestrator", async ({ page }) => {
  await page.goto("/admin");
  const group = page.getByRole("button", { name: /AI & Prompt/i }).first();
  if (await group.isVisible()) await group.click();
  await page.getByRole("link", { name: "Orchestrator", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/orchestrator$/);
});
