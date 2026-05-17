/**
 * UI E2E #2: /admin/orchestrator/subagents — CRUD sub-agent kinds.
 *
 * Verifica che i 5 kind base (plan, explore, implement, verify, review)
 * siano elencati e che il bottone "Nuovo kind" apra il modale di edit.
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("subagent editor elenca i 5 kind base", async ({ page }) => {
  await page.goto("/admin/orchestrator/subagents");
  for (const kind of ["plan", "explore", "implement", "verify", "review"]) {
    await expect(page.getByText(kind, { exact: false })).toBeVisible({ timeout: 10_000 });
  }
});

test("bottone Nuovo kind apre il modale di edit", async ({ page }) => {
  await page.goto("/admin/orchestrator/subagents");
  await page.getByRole("button", { name: /Nuovo kind/i }).click();
  await expect(page.getByText(/Nuovo sub-agent kind/i)).toBeVisible({ timeout: 5_000 });
  // I campi obbligatori sono presenti
  await expect(page.getByText(/Prompt key/i)).toBeVisible();
  await expect(page.getByText(/Tool whitelist/i)).toBeVisible();
  // Annulla chiude il modale
  await page.getByRole("button", { name: /Annulla/i }).click();
  await expect(page.getByText(/Nuovo sub-agent kind/i)).toBeHidden();
});
