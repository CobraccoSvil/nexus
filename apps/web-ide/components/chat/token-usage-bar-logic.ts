// Logica pura del contatore sotto la chat («N token - $X»). Estratta dal JSX per
// essere testabile senza React — stesso pattern di usage-badge-logic.ts.
//
// PERCHE' ESISTE (regole L e Q). Il contatore mostrava due numeri che non
// potevano riferirsi alla stessa cosa. MISURATO l'08/08/2026 su gestione-corsi,
// a run in corso:
//
//     UI:      639 token  -  $2.14
//     ledger:  27.813.580 token  -  $2,6024   (758 righe finalizzate, 74 run)
//
// I token erano lo 0,0023% del reale; il costo era dello stesso ordine del
// totale, ma di un ALTRO insieme. Non due sviste: quattro produttori che
// scrivevano lo stesso stato con quattro perimetri diversi, e uno solo leggeva
// il ledger.
//
//   - `GET /api/billing/session-usage` -> ledger, perimetro SESSIONE (corretto)
//   - evento SSE `agent_usage`         -> i token del TURNO, nessun costo
//   - evento `ChatMessageAdded`        -> i totali del turno di chat singolo
//   - evento `ChatSessionCompacted`    -> costo sommato dai metadata dei MESSAGGI
//
// Il secondo spiega entrambi gli errori insieme: sostituiva i token col valore
// dell'ultima chiamata, e non portando alcun costo lasciava in video quello di
// sessione dell'ultima lettura autoritativa. Da qui il segno opposto.
//
// Il rimedio non e' sommare meglio: e' che il contatore ha UN produttore
// (`refreshSessionUsage`), e gli eventi sono segnali di avanzamento che ne
// innescano la rilettura. Questo modulo rende quel contratto un tipo.

/** Token e costo di un perimetro, come li riporta il ledger. */
export interface UsageTotals {
  totalTokens: number;
  totalCostUsd: number;
}

/** Il consumo del run in corso e del lavoro che ha delegato. */
export interface CurrentRunUsage extends UsageTotals {
  runId: string;
  /** Quanti run compongono il perimetro: 1 = nessuna delega. */
  runCount: number;
}

/**
 * Stato del contatore.
 *
 * L'ignoto e' una VARIANTE, non uno zero e non l'ultimo valore noto lasciato in
 * video (regola Q): un numero vecchio e uno fresco sono indistinguibili, ed e'
 * esattamente il modo in cui il costo di un altro insieme e' rimasto sullo
 * schermo per l'intera durata di un run.
 */
export type SessionUsageState =
  | { stato: "in_attesa" }
  | { stato: "noto"; sessione: UsageTotals; run: CurrentRunUsage | null }
  | { stato: "non_disponibile"; motivo: string };

export interface UsageBarView {
  /** True se la barra ha qualcosa da mostrare (altrimenti il chiamante non la rende). */
  visibile: boolean;
  /** Es. "27.8M token", oppure il marcatore di indisponibilita'. */
  tokensLabel: string;
  /** Es. "$2.60", oppure il marcatore di indisponibilita'. */
  costLabel: string;
  /** True quando i numeri sono una misura; false quando sono un'assenza dichiarata. */
  misurato: boolean;
  /** Tooltip della riga: dichiara SEMPRE il perimetro dei numeri mostrati. */
  titolo: string;
  /** Riga "di cui il run corrente" del pannello espanso. Assente se non pertinente. */
  runLabel?: string;
}

/** Il segno che sostituisce un numero che non e' stato misurato. */
export const NON_MISURATO = "—";

function tokenCompatti(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M token`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K token`;
  return `${n} token`;
}

/** Il costo con la precisione che serve a leggerlo: sotto il centesimo servono
 *  quattro decimali, sopra ne bastano due. */
export function costoLeggibile(usd: number): string {
  if (usd === 0) return "$0.00";
  return usd < 0.01 ? `$${usd.toFixed(4)}` : `$${usd.toFixed(2)}`;
}

function it(n: number): string {
  return n.toLocaleString("it-IT");
}

/**
 * Vista del contatore dallo stato.
 *
 * Il perimetro dei due numeri principali e' SEMPRE la sessione — cioe' cio' che
 * l'etichetta ha sempre promesso e cio' che l'endpoint autoritativo risponde. Il
 * consumo del run corrente e' una domanda diversa (sullo stesso istante misurato:
 * $2,6024 contro $0,1272, venti volte) e vive in una riga sua, dichiarata come
 * tale: mescolarli e' il difetto da cui questo modulo nasce.
 */
export function usageBarView(state: SessionUsageState): UsageBarView {
  if (state.stato === "in_attesa") {
    return {
      visibile: false,
      tokensLabel: NON_MISURATO,
      costLabel: NON_MISURATO,
      misurato: false,
      titolo: "Contabilita' non ancora letta",
    };
  }

  if (state.stato === "non_disponibile") {
    // Visibile di proposito: un contatore che sparisce quando la lettura
    // fallisce e' indistinguibile da una chat che non ha ancora speso nulla.
    return {
      visibile: true,
      tokensLabel: NON_MISURATO,
      costLabel: NON_MISURATO,
      misurato: false,
      titolo: `Contabilita' non leggibile: ${state.motivo}. Nessun numero mostrato finche' non torna disponibile.`,
    };
  }

  const { sessione, run } = state;
  const vuoto = sessione.totalTokens === 0 && sessione.totalCostUsd === 0;

  const view: UsageBarView = {
    visibile: !vuoto,
    tokensLabel: tokenCompatti(sessione.totalTokens),
    costLabel: costoLeggibile(sessione.totalCostUsd),
    misurato: true,
    titolo: `Token e costo di TUTTA la conversazione (${it(
      sessione.totalTokens,
    )} token, ${costoLeggibile(sessione.totalCostUsd)}), sub-run inclusi, dal ledger.`,
  };

  if (run) {
    // "su N run" perche' $0,13 su un run solo e $0,13 su quattro dicono cose
    // diverse a chi sta valutando se un run e' costato troppo.
    const quanti = run.runCount === 1 ? "1 run" : `${run.runCount} run`;
    view.runLabel = `${it(run.totalTokens)} token - ${costoLeggibile(
      run.totalCostUsd,
    )} (${quanti})`;
  }

  return view;
}

/**
 * Il rapporto che colora la barra e ne riempie la traccia.
 *
 * Ritorna `null` quando non c'e' nulla da rapportare: nessun budget, nessuna
 * finestra di contesto, oppure numeri non misurati. Un rapporto calcolato su
 * un'assenza sarebbe una barra piena o vuota che afferma qualcosa.
 */
export function rapportoBarra(args: {
  misurato: boolean;
  totalCostUsd: number;
  budgetUsd?: number | null;
  contextWindow?: number | null;
  lastInputTokens?: number | null;
}): { valore: number; base: "budget" | "ctx" } | null {
  if (!args.misurato) return null;
  if (args.budgetUsd != null && args.budgetUsd > 0) {
    return { valore: args.totalCostUsd / args.budgetUsd, base: "budget" };
  }
  if (
    args.contextWindow != null &&
    args.contextWindow > 0 &&
    args.lastInputTokens != null &&
    args.lastInputTokens > 0
  ) {
    return { valore: args.lastInputTokens / args.contextWindow, base: "ctx" };
  }
  return null;
}
