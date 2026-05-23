/**
 * Smoke #2: Knowledge Base panel carica e i tab sono navigabili.
 *
 * Verifica:
 *  - Sidebar mostra "Knowledge"
 *  - Click -> panel mostra "Knowledge Base"
 *  - Tab Note / Tag / Ricerca / Grafo / Meta visibili
 *  - Switch al tab Meta non blocca la UI
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

test("Knowledge Base panel responsive con tutti i tab", async ({ page }) => {
  await page.goto("/ide");
  await page.waitForLoadState("networkidle");

  // Apri il pannello Knowledge dalla sidebar
  const knowledgeBtn = page.getByRole("button", { name: /Knowledge/i }).first();
  if (await knowledgeBtn.count()) {
    await knowledgeBtn.click();
  }

  // Header del pannello
  await expect(page.getByText(/Knowledge Base/i)).toBeVisible({ timeout: 8_000 });

  // I 5 tab esistono come bottoni
  for (const tab of ["Note", "Tag", "Ricerca", "Grafo", "Meta"]) {
    await expect(page.getByRole("button", { name: new RegExp(`^${tab}$`, "i") })).toBeVisible();
  }

  // Click sul tab Meta: deve aprire la lista doc del meta-vault
  await page.getByRole("button", { name: /^Meta$/i }).click();
  // Filtro "Tutto" visibile (header tab Meta)
  await expect(page.getByRole("button", { name: /Tutto/i })).toBeVisible({ timeout: 4_000 });
});
