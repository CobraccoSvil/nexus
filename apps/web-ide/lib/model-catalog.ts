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
 * I `cacheReadTokens` sono gia' compresi negli `inputTokens` (sono token di
 * prompt fatturati a tariffa ridotta), quindi si scorporano dall'input e si
 * ritariffano. Quando il catalog non espone la tariffa di cache
 * (`cacheReadCostPerMillionTokens` null) quei token restano a tariffa piena:
 * si sovrastima di poco, invece di applicare un rapporto inventato che sarebbe
 * giusto per un provider e sbagliato per gli altri.
 */
export function costFromCatalog(
  entry: ModelCatalogEntry | null,
  inputTokens: number,
  outputTokens: number,
  cacheReadTokens = 0,
): number | null {
  if (!entry) return null;
  const cacheRate = entry.cacheReadCostPerMillionTokens;
  const cached = cacheRate != null ? cacheReadTokens : 0;
  const billableInput = Math.max(0, inputTokens - cached);
  const inputCost = (billableInput * entry.inputCostPerMillionTokens) / 1_000_000;
  const cacheCost = (cached * (cacheRate ?? 0)) / 1_000_000;
  const outputCost = (outputTokens * entry.outputCostPerMillionTokens) / 1_000_000;
  return inputCost + cacheCost + outputCost;
}

/** Format USD con soglie di precisione adattive. */
export function formatCostUsd(usd: number): string {
  if (usd <= 0)     return "$0.000";
  if (usd < 0.0001) return "< $0.0001";
  if (usd < 0.001)  return `$${usd.toFixed(5)}`;
  if (usd < 0.01)   return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(3)}`;
}
