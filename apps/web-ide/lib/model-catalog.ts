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
import type { ProviderTokenBucket } from "./use-chat/activity-stream";

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
 * Costo USD di un bucket di token (una coppia provider/model gia' aggregata):
 * il PREZZATORE che il footer costo-per-provider passa a `providerCostBreakdown`.
 *
 * Vive qui, e non dentro il componente, per una ragione di misura (regola O):
 * finche' stava nel `.tsx` nessun test poteva chiamarlo — il modulo tira dentro
 * React — e il test della ripartizione si iniettava un prezzatore PROPRIO che
 * ricopiava a mano questo stesso adattamento. Le due copie potevano divergere in
 * silenzio: togliendo di qui le quantita' di cache, tutti i test restavano verdi.
 * Ora il test chiama questa funzione, cioe' quella che gira nel footer.
 *
 * L'unica scelta locale al footer che resta incapsulata qui e' il modello non a
 * catalogo: contributo ZERO invece che voce nascosta. In una ripartizione per
 * provider omettere una voce falserebbe le proporzioni delle altre; il `null` di
 * `costFromCatalog` resta invece la risposta onesta per la cella singola.
 */
export function bucketCost(
  bucket: ProviderTokenBucket,
  catalog: ModelCatalogEntry[],
): number {
  const entry = findCatalogEntry(catalog, bucket.provider, bucket.model);
  return (
    costFromCatalog(
      entry,
      bucket.inputTokens,
      bucket.outputTokens,
      // Le quantita' di cache vanno passate: omettendole, il loro sconto non lo
      // applica nessuno e il footer dichiara PIU' del ledger, tanto piu' quanto
      // meglio la cache ha servito.
      bucket.cacheReadTokens,
      bucket.cacheCreationTokens,
    ) ?? 0
  );
}

/**
 * Costo USD di una singola trace, `null` se il modello non e' a catalogo (la
 * cella si nasconde: un trattino e' onesto, uno zero e' una bugia).
 *
 * Sta qui per la stessa ragione di `bucketCost`: dentro `inline-trace-panel.tsx`
 * nessun test poteva chiamarla, e togliere le due quantita' di cache dalla
 * chiamata lasciava verde tutta la suite (regola O).
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
