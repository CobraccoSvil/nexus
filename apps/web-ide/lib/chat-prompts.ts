import type { ProblemItem, PortEntry, PlaywrightRunSummary } from "./api-client";

/** Hint di tipo agente per i prompt d'azione (error-fix) generati dai pannelli
 *  diagnostici. Propagato come `agentTypeHint` nel POST del messaggio: il backend
 *  lo mappa su `agent_type_hint` -> `nexus_agent_type_hint`, attiva
 *  `agent_type_forced` e SALTA la disambiguazione d'intent (A/B) che altrimenti
 *  bloccherebbe l'avvio del run. Valore coerente con il canale fuori-chat
 *  service_observer (service_observer_remediation.rs:155). */
export const ACTION_AGENT_HINT = "debugger";

type Severity = "error" | "warn" | "info";

/** Prefisso comune: i testi lunghi "solo analisi" spingevano l'LLM a rispondere senza tool. */
function operativePreamble(): string {
  return [
    "ISTRUZIONE OPERATIVA (obbligatoria — Nexus):",
    "",
    "Fix M44: il workflow di error-fix richiede AZIONE, non solo analisi.",
    "1. NON chiedere conferma all'utente prima di agire (es. 'Applicheresti questa modifica?', 'Quale file vuoi che modifichi?'). DECIDI tu il file corretto leggendo il filesystem e PROCEDI.",
    "2. NON elencare possibili soluzioni come domanda aperta: scegli la piu' probabile, applicala con tool concreti (read_file, edit_file, write_file, run_command), poi verifica.",
    "3. Tutti i passaggi devono usare i tool del progetto attivo. Niente 'snippet teorici' o esempi YAML/sh se NON sono stati prima applicati nei file reali.",
    "4. Risposta finale: lista file modificati + comando di verifica eseguito + esito.",
    "",
    "Solo se e' impossibile agire (segreti mancanti, permessi root non disponibili, servizi offline irraggiungibili) spiega cosa blocca DOPO aver tentato ciò che puoi.",
    "",
    "Lingua: rispondi nella lingua dell'utente (default italiano). Tutti i documenti markdown generati (PRD, README, docs/*) seguono la stessa lingua del prompt utente.",
    "",
    "---",
    "",
  ].join("\n");
}

function normalizeSeverity(input: string | undefined): Severity {
  const s = (input ?? "").toLowerCase();
  if (s.includes("warn")) return "warn";
  if (s.includes("info") || s.includes("debug")) return "info";
  return "error";
}

function header(sev: Severity, title: string) {
  const prefix = sev === "error" ? "ERRORE" : sev === "warn" ? "WARNING" : "INFO";
  return `${prefix} — ${title}`;
}

export function promptFromProblem(item: ProblemItem): string {
  const sev = normalizeSeverity(item.severity);
  const loc = item.filePath
    ? `${item.filePath}${item.line ? `:${item.line}` : ""}${item.column ? `:${item.column}` : ""}`
    : null;
  const req =
    sev === "warn"
      ? [
          "Passi (in ordine — con tool sul repo):",
          "1) Verifica se è innocuo o un problema reale.",
          "2) Se reale: applica fix nel codice (patch) e dimostra con verifica.",
          "3) Se falso positivo: configurazione o soppressione corretta (senza nascondere errori veri).",
        ]
      : [
          "Passi (in ordine — con tool sul repo):",
          "1) Trova root cause partendo dai file indicati e dal messaggio.",
          "2) Implementa la fix (patch) e tieni i cambiamenti minimi.",
          "3) Esegui verifica (build/lint/test/comando rilevante).",
        ];

  return [
    operativePreamble(),
    header(sev, "Problema rilevato"),
    "",
    `- Severità: ${item.severity}`,
    `- Sorgente: ${item.source}`,
    loc ? `- File: ${loc}` : undefined,
    item.createdAt ? `- Quando: ${new Date(item.createdAt).toLocaleString()}` : undefined,
    "",
    "Messaggio:",
    item.message,
    "",
    ...req,
  ]
    .filter(Boolean)
    .join("\n");
}

export function promptFromPort(port: PortEntry): string {
  const sev: Severity = port.url ? "info" : "warn";
  return [
    operativePreamble(),
    header(sev, "Porta rilevata"),
    "",
    `- Porta: ${port.port ?? "(n/a)"}`,
    port.label ? `- Label: ${port.label}` : undefined,
    port.service ? `- Service: ${port.service}` : undefined,
    port.state ? `- State: ${port.state}` : undefined,
    port.url ? `- URL: ${port.url}` : "- URL: (non disponibile)",
    "",
    "Richiesta:",
    "1) Spiega cosa sta esponendo questa porta e come verificarlo (curl/browser).",
    "2) Se manca l'URL, dimmi come ricavarlo o configurarlo in Nexus/servizio.",
    "3) Se è una porta in conflitto o non raggiungibile, proponi una fix concreta.",
  ]
    .filter(Boolean)
    .join("\n");
}

export function promptFromPlaywrightRun(run: PlaywrightRunSummary): string {
  const sev: Severity = run.status === "failed" ? "error" : run.status === "passed" ? "info" : "warn";
  return [
    operativePreamble(),
    header(sev, "Run Playwright"),
    "",
    `- Run: ${run.label}`,
    `- Status: ${run.status}`,
    run.createdAt ? `- Quando: ${new Date(run.createdAt).toLocaleString()}` : undefined,
    run.summary ? `- Summary: ${run.summary}` : undefined,
    "",
    "Richiesta (esegui in sequenza, NON limitarti ad elencare):",
    "1) Riproduci localmente: lancia `pnpm exec playwright test --reporter=line` (oppure il tool Nexus run_playwright_tests) e leggi l'output.",
    "2) Applica la fix nel file appropriato del progetto (playwright.config.ts, package.json scripts, .env, ecc.). NON chiedere quale file e' giusto: scoprilo tu con list_files/read_file.",
    "3) Re-run dei test e verifica che l'errore non si ripresenta. Riporta esito (passed/failed + file modificati).",
    "",
    "IMPORTANTE: usa il tool dedicato `run_playwright_tests` (NON `run_command pnpm exec`): legge automaticamente le porte da nexus_port_allocations e popola il pannello Playwright. Parametri: { auto_start_server: true, reporter: \"line\" }.",
  ]
    .filter(Boolean)
    .join("\n");
}

/** Fix M17: porta dev preferita per Playwright/UI runner.
 * Allineamento con pick_dev_port di crates/mcp-core/src/nexus_tools/test_playwright.rs:25.
 * - Cerca label che CONTIENE uno dei keyword dev (non solo equals)
 * - ESCLUDE esplicitamente label backend/api/server-api
 * - Fallback: preferisce porte tipiche dev (>= 5000) rispetto a backend (< 5000)
 * - Ultimo fallback: 5173 (default Vite) invece di 3000 (che spesso e' Next.js / web-ide stesso)
 */
function pickBestPort(ports: PortEntry[]): number {
  const dev_keywords = ["dev", "app", "http", "web", "frontend", "serve", "server", "vite", "next", "react"];
  const backend_keywords = ["backend", "api", "server-api", "dotnet", "fastify", "express", "graphql"];

  const isBackend = (p: PortEntry) => {
    const l = p.label?.toLowerCase() ?? "";
    return backend_keywords.some((bk) => l.includes(bk));
  };

  // 1) Preferenza per label che contiene un dev keyword e NON e' backend
  for (const kw of dev_keywords) {
    const entry = ports.find((p) => {
      const l = p.label?.toLowerCase() ?? "";
      return l.includes(kw) && !isBackend(p) && p.port != null;
    });
    if (entry?.port != null) return entry.port;
  }

  // 2) Tra le porte non-backend, preferisci porte tipiche dev (>= 5000)
  const nonBackend = ports.filter((p) => p.port != null && !isBackend(p));
  const devTypical = nonBackend
    .filter((p) => (p.port ?? 0) >= 5000)
    .sort((a, b) => (a.port ?? 0) - (b.port ?? 0));
  if (devTypical[0]?.port != null) return devTypical[0].port;

  // 3) Fallback finale: porta minore tra non-backend, oppure 5173 (default Vite)
  const sorted = nonBackend.sort((a, b) => (a.port ?? 0) - (b.port ?? 0));
  return sorted[0]?.port ?? 5173;
}

/**
 * Prompt per abilitare Playwright in un progetto Nexus.
 * Usa la porta allocata da Nexus come fallback hardcoded nel config generato;
 * il config legge comunque BASE_URL dall'ambiente, che run_playwright_tests imposta a runtime.
 */
export function promptEnablePlaywright(ports: PortEntry[]): string {
  const port = pickBestPort(ports);

  const configLines = [
    "import { defineConfig, devices } from '@playwright/test';",
    "",
    "const BASE_URL =",
    "  process.env.BASE_URL ||",
    "  process.env.PLAYWRIGHT_BASE_URL ||",
    `  'http://localhost:${port}';`,
    "",
    "const port = (() => {",
    "  try {",
    `    return parseInt(new URL(BASE_URL).port || '${port}', 10);`,
    "  } catch {",
    `    return ${port};`,
    "  }",
    "})();",
    "",
    "export default defineConfig({",
    "  testDir: './e2e',",
    "  fullyParallel: true,",
    "  forbidOnly: !!process.env.CI,",
    "  retries: process.env.CI ? 2 : 1,",
    "  workers: process.env.CI ? 1 : undefined,",
    "  timeout: 30_000,",
    "  reporter: process.env.CI ? 'list' : 'html',",
    "  use: {",
    "    baseURL: BASE_URL,",
    "    trace: 'on-first-retry',",
    "  },",
    "  projects: [",
    "    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },",
    "  ],",
    "  webServer: {",
    "    command: `PORT=${port} pnpm dev`,",
    "    url: BASE_URL,",
    "    reuseExistingServer: true,",
    "    timeout: 60_000,",
    "    stdout: 'pipe',",
    "    stderr: 'pipe',",
    "  },",
    "});",
  ].join("\n");

  const exampleSpec = [
    "import { test, expect } from '@playwright/test';",
    "",
    "test('homepage carica senza errori', async ({ page }) => {",
    "  await page.goto('/');",
    "  await expect(page).toHaveTitle(/.*/);",
    "});",
  ].join("\n");

  return [
    operativePreamble(),
    `Abilita Playwright nel progetto. Porta dev Nexus assegnata: ${port} (BASE_URL default: http://localhost:${port}).`,
    "",
    "Esegui questi step nell'ordine:",
    "1. Installa dipendenze: usa `run_command` con `pnpm add -D @playwright/test` nella root del progetto",
    "2. Installa browser: usa `run_command` con `pnpm exec playwright install --with-deps chromium`",
    "3. Crea il file `playwright.config.ts` nella root del progetto con questo contenuto ESATTO:",
    "",
    "```typescript",
    configLines,
    "```",
    "",
    "IMPORTANTE: il config legge BASE_URL dall'ambiente — il tool `run_playwright_tests` la imposta automaticamente con la porta Nexus corretta.",
    "",
    "4. Crea la directory `e2e/` con un file `e2e/example.spec.ts` minimale:",
    "",
    "```typescript",
    exampleSpec,
    "```",
    "",
    "5. Verifica che il config sia valido: usa `run_command` con `pnpm exec playwright test --list` (deve listare i test senza errori di compilazione)",
    "6. Riporta il risultato finale: file creati, eventuali errori di installazione o configurazione",
  ].join("\n");
}

/**
 * Prompt per eseguire i test Playwright tramite il tool dedicato di Nexus.
 * Invita esplicitamente l'agente a usare run_playwright_tests invece di run_command.
 */
export function promptRunPlaywrightTests(): string {
  return [
    operativePreamble(),
    "Esegui i test Playwright del progetto usando il tool dedicato Nexus.",
    "",
    "Usa il tool `run_playwright_tests` (NON `run_command` con pnpm exec). Il tool:",
    "- legge automaticamente le porte allocate in nexus_port_allocations per questo progetto",
    "- imposta BASE_URL con la porta Nexus corretta prima di lanciare Playwright",
    "- avvia il server dev se non e' ancora in ascolto (imposta auto_start_server: true)",
    "- salva pass/fail nel pannello Playwright di Nexus",
    "",
    "Parametri consigliati: { auto_start_server: true, reporter: \"list\" }",
    "",
    "Dopo l'esecuzione riporta: test passati, test falliti, nomi dei test falliti (se presenti).",
  ].join("\n");
}

/** Log a singola riga o prefisso senza stack → chiedi esplicitamente ricerca nel repo invece di "raccogliere log". */
function debugLogLooksTruncated(message: string): boolean {
  const t = message.trim();
  if (t.length < 120) return true;
  if (/exception data:\s*$/i.test(t)) return true;
  if (/^.{0,200}(exception data|inner exception|--->)\s*:?\s*$/i.test(t)) return true;
  if (/exception|fail|unhandled|stacktrace/i.test(t) && !/\b(at\s+[\w.]+\(|\.cs:\d+)/i.test(t) && t.length < 500) {
    return true;
  }
  return false;
}

export function promptFromDebugEntry(args: {
  level: "ERROR" | "WARN";
  timestamp?: string;
  source?: string;
  message: string;
  /** Righe di log vicine (stesso flusso Debug) per eccezioni/stack multi-riga */
  contextLines?: string[];
}): string {
  const sev: Severity = args.level === "WARN" ? "warn" : "error";
  const where =
    args.source && args.source !== "terminal"
      ? `- Sorgente: ${args.source} (servizio/log)`
      : args.source === "terminal"
        ? "- Sorgente: terminale"
        : "- Sorgente: (non specificata)";

  const ctx = (args.contextLines ?? []).map((l) => l.trimEnd()).filter(Boolean);
  const truncated = debugLogLooksTruncated(args.message) && ctx.length === 0;
  const truncatedNote = truncated
    ? [
        "Nota su questo log:",
        "Il testo principale può essere tronco (journalctl/singola riga). NON limitarti a 'spiegare' l'errore: usa i tool sul progetto per trovare file/route/tipo citati o correlati, confrontare con il codice e applicare fix. Se serve più log runtime, dopo la patch indica come riprodurre e dove guardare — ma la priorità è intervenire sul codice nel repo.",
        "",
      ]
    : [];
  const contextBlock =
    ctx.length > 0
      ? [
          "Contesto (righe adiacenti dalla console Debug, stesso flusso — possono includere stack trace o InnerException):",
          ...ctx.map((line) => (line.length > 2000 ? `${line.slice(0, 2000)}… [troncato]` : line)),
          "",
        ]
      : [];

  const req =
    sev === "warn"
      ? [
          "Azione richiesta (ordine — tool sul progetto attivo):",
          "1) Conferma se è rumore/spam o un problema reale confrontando con codice/config.",
          "2) Se reale: modifica mirata (patch) nel repo; evita solo consigli testuali.",
          "3) Verifica con comando o test minimi (es. build/lint/endpoint o run del servizio).",
          "4) Riassunto: cosa cambiato, perché, come verificare.",
        ]
      : [
          "Azione richiesta (ordine — NON fare solo analisi o 'root cause' in teoria):",
          "1) Collega il messaggio al codice: identifica stack tecnologica dal servizio/log (es. .NET → progetto/endpoint/handlers) e cerca nel workspace stringhe, tipi eccezione, route o nomi file citati (search_in_files / grep).",
          "2) Apri i file coinvolti con read_file; individua condizione o bug concreto che spiega l'errore.",
          "3) Implementa una correzione minima sicura (patch) nel repository; se servono config o env, aggiorna file tracciati nel repo o documenta il valore richiesto senza allucinare segreti.",
          "4) Verifica con il comando appropriato (es. `dotnet build` / test progetto, `npm test`, curl sull'API, riavvio servizio) e riporta esito. Se non puoi eseguire, indica esattamente quale comando l'utente deve lanciare.",
          "5) Output finale: file modificati, diff concettuale, comando di verifica e (se noto) come riprodurre il caso con 1–2 passi.",
        ];

  return [
    operativePreamble(),
    header(sev, "Console Debug"),
    "",
    `- Livello: ${args.level}`,
    args.timestamp ? `- Timestamp: ${args.timestamp}` : undefined,
    where,
    "",
    "Messaggio principale (riga selezionata):",
    args.message,
    "",
    ...contextBlock,
    ...truncatedNote,
    ...req,
  ]
    .filter(Boolean)
    .join("\n");
}

