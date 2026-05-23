/**
 * Helper condiviso per i test E2E "nexus-self":
 * verificano che Nexus stesso (la sua UI web-ide) funzioni end-to-end.
 *
 * Auth: carica JWT da /tmp/nexus_jwt.txt o env NEXUS_TEST_JWT.
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
  await context.addCookies([
    {
      name: "token",
      value: token,
      domain: url.hostname,
      path: "/",
      httpOnly: false,
      secure: false,
      sameSite: "Lax",
    },
  ]);
}
