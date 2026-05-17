/**
 * Helper condiviso per i test e2e UI di Nexus orchestrator.
 *
 * Carica il JWT da `/tmp/nexus_jwt.txt` (creato da mint_jwt.sh) e lo inietta
 * come cookie `token=` nel contesto Playwright.
 */
import { BrowserContext } from "@playwright/test";
import * as fs from "node:fs";

export function loadJwt(): string {
  const path = process.env.NEXUS_TEST_JWT_PATH || "/tmp/nexus_jwt.txt";
  try {
    return fs.readFileSync(path, "utf-8").trim();
  } catch {
    return process.env.NEXUS_TEST_JWT || "";
  }
}

export async function setAuthCookie(context: BrowserContext, baseURL: string): Promise<void> {
  const token = loadJwt();
  if (!token) return;
  const url = new URL(baseURL);
  await context.addCookies([{
    name: "token",
    value: token,
    domain: url.hostname,
    path: "/",
    httpOnly: false,
    secure: false,
    sameSite: "Lax",
  }]);
}
