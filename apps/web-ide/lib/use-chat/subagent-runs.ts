/**
 * Estrazione dei run-id dei sub-agenti dal tool_result, per agganciare i loro
 * step alla chat mentre lavorano.
 *
 * Punto unico (regola L) e SEGNALE STRUTTURATO (regola M). Prima la chat faceva
 * due cose sbagliate insieme:
 *
 *  1. ascoltava `dispatch_subtask`, che e' uno stub disabilitato dal refactor
 *     Fase 4 (mig 0345) — i tool vivi sono `dispatch_subagent` e
 *     `dispatch_subagents`, quindi la sottoscrizione non scattava mai;
 *  2. ricavava l'id con `toolResult.match(/ID:\s*([0-9a-f-]{36})/i)`, cioe'
 *     leggendo la PROSA del risultato. Il backend il campo lo dichiara —
 *     `subagent_run_id`, `const K_SUB_RUN_ID` in
 *     `crates/mcp-core/src/agent_tools/subagent_native.rs:67`, con il commento
 *     "i consumatori (frontend e test) leggono le stesse [chiavi]" — ma nessuno
 *     lo leggeva. Un cambio di wording del messaggio avrebbe rotto la
 *     sottoscrizione senza un errore da nessuna parte.
 *
 * Il campo e' presente in TUTTI i rami di ritorno del backend: chiusura
 * riuscita (`finalize_success`), timeout, e dispatch in background.
 */

/** Nomi dei tool che avviano sub-run. `dispatch_subtask` NON e' incluso: e' lo
 *  stub disabilitato dalla mig 0345 e non produce alcun sub-run. */
const SUBAGENT_DISPATCH_TOOLS = new Set(["dispatch_subagent", "dispatch_subagents"]);

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function pushIfRunId(out: string[], value: unknown): void {
  if (typeof value === "string" && UUID_RE.test(value) && !out.includes(value)) {
    out.push(value);
  }
}

/**
 * Run-id dei sub-agenti avviati da questo step, in ordine di apparizione.
 * Array vuoto se il tool non e' un dispatch, se il risultato non e' JSON, o se
 * non dichiara alcun sub-run: nessuno di questi casi e' un errore.
 *
 * Forme accettate, tutte prodotte da `subagent_native.rs`:
 *  - singolo:            `{ subagent_run_id, kind, status, ... }`
 *  - batch:              `{ results: [ { subagent_run_id, ... }, ... ] }`
 *  - batch in background: `{ background_dispatched: true, child_run_ids: [...] }`
 */
export function childRunIdsFromToolResult(
  toolName: string | null | undefined,
  toolResult: string | null | undefined,
): string[] {
  if (!toolName || !toolResult || !SUBAGENT_DISPATCH_TOOLS.has(toolName)) return [];

  let parsed: unknown;
  try {
    parsed = JSON.parse(toolResult);
  } catch {
    // Il tool ha risposto con un messaggio d'errore in prosa (es. parametro
    // mancante): nessun sub-run e' partito, quindi non c'e' nulla da agganciare.
    return [];
  }
  if (!parsed || typeof parsed !== "object") return [];
  const root = parsed as Record<string, unknown>;

  const ids: string[] = [];
  pushIfRunId(ids, root.subagent_run_id);
  if (Array.isArray(root.child_run_ids)) {
    for (const v of root.child_run_ids) pushIfRunId(ids, v);
  }
  if (Array.isArray(root.results)) {
    for (const r of root.results) {
      if (r && typeof r === "object") {
        pushIfRunId(ids, (r as Record<string, unknown>).subagent_run_id);
      }
    }
  }
  return ids;
}
