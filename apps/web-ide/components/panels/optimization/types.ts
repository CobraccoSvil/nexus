import type { QualityFinding } from "../../../lib/api-client";
import type { Theme } from "../../../lib/theme";

export const CATEGORIES = [
  { id: "all", label: "Tutti" },
  { id: "sql", label: "SQL" },
  { id: "security", label: "Sicurezza" },
  { id: "typing", label: "Typing" },
  { id: "complexity", label: "Complessità" },
  { id: "maintainability", label: "Manutenibilità" },
  { id: "docs", label: "Documentazione" },
  { id: "comments", label: "Commenti" },
  { id: "dead_code", label: "Dead Code" },
  { id: "naming", label: "Naming" },
  { id: "style", label: "Stile" },
  { id: "reliability", label: "Affidabilità" },
  { id: "duplication", label: "Duplicazione" },
];

export const SEVERITY_COLOR: Record<string, string> = {
  high: "#ef4444",
  medium: "#f97316",
  low: "#6b7280",
};

export const OPT_API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

export interface OptimizationPanelProps {
  projectId: string;
  onSendToChat?: (message: string) => void;
  onAutoSendToChat?: (message: string) => void;
  agentRunEndSignal?: number;
}

/**
 * Suggerimento di retry context-aware basato sul category/title del finding.
 * Senza questo, ogni retry usava il prompt long-function ("estrai helper functions
 * finché la funzione scende sotto la soglia") anche per N+1 query, parse_error,
 * complexity, ecc. — confondendo l'agente. Vedi bug 4 del test E2E.
 */
export function retryHintForCategory(category: string | undefined, title: string | undefined): string {
  const cat = (category || "").toLowerCase();
  const titleLc = (title || "").toLowerCase();

  // N+1 / DB query inside loop
  if (titleLc.includes("n+1") || titleLc.includes("query inside loop")) {
    return "Riprova: sposta la query DB FUORI dal loop. Esegui una sola query con JOIN/WHERE IN/ORDER BY/LIMIT che restituisca tutto il dataset necessario, poi itera sui risultati in memoria.";
  }
  // Long function / cyclomatic complexity
  if (cat === "maintainability" && titleLc.includes("long function")) {
    return "Riprova: estrai blocchi logici in helper functions separate finché la funzione principale scende sotto la soglia indicata.";
  }
  if (cat === "complexity" || titleLc.includes("cyclomatic")) {
    return "Riprova: riduci la complessità ciclomatica suddividendo i rami condizionali in funzioni più piccole, o usa table-driven dispatch al posto di catene if/else.";
  }
  // Dead code
  if (cat === "dead_code" || titleLc.includes("dead code") || titleLc.includes("unused")) {
    return "Riprova: rimuovi codice/import/variabili non utilizzati, oppure sostituisci con `_` per quelli intenzionali.";
  }
  // Documentation / comments
  if (cat === "documentation" || cat === "commenti") {
    return "Riprova: aggiungi commento doc (TSDoc/JSDoc/Rust doc-comment) che spiega lo scopo della funzione, parametri principali e valore di ritorno.";
  }
  // Typing
  if (cat === "typing") {
    return "Riprova: sostituisci `any` con un tipo specifico (interface, type, generics). Se davvero serve un tipo dinamico, usa `unknown` con type-guards.";
  }
  // Parse error: probabile falso positivo dello scanner
  if (cat === "parse_error" || titleLc.includes("parse error")) {
    return "Attenzione: 'parse_error' su file SQL è spesso un falso positivo dello scanner che non supporta tutti i dialetti. Verifica se il file è davvero malformato — se è valido SQL standard segnalalo come falso positivo invece di modificarlo.";
  }
  // Reliability generico
  if (cat === "reliability") {
    return "Riprova: applica il fix specifico al pattern descritto. Se il pattern segnalato non è effettivamente presente nel codice, segnalalo come falso positivo.";
  }
  // Security
  if (cat === "security") {
    return "Riprova: applica il fix di sicurezza descritto (input validation, escape, rate-limit, ecc.). Verifica che non si introducano regressioni funzionali.";
  }
  // Default generico
  return "Riprova: applica il fix specifico per il problema descritto sopra. Se il pattern segnalato non è effettivamente presente nel codice, segnalalo come falso positivo invece di modificare il file.";
}

export type Tc = Theme;

export interface FixQueueItem {
  filePath: string;
  findings: QualityFinding[];
}
