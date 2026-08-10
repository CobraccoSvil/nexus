/**
 * La resa del TETTO DI SPESA di un fornitore, dichiarata una volta sola.
 *
 * Gemella di `renderDeclaration` in `gateway-providers.ts`, e per la stessa
 * ragione: il verdetto lo produce Rust (`mcp-core::provider_spend_cap`) come
 * campo tipizzato, e qui se ne fa una frase — mai il contrario (regola Q,
 * punto 3).
 *
 * MISURATO a schermo il 10/08/2026 su /admin: il riquadro BUDGET MENSILE
 * mostrava 5 fornitori mentre il riquadro PROVIDER accanto ne elencava 10. I
 * cinque non erano una scelta: sono esattamente la lista seminata a mano dalla
 * migrazione 0173 (anthropic, openai, google, mistral, deepseek). Ogni
 * fornitore onboardato dopo quella migrazione ottiene una riga solo QUANDO
 * SPENDE — `charge_provider_budget` fa un INSERT senza tetto, che prende il
 * DEFAULT 0 — e il pannello filtrava via proprio le righe a tetto 0 con un
 * `parseFloat(b.monthly_budget_usd) > 0` scritto in casa propria. Quindi
 * openrouter e kimi, secondo e quarto fornitore per chiamate reali, erano
 * invisibili PERCHE' nessuno aveva deciso un tetto per loro: il pannello
 * nascondeva esattamente i casi che doveva mostrare.
 */

/** Le varianti dichiarate da `mcp-core::provider_spend_cap`. */
export type SpendCap =
  | "capped"
  | "uncapped_spending"
  | "uncapped_idle"
  | "undetermined";

/** Cio' che il pannello mostra: l'etichetta breve e se il caso RICHIEDE UN INTERVENTO. */
export interface RenderedSpendCap {
  label: string;
  requiresAction: boolean;
}

/**
 * L'etichetta per il tetto di spesa, o `null` quando non c'e' nulla da dire.
 *
 * `null` non e' un esito conflazionato: l'esito sta nel campo `spend_cap`, e
 * questo e' il verdetto su cosa MOSTRARE. Un fornitore con un tetto regolare
 * non ha bisogno di una riga in piu' — la barra accanto lo dice gia' — e il
 * rumore e' il modo in cui una riga che conta smette di essere letta.
 *
 * Le due assenze di tetto restano DUE frasi perche' hanno due rimedi diversi:
 * `uncapped_spending` e' spesa in corso che nessuno fermera' (si decide un
 * tetto adesso), `uncapped_idle` e' un fornitore che semplicemente non ha
 * ancora speso (si decide quando serve).
 *
 * L'IGNOTO non diventa un allarme ne' una rassicurazione: una entry senza
 * `spend_cap` viene da un backend che non parla questa versione del contratto.
 */
export function renderSpendCap(spendCap: SpendCap | undefined): RenderedSpendCap | null {
  switch (spendCap) {
    case "uncapped_spending":
      return { label: "sta spendendo senza tetto", requiresAction: true };
    case "uncapped_idle":
      return { label: "nessun tetto impostato", requiresAction: false };
    case "undetermined":
      return { label: "tetto non leggibile", requiresAction: false };
    default:
      // `capped`, o campo assente: niente da mostrare.
      return null;
  }
}
