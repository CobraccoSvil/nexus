// Il confine wire di `GET /api/billing/session-usage`: la forma che il backend
// manda e la traduzione verso i tipi del frontend.
//
// PERCHE' E' UN MODULO A SE' (regola O). Qui vive l'unica parte del client che
// puo' divergere dal produttore Rust senza che nessun compilatore se ne accorga:
// i NOMI dei campi. Il difetto e' gia' accaduto su questo confine — un tipo TS
// in snake_case contro un wire camelCase, ogni lettura `undefined`, e un `?? 0` a
// valle che la trasformava in «costo zero» — e i test dei due lati erano verdi
// perche' nessuno attraversava la giunzione.
//
// Il modulo NON ha dipendenze: cosi' il test puo' caricarlo e dargli in pasto la
// stessa fixture che il test Rust asserisce essere il prodotto di
// `corpo_session_usage` (`__wire__/session-usage.json`). La produzione passa di
// qui, non da una copia: `billing.ts::getSessionUsage` chiama queste due
// funzioni e non traduce nulla per conto proprio.

/** Il corpo che `crates/mcp-core/src/billing.rs` (`corpo_session_usage`) emette.
 *  Snake_case: quel corpo e' costruito a mano con `json!`, NON da un tipo
 *  annotato `#[serde(rename_all = "camelCase")]`. */
export interface SessionUsageWire {
  session_id: string;
  total_tokens: number;
  total_cost_usd: number;
  breakdown: Array<{ model: string; tokens: number; cost_usd: number }>;
  /** `null` quando la richiesta non chiede un run, o quando il run non
   *  appartiene alla sessione. Assente sui backend anteriori a questo campo. */
  current_run?: {
    run_id: string;
    total_tokens: number;
    total_cost_usd: number;
    run_count: number;
    /** Ripartizione del RUN, dalla stessa fonte e dallo stesso elenco del suo
     *  totale. Assente sui backend anteriori a questo campo. */
    breakdown?: Array<{ model: string; tokens: number; cost_usd: number }>;
  } | null;
}

/** Una riga di ripartizione come la manda il ledger: `model` e' l'ETICHETTA
 *  intera che `usage_by_model_for_runs` compone, cioe' `provider/model`. */
export interface RigaRipartizione {
  model: string;
  tokens: number;
  costUsd: number;
}

export interface SessionUsage {
  totalTokens: number;
  totalCostUsd: number;
  breakdown: RigaRipartizione[];
  /** Consumo del run richiesto e del lavoro che ha delegato. `null` se non
   *  richiesto o non pertinente — mai un oggetto a zeri: «non ho un perimetro»
   *  e «non e' costato nulla» sono due cose diverse (regola Q). */
  currentRun: {
    runId: string;
    totalTokens: number;
    totalCostUsd: number;
    runCount: number;
    /** Ripartizione per modello del run. Lista VUOTA quando il backend non
     *  manda il campo: «non me l'ha detto» e «non ha speso nulla» portano
     *  entrambi a non mostrare voci, ma nessuna delle due e' uno zero
     *  inventato — il totale accanto resta quello del ledger. */
    breakdown: RigaRipartizione[];
  } | null;
}


/**
 * Traduzione del wire nei tipi del frontend.
 *
 * Niente `?? 0` sui campi che il wire garantisce: un nome sbagliato deve
 * diventare `undefined` visibile, non un plausibile zero. Il `?? null` su
 * `current_run` e' un'altra cosa e non maschera nulla — distingue un backend che
 * non parla ancora questa versione del contratto da un perimetro che non c'e', e
 * in entrambi i casi non c'e' un consumo di run da mostrare.
 */
export function sessionUsageDalWire(res: SessionUsageWire): SessionUsage {
  const run = res.current_run;
  return {
    totalTokens: res.total_tokens,
    totalCostUsd: res.total_cost_usd,
    breakdown: ripartizioneDalWire(res.breakdown),
    currentRun: run
      ? {
          runId: run.run_id,
          totalTokens: run.total_tokens,
          totalCostUsd: run.total_cost_usd,
          runCount: run.run_count,
          breakdown: ripartizioneDalWire(run.breakdown),
        }
      : null,
  };
}

/** La traduzione di UNA ripartizione, usata dai due perimetri. Il produttore
 *  Rust ne compone una sola (`ripartizione_wire`): due letture divergerebbero
 *  al primo campo rinominato, e lo farebbero su un solo perimetro. */
function ripartizioneDalWire(
  righe: Array<{ model: string; tokens: number; cost_usd: number }> | undefined,
): RigaRipartizione[] {
  return (righe ?? []).map((b) => ({ model: b.model, tokens: b.tokens, costUsd: b.cost_usd }));
}

/**
 * URL della richiesta. `runId` chiede in piu' il perimetro di QUEL run (se
 * stesso + i sub-run che ha dispatchato): senza quel parametro il backend non
 * puo' calcolarlo, e il contatore non avrebbe il numero su cui si decide se un
 * run e' costato troppo.
 */
export function urlSessionUsage(
  apiBase: string,
  origin: string,
  sessionId: string,
  runId?: string,
): string {
  const url = new URL(`${apiBase}/api/billing/session-usage`, origin);
  url.searchParams.set("session_id", sessionId);
  if (runId) url.searchParams.set("run_id", runId);
  return url.toString();
}
