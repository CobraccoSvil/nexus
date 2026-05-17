/**
 * UI E2E #3: PlanInspector drawer.
 *
 * Verifica che cliccando "Inspect" su una row di Plans si apra il drawer
 * con le sezioni todos/verifier runs/sub-agent runs. Se non ci sono plan
 * storici il test si auto-skippa.
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("PlanInspector si apre su click Inspect (skippa se zero plan)", async ({ page }) => {
  await page.goto("/admin/orchestrator");
  await page.waitForLoadState("networkidle");
  const inspectButtons = page.getByRole("button", { name: /Inspect/i });
  const count = await inspectButtons.count();
  if (count === 0) {
    test.skip(true, "nessun plan storico in DB, drawer non testabile");
    return;
  }
  await inspectButtons.first().click();
  // Drawer mostra header con runId e sezioni base
  await expect(page.getByText(/Plan [a-f0-9]{8}/)).toBeVisible({ timeout: 5_000 });
  await expect(page.getByText(/Todos/i)).toBeVisible();
  await expect(page.getByText(/Verifier runs/i)).toBeVisible();
  await expect(page.getByText(/Sub-agent runs/i)).toBeVisible();
  // Close
  await page.getByRole("button", { name: /Close/i }).click();
});
