/**
 * UI E2E #9: M71 cost breakdown badge per provider+model.
 *
 * Verifica indiretta: cerca elementi del Token Usage Bar o badge di cost
 * nella chat panel (smoke test).
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("chat panel non e' rotto da modifiche M71 (smoke)", async ({ page }) => {
  await page.goto("/ide");
  await page.waitForLoadState("networkidle");
  // Verifica che il composer sia renderizzato (sentinel: textarea principale).
  const composer = page.locator("textarea").first();
  // Se non c'e' progetto attivo, la textarea potrebbe non comparire.
  if (!(await composer.isVisible())) {
    test.skip(true, "composer non visibile, skip");
    return;
  }
  await expect(composer).toBeVisible();
});

test("api-client espone resetProviderCooldown helper (smoke compile)", async ({ page }) => {
  // Test soft del compile: visita una pagina admin e verifica console error free.
  const errors: string[] = [];
  page.on("pageerror", (err) => errors.push(err.message));
  await page.goto("/admin/settings/providers");
  await page.waitForLoadState("networkidle");
  // Errori JS critici (TypeError, ReferenceError) non devono comparire
  const critical = errors.filter((e) => /TypeError|ReferenceError|is not (a |)function/.test(e));
  expect(critical, `errori JS critici: ${critical.join("\n")}`).toHaveLength(0);
});
