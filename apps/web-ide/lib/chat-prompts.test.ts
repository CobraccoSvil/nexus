// Unit test dei prompt d'azione generati per la chat (node --test, type-stripping).
import { test } from "node:test";
import assert from "node:assert/strict";
import { promptEnablePlaywright } from "./chat-prompts.ts";
import type { PortEntry } from "./api-client.ts";

const DEV_PORTS: PortEntry[] = [
  { port: 5173, label: "frontend-dev", allocated: true },
];

/** Estrae il contenuto ESATTO che il prompt detta per `playwright.config.ts`:
 *  il primo blocco fenced ```typescript``` (il secondo e' l'example.spec.ts).
 *  Misurare il blocco di codice, non l'intero prompt, evita falsi positivi
 *  sul testo in prosa che nomina la sintassi vietata proprio per avvertire
 *  di non usarla (regola O: lo strumento arriva all'oggetto reale, il file
 *  che verra' scritto, non a un suo riassunto testuale). */
function extractDictatedConfig(prompt: string): string {
  const blocks = [...prompt.matchAll(/```typescript\n([\s\S]*?)\n```/g)];
  assert.ok(blocks.length >= 1, "il prompt deve contenere almeno un blocco ```typescript```");
  return blocks[0][1];
}

test("promptEnablePlaywright: il config dettato non contiene sintassi solo-Unix", () => {
  const config = extractDictatedConfig(promptEnablePlaywright(DEV_PORTS));

  // Prefisso env inline (`PORT=1234 comando`) e' sintassi di shell POSIX:
  // su Windows nativo (ambiente canonico, vedi CLAUDE.md) `PORT=1234 pnpm dev`
  // non e' un comando valido. Regressione osservata su bacheca-attivita
  // (job 28a2aa0b / 216e553a): il webServer generato usava questa sintassi e
  // Playwright falliva con "Process from config.webServer was not able to start".
  assert.doesNotMatch(
    config,
    /\b[A-Z_][A-Z0-9_]*=\S+\s+(pnpm|npm|npx|node|yarn)\b/,
    "il config non deve dettare un prefisso env inline stile Unix"
  );

  // `env VAR=valore comando` (coreutils `env`) e' altrettanto Unix-only.
  assert.doesNotMatch(
    config,
    /\benv\s+[A-Z_][A-Z0-9_]*=/,
    "il config non deve dettare `env VAR=...` (coreutils, assente su Windows)"
  );
});

test("promptEnablePlaywright: nessun webServer che riavvia il dev server", () => {
  const config = extractDictatedConfig(promptEnablePlaywright(DEV_PORTS));

  // Filosofia allineata a playwright_install.rs: il servizio dev lo gestisce
  // Nexus (pannello Servizi / nexus_port_allocations), non Playwright.
  assert.match(config, /webServer:\s*undefined/);
  assert.doesNotMatch(config, /webServer:\s*\{/);
});

test("promptEnablePlaywright: BASE_URL letta dall'ambiente, non hardcoded come unica fonte", () => {
  const config = extractDictatedConfig(promptEnablePlaywright(DEV_PORTS));

  assert.match(config, /process\.env\.BASE_URL/);
  assert.match(config, /process\.env\.PLAYWRIGHT_BASE_URL/);
});
