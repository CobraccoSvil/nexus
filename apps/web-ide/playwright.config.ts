import { defineConfig } from "@playwright/test";

/**
 * Playwright config per i test e2e UI di Nexus (PR-4 Livello 6).
 *
 * Setup richiesto:
 *   pnpm add -D -F web-ide @playwright/test
 *   pnpm --filter web-ide exec playwright install --with-deps chromium
 *
 * Esecuzione:
 *   pnpm --filter web-ide test:e2e:orchestrator
 *
 * I test assumono che web-ide, mcp-core, brain e admin-service siano gia' up
 * (porte 3000/4000/8001/4010). Non avviano nulla automaticamente.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  retries: 0,
  workers: 1,
  reporter: [["list"], ["html", { open: "never" }]],
  timeout: 60_000,
  use: {
    baseURL: process.env.WEB_IDE_URL || "http://localhost:3000",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    // Token JWT pre-iniettato come cookie (mint_jwt.sh).
    extraHTTPHeaders: process.env.NEXUS_TEST_JWT
      ? { Cookie: `token=${process.env.NEXUS_TEST_JWT}` }
      : undefined,
  },
  projects: [
    {
      name: "chromium",
      use: { browserName: "chromium" },
    },
  ],
});
