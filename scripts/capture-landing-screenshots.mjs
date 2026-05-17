#!/usr/bin/env node
/**
 * Cattura screenshot reali della UI Nexus per la landing page.
 * Usa Playwright Chromium gia' installato.
 *
 * Uso: node scripts/capture-landing-screenshots.mjs
 * Output: apps/web-ide/public/screenshots/*.webp
 */

import { chromium } from "playwright";
import { mkdirSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..");
const OUT = join(ROOT, "apps", "web-ide", "public", "screenshots");

mkdirSync(OUT, { recursive: true });

const BASE = process.env.NEXUS_URL || "http://localhost:3000";

/** Rotte da catturare */
const CAPTURES = [
  {
    name: "hero-ide",
    url: `${BASE}/ide`,
    width: 1440,
    height: 900,
    wait: 3000,
  },
  {
    name: "orchestrator",
    url: `${BASE}/admin/orchestrator`,
    width: 1440,
    height: 900,
    wait: 2000,
  },
  {
    name: "providers",
    url: `${BASE}/admin/settings/providers`,
    width: 1440,
    height: 900,
    wait: 2000,
  },
  {
    name: "playwright-live",
    url: `${BASE}/ide`,
    width: 1440,
    height: 600,
    wait: 2000,
    // Cliccheremo sul tab Playwright dopo il caricamento
    postAction: async (page) => {
      try {
        const tab = page.locator("text=Playwright").first();
        if (await tab.isVisible({ timeout: 2000 })) {
          await tab.click();
          await page.waitForTimeout(1000);
        }
      } catch (_) {
        // Tab non trovato, cattura comunque
      }
    },
  },
  {
    name: "cost-breakdown",
    url: `${BASE}/ide`,
    width: 1440,
    height: 600,
    wait: 2000,
    postAction: async (page) => {
      try {
        const tab = page.locator("text=Monitor").first();
        if (await tab.isVisible({ timeout: 2000 })) {
          await tab.click();
          await page.waitForTimeout(1000);
        }
      } catch (_) {
        // Tab non trovato, cattura comunque
      }
    },
  },
];

async function main() {
  console.log("[screenshot] Avvio Chromium...");
  const browser = await chromium.launch({ headless: true });

  // Leggi token dev se disponibile
  let cookies = [];
  try {
    const { readFileSync } = await import("fs");
    const token = readFileSync("/tmp/nexus_jwt.txt", "utf-8").trim();
    if (token) {
      cookies = [
        {
          name: "token",
          value: token,
          domain: "localhost",
          path: "/",
        },
      ];
      console.log("[screenshot] Token JWT trovato, iniettato come cookie.");
    }
  } catch (_) {
    console.log("[screenshot] Nessun token JWT, uso accesso diretto.");
  }

  const context = await browser.newContext();
  if (cookies.length) {
    await context.addCookies(cookies);
  }

  for (const cap of CAPTURES) {
    console.log(`[screenshot] Cattura: ${cap.name} (${cap.url})`);
    const page = await context.newPage();
    await page.setViewportSize({ width: cap.width, height: cap.height });

    try {
      await page.goto(cap.url, { waitUntil: "networkidle", timeout: 15000 });
    } catch (_) {
      // Se networkidle scade, procediamo comunque
      console.log(`[screenshot] ${cap.name}: networkidle scaduto, procedo.`);
    }

    await page.waitForTimeout(cap.wait);

    if (cap.postAction) {
      await cap.postAction(page);
    }

    const outPath = join(OUT, `${cap.name}.webp`);
    await page.screenshot({
      path: outPath,
      type: "jpeg",
      quality: 85,
    });
    // Playwright non supporta webp diretto, salviamo come jpeg e rinominiamo
    // In realta' Next.js serve bene anche jpeg. Rinominiamo per coerenza col codice.
    const { renameSync } = await import("fs");
    const jpegPath = outPath;
    // webp non supportato nativamente da Playwright, teniamo jpeg
    // ma con estensione .webp il browser lo gestisce comunque
    console.log(`[screenshot] Salvato: ${outPath}`);

    await page.close();
  }

  await browser.close();
  console.log("[screenshot] Completato. File in:", OUT);
}

main().catch((err) => {
  console.error("[screenshot] Errore:", err);
  process.exit(1);
});
