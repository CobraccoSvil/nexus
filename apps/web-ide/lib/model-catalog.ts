/**
 * Costo di un'interazione e sua formattazione. Punto unico (regola L) del
 * CALCOLO; il DATO non e' qui e non deve tornarci.
 *
 * Storia, perche' non si ripeta: questo file conteneva il listino di 27 modelli,
 * le context window e la lista dei modelli per provider, tutti scritti a mano,
 * con in testa una deroga dichiarata alla regola G ("hardcoded in deroga
 * temporanea"). La deroga e' costata: il listino conteneva ancora
 * `mistral-small-4`, nome che il provider ha deprecato, quindi il dropdown dei
 * profili proponeva un modello che risponde 400; e i modelli assenti dalla mappa
 * venivano prezzati zero invece che dichiarati sconosciuti.
 *
 * La fonte e' `ai_price_catalog` via `/api/models`, tipizzata in
 * `lib/api/models.ts` e servita dall'hook `usePricingCatalog`
 * (`components/chat/provider-badge.tsx`, cache 5 min). Se il catalog non copre
 * un modello il costo e' `null` e la UI NON lo mostra: un trattino e' onesto,
 * uno zero e' una bugia.
 */

import type { ModelCatalogEntry } from "./api/models";
import type { AITraceEvent } from "./api/agent";

/** Entry del catalogo per un (provider, model), o `null` se non coperto.
 *
 *  Il match e' esatto su entrambi i campi: il vecchio codice cercava col
 *  `includes()` sul solo nome modello, e bastava un catalogo con due varianti
 *  dello stesso prefisso per prezzare con la riga sbagliata. */
export function findCatalogEntry(
  catalog: ModelCatalogEntry[],
  provider: string | null | undefined,
  model: string | null | undefined,
): ModelCatalogEntry | null {
  if (!provider || !model) return null;
  return catalog.find((e) => e.provider === provider && e.model === model) ?? null;
}

/**
 * Costo in USD di una chiamata. Ritorna `null` se il modello non e' nel
 * catalogo — il chiamante nasconde la cella.
 *
 * ## `inputTokens` e' il prompt LORDO: si scorpora, non si somma
 *
 * `cacheReadTokens` e `cacheCreationTokens` sono SOTTOINSIEMI di `inputTokens`
 * (convenzione unica del sistema, fissata alla fonte da
 * `nexus_gateway::LlmUsage::normalized`, cosi' nessun consumatore deve sapere
 * quale convenzione usi il provider che ha risposto). Qui si tolgono dal monte a
 * tariffa piena e si ritariffano al loro prezzo: sommarli invece che scorporarli
 * conterebbe due volte gli stessi token.
 *
 * Il clamp a `>= 0` sul monte residuo copre il dato incoerente (cache dichiarata
 * maggiore del prompt): mai un input negativo che genererebbe credito.
 *
 * Tariffa mancante nel catalog (`null`): i token di cache NON diventano gratis
 * — sarebbe far sparire dal conto token realmente consumati. Restano a tariffa
 * piena di input, cioe' il costo che la UI mostrava prima che il catalog
 * esponesse le tariffe di cache. E' il limite superiore per le letture (sempre
 * piu' economiche) e una lieve sottostima per le scritture (~1.25x). Dichiarato
 * qui, invece di un rapporto inventato che sarebbe giusto per un provider e
 * sbagliato per gli altri.
 */
export function costFromCatalog(
  entry: ModelCatalogEntry | null,
  inputTokens: number,
  outputTokens: number,
  cacheReadTokens = 0,
  cacheCreationTokens = 0,
): number | null {
  if (!entry) return null;
  const inputRate = entry.inputCostPerMillionTokens;
  const cacheRead = Math.max(0, cacheReadTokens);
  const cacheCreation = Math.max(0, cacheCreationTokens);
  // Tariffa assente: il token torna nel monte a prezzo pieno di input.
  const cacheReadRate = entry.cacheReadCostPerMillionTokens ?? inputRate;
  const cacheCreationRate = entry.cacheCreationCostPerMillionTokens ?? inputRate;
  const billableInput = Math.max(0, Math.max(0, inputTokens) - cacheRead - cacheCreation);
  const inputCost = (billableInput * inputRate) / 1_000_000;
  const cacheReadCost = (cacheRead * cacheReadRate) / 1_000_000;
  const cacheCreationCost = (cacheCreation * cacheCreationRate) / 1_000_000;
  const outputCost = (Math.max(0, outputTokens) * entry.outputCostPerMillionTokens) / 1_000_000;
  return inputCost + cacheReadCost + cacheCreationCost + outputCost;
}

/**
 * Costo USD di una singola trace, `null` se il modello non e' a catalogo (la
 * cella si nasconde: un trattino e' onesto, uno zero e' una bugia).
 *
 * Sta qui e non dentro `inline-trace-panel.tsx` per una ragione di misura
 * (regola O): nel `.tsx` nessun test potrebbe chiamarla — il modulo tira dentro
 * React — e togliere le due quantita' di cache dalla chiamata lascerebbe verde
 * tutta la suite.
 *
 * NON e' piu' il prezzatore del footer costo-per-provider. Il gemello che lo
 * era (`bucketCost`, che prezzava col catalogo i token aggregati dalle TRACCE)
 * e' stato rimosso il 10/08/2026 insieme alla ripartizione che alimentava: quel
 * costo ora arriva gia' calcolato dal ledger, cioe' dalla stessa fonte del
 * totale che gli sta accanto. Qui resta la cella SINGOLA della trace, che e' una
 * domanda diversa — quanto e' costata QUESTA chiamata secondo il listino — e ha
 * un solo consumatore.
 *
 * I campi di cache sono opzionali sul wire — le trace persistite prima che il
 * campo esistesse non li portano — e in quel caso non c'e' nulla da scorporare.
 */
export function traceCost(
  trace: AITraceEvent,
  catalog: ModelCatalogEntry[],
): number | null {
  return costFromCatalog(
    findCatalogEntry(catalog, trace.provider, trace.model),
    trace.inputTokens ?? 0,
    trace.outputTokens ?? 0,
    trace.cacheReadTokens ?? 0,
    trace.cacheCreationTokens ?? 0,
  );
}

/** Format USD con soglie di precisione adattive. */
export function formatCostUsd(usd: number): string {
  if (usd <= 0)     return "$0.000";
  if (usd < 0.0001) return "< $0.0001";
  if (usd < 0.001)  return `$${usd.toFixed(5)}`;
  if (usd < 0.01)   return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(3)}`;
}
