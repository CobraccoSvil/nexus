import type { ProblemItem, PortEntry, PlaywrightRunSummary } from "./api-client";

type Severity = "error" | "warn" | "info";

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
          "Richiesta:",
          "1) Dimmi se è un warning innocuo o un problema reale (e perché).",
          "2) Se è reale: fix concreta (file/righe) e come verificarla.",
          "3) Se è falso positivo: come silenziarlo correttamente senza nascondere errori veri.",
        ]
      : [
          "Richiesta:",
          "1) Root cause più probabile con ragionamento.",
          "2) File/righe da controllare e fix concreta.",
          "3) Test plan minimo per validare.",
        ];

  return [
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

export function promptFromDebugEntry(args: {
  level: "ERROR" | "WARN";
  timestamp?: string;
  source?: string;
  message: string;
}): string {
  const sev: Severity = args.level === "WARN" ? "warn" : "error";
  const where =
    args.source && args.source !== "terminal"
      ? `- Sorgente: ${args.source} (servizio/log)`
      : args.source === "terminal"
        ? "- Sorgente: terminale"
        : "- Sorgente: (non specificata)";

  const req =
    sev === "warn"
      ? [
          "Richiesta:",
          "1) È innocuo o è un problema reale? (perché)",
          "2) Se reale: fix concreta (file/righe) e come verificarla.",
          "3) Se è noise: come ridurlo senza perdere segnale.",
        ]
      : [
          "Richiesta:",
          "1) Root cause probabile con ragionamento.",
          "2) Come riprodurre e quali log raccogliere.",
          "3) Fix concreta + test plan minimo.",
        ];

  return [
    header(sev, "Console Debug"),
    "",
    `- Livello: ${args.level}`,
    args.timestamp ? `- Timestamp: ${args.timestamp}` : undefined,
    where,
    "",
    "Messaggio:",
    args.message,
    "",
    ...req,
  ]
    .filter(Boolean)
    .join("\n");
}

