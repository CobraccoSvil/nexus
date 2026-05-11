import type { ProblemItem, PortEntry, PlaywrightRunSummary } from "./api-client";

type Severity = "error" | "warn" | "info";

/** Prefisso comune: i testi lunghi "solo analisi" spingevano l'LLM a rispondere senza tool. */
function operativePreamble(): string {
  return [
    "ISTRUZIONE OPERATIVA (obbligatoria — Nexus):",
    "",
    "Non limitarti a diagnosticare o a elencare ipotesi in chat. Se il problema è risolvibile nel codice/config del progetto attivo, DEVI usare i tool (lettura file, edit, comandi di verifica) e applicare modifiche concrete, poi riassumere file cambiati e come verificare.",
    "Solo se è impossibile agire (mancano segreti, permessi, servizi offline), spiega cosa blocca dopo aver tentato ciò che puoi.",
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
    "Richiesta:",
    "1) Diagnosi probabile basata sul summary/log.",
    "2) Comandi per riprodurre localmente.",
    "3) Fix concreta + test plan (re-run e controlli).",
  ]
    .filter(Boolean)
    .join("\n");
}

/** Porta preferita tra quelle Nexus: dev > app > http > web > minima disponibile */
function pickBestPort(ports: PortEntry[]): number {
  const priority = ["dev", "app", "http", "web"];
  for (const label of priority) {
    const entry = ports.find((p) => p.label?.toLowerCase() === label && p.port != null);
    if (entry?.port != null) return entry.port;
  }
  const sorted = ports.filter((p) => p.port != null).sort((a, b) => (a.port ?? 0) - (b.port ?? 0));
  return sorted[0]?.port ?? 3000;
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

