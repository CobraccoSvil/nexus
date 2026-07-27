// Logica PURA per la resa leggibile di parametri e risultato dei tool (ADR
// 0037), SENZA dipendenze da React: cosi' e' testabile con `node --test`.
// Il componente step-detail.tsx la riesporta; i renderer la usano da li'.

/** Formatta l'input strutturato di un tool in testo leggibile "chiave: valore",
 *  MAI il JSON grezzo se evitabile. */
export function formatStepInput(input: Record<string, unknown>): string {
  const lines: string[] = [];
  for (const [key, val] of Object.entries(input)) {
    lines.push(`${key}: ${formatValue(val)}`);
  }
  return lines.join("\n");
}

/** Rende un valore in forma leggibile:
 *  - stringhe as-is (comprimendo solo quelle enormi > 300 char a placeholder);
 *  - numeri/boolean/null diretti;
 *  - array corti in linea; oggetti annidati semplici come "k=v; k2=v2" (mai JSON
 *    grezzo se evitabile); strutture profonde/grandi -> placeholder con lunghezza. */
export function formatValue(val: unknown): string {
  if (val === null) return "null";
  if (val === undefined) return "—";
  if (typeof val === "string") {
    return val.length > 300 ? `[${val.length} car.]` : val;
  }
  if (typeof val === "number" || typeof val === "boolean") return String(val);
  if (Array.isArray(val)) {
    if (val.every((x) => typeof x === "string" || typeof x === "number" || typeof x === "boolean")) {
      const joined = val.map(String).join(", ");
      return joined.length > 300 ? `[${val.length} elementi]` : joined;
    }
    return `[${val.length} elementi]`;
  }
  if (typeof val === "object") {
    const entries = Object.entries(val as Record<string, unknown>);
    const flat = entries.every(
      ([, v]) =>
        v === null ||
        typeof v === "string" ||
        typeof v === "number" ||
        typeof v === "boolean",
    );
    if (flat) {
      const rendered = entries
        .map(([k, v]) => `${k}=${typeof v === "string" && v.length > 120 ? `[${v.length} car.]` : String(v)}`)
        .join("; ");
      return rendered.length > 300 ? `[oggetto, ${entries.length} campi]` : rendered;
    }
    const j = JSON.stringify(val);
    return `[oggetto, ${j.length} car.]`;
  }
  return String(val);
}

/** Path del file bersaglio di un tool dai suoi input STRUTTURATI (regola M: le
 *  chiavi note, mai un guess sul testo). Punto unico (regola L) condiviso da chi
 *  deve "aprire il file" di uno step: notifiche del run (passo fallito, azione
 *  HITL) e simili. `undefined` se nessuna chiave path e' presente. */
export function filePathFromToolInput(
  input: Record<string, unknown> | undefined,
): string | undefined {
  if (!input) return undefined;
  for (const key of ["path", "file_path", "filename", "file"]) {
    const v = input[key];
    if (typeof v === "string" && v.trim().length > 0) return v.trim();
  }
  return undefined;
}

// ── Umanizzazione del RISULTATO tool ────────────────────────────────────────

export interface HumanToolResult {
  text: string;
  isError?: boolean;
}

/** Indizi di errore in un valore di stato del risultato tool. */
function statusIsError(status: unknown): boolean {
  if (typeof status !== "string") return false;
  const s = status.toLowerCase();
  return s === "error" || s === "failed" || s === "failure" || s === "err";
}

/**
 * Umanizza il RISULTATO di un tool (punto unico, testabile). Molti step storici
 * serializzano il risultato come JSON `{"content":"...","status":"..."}` con i
 * newline escaped: qui li rendiamo leggibili.
 *  - JSON con `content` string -> ritorna quel content, con i "\n" resi come
 *    newline REALI (niente involucro {content,status});
 *  - errore segnalato da `status`/`error`/`is_error` -> isError=true;
 *  - non-JSON parseabile -> ritorna il raw invariato.
 * Nessuna deduzione dell'esito dal TESTO libero (regola M): l'errore si legge
 * dai campi strutturati, non dal contenuto in prosa.
 */
export function humanizeToolResult(raw: string): HumanToolResult {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) return { text: "" };
  if (!(trimmed.startsWith("{") || trimmed.startsWith("["))) {
    return { text: raw };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return { text: raw };
  }
  if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const obj = parsed as Record<string, unknown>;
    const isError = statusIsError(obj.status) || obj.error != null || obj.is_error === true;
    const bodyRaw =
      (typeof obj.content === "string" && obj.content) ||
      (typeof obj.text === "string" && obj.text) ||
      (typeof obj.message === "string" && obj.message) ||
      (typeof obj.error === "string" && obj.error) ||
      (typeof obj.result === "string" && obj.result) ||
      null;
    if (bodyRaw != null) {
      const text = bodyRaw.replace(/\\n/g, "\n").replace(/\\t/g, "\t");
      return { text, isError: isError || undefined };
    }
    return { text: formatStepInput(obj), isError: isError || undefined };
  }
  return { text: typeof parsed === "string" ? parsed : formatValue(parsed) };
}
