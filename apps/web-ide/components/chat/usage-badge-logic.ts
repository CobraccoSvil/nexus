// Logica pura del badge usage sotto il messaggio assistant (token + costo del
// turno). Estratta dal JSX per essere testabile senza React — stesso pattern di
// provider-icon-logic.ts e step-detail-logic.ts.
//
// PERCHE' ESISTE (regola M). Il badge deduceva la SCALA dei numeri con una
// soglia: `const cumulative = total > lastIn + lastOut + 50`. Da li' decideva se
// scrivere "N token" oppure "N token totali (ultima chiamata: ...)".
// Tre difetti, tutti misurati:
//   1. Non misurava affatto la cumulativita': `total` conta anche i token di
//      cache (sono dentro il prompt, che e' LORDO), quindi la soglia scattava
//      sui run con molta cache e taceva sugli altri. Sui dati reali: 0% di
//      precisione.
//   2. I due campi confrontati contenevano lo STESSO valore (la riconciliazione
//      col ledger non avveniva mai), quindi il ramo "totali" era di fatto morto.
//   3. Appena la riconciliazione si riattiva, `promptTokens` diventa il
//      cumulativo del run e la soglia cambia comportamento in SILENZIO su tutti
//      i badge.
// Un'etichetta non si indovina da un confronto di grandezze: si scrive quella
// giusta, perche' la semantica dei campi e' nota a priori (vedi sotto).

/** Campi usage del messaggio assistant necessari al badge. */
export interface UsageBadgeInput {
  totalTokens?: number;
  promptTokens?: number;
  completionTokens?: number;
  totalCost?: number;
  currency?: string;
  provider?: string;
  model?: string;
}

export interface UsageBadgeView {
  /** Es. "192.164 token del run". Assente se non ci sono token da mostrare. */
  tokensLabel?: string;
  /** Es. "189.025 in / 3.139 out". Assente se il dettaglio non e' disponibile. */
  breakdownLabel?: string;
  /** Es. "$0.0853 USD". Assente se il costo e' 0/assente. */
  costLabel?: string;
  /** Es. "modello finale: mistral/mistral-large-2512". Assente se ignoto. */
  modelLabel?: string;
}

function it(n: number): string {
  return n.toLocaleString("it-IT");
}

/**
 * Vista del badge dai campi del messaggio.
 *
 * SEMANTICA DEI CAMPI (dal backend, non dedotta qui):
 * `totalTokens`/`promptTokens`/`completionTokens` sono i totali del RUN INTERO,
 * riconciliati da `ai_usage_ledger` (una riga per chiamata LLM). Sono quindi
 * coerenti tra loro: in + out = total — il ledger scrive `total_tokens` come
 * `prompt_tokens + completion_tokens`, col prompt LORDO — e nessuna etichetta
 * condizionale serve.
 * Il riempimento del contesto (prompt dell'ULTIMA iterazione) e' un dato diverso,
 * vive in `lastPromptTokens` e ha gia' il suo indicatore dedicato: NON si mescola
 * qui.
 *
 * Residuo dichiarato: se il gateway non ha contabilizzato il run (nessuna riga di
 * ledger — provider che non scrive ledger, o DB dei costi irraggiungibile), il
 * backend pubblica i contatori dell'ultimo turno. In quel caso "del run" e'
 * un'approssimazione per difetto. Caso misurato su 0 run su 580; renderlo
 * esplicito richiederebbe un campo strutturato dedicato, non un'euristica.
 */
export function usageBadgeView(m: UsageBadgeInput): UsageBadgeView {
  const out: UsageBadgeView = {};

  if (m.provider && m.model) {
    // "modello finale": il campo porta il modello dell'ULTIMA iterazione, mentre i
    // token/costo accanto sono di TUTTO il run — che puo' aver usato piu' modelli
    // (cascade/escalation). Senza il qualificatore il badge attribuisce a un solo
    // modello il lavoro di tutti: misurato un run da 618.984 token attribuito a
    // google, con il 65% dei token effettivamente consumati da mistral. Il
    // dettaglio per-modello e' nel footer costo-per-provider del nastro attivita'.
    out.modelLabel = `modello finale: ${m.provider}/${m.model}`;
  }

  const total = m.totalTokens ?? 0;
  if (total > 0) out.tokensLabel = `${it(total)} token del run`;

  const inTok = m.promptTokens ?? 0;
  const outTok = m.completionTokens ?? 0;
  if (inTok > 0 || outTok > 0) {
    out.breakdownLabel = `${it(inTok)} in / ${it(outTok)} out`;
  }

  const cost = m.totalCost ?? 0;
  if (cost > 0) out.costLabel = `$${cost.toFixed(4)} ${m.currency ?? "USD"}`;

  return out;
}
