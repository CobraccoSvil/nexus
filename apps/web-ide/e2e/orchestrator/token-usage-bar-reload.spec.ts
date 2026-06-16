/**
 * UI E2E: coerenza della TokenUsageBar tra live e reload (regola L).
 *
 * Regressione catturata:
 *   Prima del fix la barra ricalcolava il costo lato client al reload, sommando
 *   `msg.totalCost` dei soli messaggi vivi (filtro `deletedAt`). Quel filtro
 *   azzerava il costo dei turni compattati, ri-introducendo il "bug storico"
 *   gia' risolto dal backend (billing.rs::get_session_usage, dove il costo e'
 *   CUMULATIVO e i token solo VIVI). Risultato: live e reload divergevano.
 *   Esempio osservato su una sessione multi-modello (gemini-2.5-pro + flash):
 *   pre-reload 186.6K token / $3.31, post-reload 279.7K token / $0.07 — un
 *   costo cumulativo non puo' scendere mentre i token salgono.
 *
 *   Il fix fa convergere reload, fine-run e send sincrono sull'unica fonte
 *   autoritativa GET /api/billing/session-usage (punto unico, use-chat.ts
 *   refreshSessionUsage).
 *
 * Invariante verificata: il testo della barra (token + costo) e' IDENTICO prima
 * e dopo un reload sulla stessa sessione (nessun nuovo turno tra le due letture).
 * Il caso multi-modello e' il trigger naturale del bug, ma l'invariante vale per
 * qualunque sessione con contabilita'.
 *
 * Auto-skip se non c'e' una TokenUsageBar visibile (sessione senza usage).
 */
import { test, expect, type Page } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

// Legge "NK token ... $X.XX" dalla TokenUsageBar, o null se assente/illeggibile.
// Match indipendenti per token e costo: il separatore (bullet) e l'eventuale
// suffisso "(N% ctx)" non devono rendere fragile il parsing.
async function readUsageBar(
  page: Page,
): Promise<{ tokens: string; cost: string } | null> {
  const bar = page
    .getByRole("button")
    .filter({ hasText: /token/i })
    .filter({ hasText: /\$/ })
    .first();
  if (!(await bar.isVisible({ timeout: 5_000 }).catch(() => false))) return null;
  const text = ((await bar.innerText().catch(() => "")) ?? "").replace(/\s+/g, " ");
  const tokMatch = text.match(/([\d.,]+\s*[KM]?)\s*token/i);
  const costMatch = text.match(/\$[\d.]+/);
  if (!tokMatch || !costMatch) return null;
  return { tokens: tokMatch[1].replace(/\s+/g, "").trim(), cost: costMatch[0] };
}

test("TokenUsageBar: token e costo identici prima e dopo un reload", async ({ page }) => {
  // Cattura la response autoritativa emessa al load (refreshSessionUsage).
  const usagePromise = page
    .waitForResponse(
      (r) => r.url().includes("/api/billing/session-usage") && r.status() === 200,
      { timeout: 15_000 },
    )
    .catch(() => null);

  await page.goto("/ide");
  await page.waitForLoadState("networkidle");

  const beforeBar = await readUsageBar(page);
  if (beforeBar == null) {
    test.skip(true, "nessuna TokenUsageBar visibile (sessione senza contabilita')");
    return;
  }

  // Sanity: se l'endpoint ha risposto, deve esporre i totali di sessione che
  // alimentano la barra (fonte unica). Non confrontiamo le cifre formattate per
  // non duplicare la logica di formattazione; la verifica forte e' l'identita'
  // pre/post reload sotto.
  const usageResp = await usagePromise;
  if (usageResp) {
    const body = (await usageResp.json().catch(() => null)) as
      | { total_tokens?: number; total_cost_usd?: number }
      | null;
    if (body) {
      expect(body, "session-usage deve esporre total_tokens").toHaveProperty("total_tokens");
      expect(body, "session-usage deve esporre total_cost_usd").toHaveProperty("total_cost_usd");
    }
  }

  // Reload e rilettura della barra.
  await page.reload();
  await page.waitForLoadState("networkidle");
  const afterBar = await readUsageBar(page);
  expect(afterBar, "la barra deve restare visibile dopo il reload").not.toBeNull();

  // Invariante centrale: stessa sessione, nessun nuovo turno -> token e costo
  // devono coincidere. Il bug faceva scendere il costo (filtro deletedAt sui
  // turni compattati) e variare i token.
  expect(
    afterBar!.cost,
    `costo divergente tra live e reload: live=${beforeBar.cost} reload=${afterBar!.cost}`,
  ).toBe(beforeBar.cost);
  expect(
    afterBar!.tokens,
    `token divergenti tra live e reload: live=${beforeBar.tokens} reload=${afterBar!.tokens}`,
  ).toBe(beforeBar.tokens);
});
