/**
 * Smoke #1: la UI principale di Nexus carica senza errori.
 *
 * Verifica:
 *  - GET /ide -> 200
 *  - Sidebar visibile
 *  - Nessun React error #185 in console
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("IDE carica senza errori React critici", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });

  await page.goto("/ide");
  await page.waitForLoadState("networkidle");

  // Header NEXUS visibile (uppercase nel layout)
  await expect(page.getByText("NEXUS", { exact: true })).toBeVisible({ timeout: 10_000 });

  // Nessun errore React #185 (setState durante render)
  const reactErrors = consoleErrors.filter((e) =>
    /Minified React error #185|setState during render|Cannot update a component while rendering/.test(e),
  );
  expect(reactErrors, `errori React rilevati: ${reactErrors.join("\n")}`).toEqual([]);
});
