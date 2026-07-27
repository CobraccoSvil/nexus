// Unit test del calcolo costo dal catalogo (node --test, type-stripping).
//
// Il difetto che questi test presidiano: prima il listino era una mappa scritta
// a mano in questo modulo, e un modello assente veniva prezzato zero (o, con il
// vecchio match `includes()`, con la riga di un altro modello). Il costo ora
// viene dal catalogo del DB e "non lo so" e' un esito distinto da "gratis".
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  bucketCost,
  costFromCatalog,
  findCatalogEntry,
  formatCostUsd,
  traceCost,
} from "./model-catalog.ts";
import type { ModelCatalogEntry } from "./api/models.ts";

/** Entry minima: al calcolo servono solo i tre costi. */
function entry(over: Partial<ModelCatalogEntry> = {}): ModelCatalogEntry {
  return {
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    displayName: "Sonnet",
    inputCostPerMillionTokens: 3.0,
    outputCostPerMillionTokens: 15.0,
    cacheReadCostPerMillionTokens: 0.3,
    cacheCreationCostPerMillionTokens: 3.75,
    currency: "USD",
    performanceTier: "high",
    tierSource: "synced",
    agenticIndex: null,
    qualificationState: "qualified",
    speedTier: "medium",
    capabilities: [],
    contextWindow: 200_000,
    supportsToolUse: true,
    batchDiscountPct: 0,
    isFeatured: false,
    isEnabled: true,
    ...over,
  };
}

test("modello non a catalogo: null, non zero", () => {
  // La distinzione e' il punto: la UI nasconde la cella su null, mentre uno
  // zero verrebbe letto come "questa chiamata non e' costata niente".
  assert.equal(costFromCatalog(null, 1_000_000, 1_000_000), null);
  assert.equal(findCatalogEntry([entry()], "openai", "claude-sonnet-4-6"), null);
  assert.equal(findCatalogEntry([entry()], "anthropic", "gpt-4o"), null);
});

test("match esatto su provider E modello", () => {
  const catalog = [
    entry({ provider: "anthropic", model: "claude-haiku-4-5" }),
    entry({ provider: "anthropic", model: "claude-haiku-4-5-20251001" }),
  ];
  // Col vecchio `includes()` la prima riga catturava anche il nome esteso.
  const found = findCatalogEntry(catalog, "anthropic", "claude-haiku-4-5-20251001");
  assert.equal(found?.model, "claude-haiku-4-5-20251001");
});

test("costo: input + output alle tariffe del catalogo", () => {
  const c = costFromCatalog(entry(), 1_000_000, 1_000_000);
  assert.equal(c, 18.0); // 3.0 input + 15.0 output
});

test("i token di cache si scorporano dal prompt lordo e pagano la loro tariffa", () => {
  // Convenzione della fonte (`nexus_gateway::LlmUsage::normalized`): l'input e'
  // il LORDO. 1M di prompt di cui 400k letti da cache e 100k scritti: restano
  // 500k a tariffa piena -> 1.5 + 0.12 + 0.375 = 1.995.
  //
  // MUTAZIONE che rende rosso: sommare le quantita' di cache invece di
  // scorporarle (la premessa opposta) -> 3.495, cioe' 500k token fatturati due
  // volte, e il pannello mostrerebbe piu' del ledger.
  const c = costFromCatalog(entry(), 1_000_000, 0, 400_000, 100_000);
  assert.ok(Math.abs((c ?? 0) - (1.5 + 0.12 + 0.375)) < 1e-9, `atteso 1.995, ottenuto ${c}`);
});

test("senza tariffa di cache nel catalogo i token pagano la tariffa piena, mai zero", () => {
  // Niente rapporto inventato (il vecchio codice assumeva input * 0.1, giusto
  // per Anthropic e sbagliato per gli altri) e nemmeno zero, che farebbe
  // sparire dal conto token realmente consumati: i token di cache rientrano nel
  // monte da cui erano stati tolti, cioe' l'intero prompt a tariffa piena — lo
  // stesso costo che la UI mostrava prima che il catalog esponesse le tariffe.
  const senzaTariffe = entry({
    cacheReadCostPerMillionTokens: null,
    cacheCreationCostPerMillionTokens: null,
  });
  const c = costFromCatalog(senzaTariffe, 1_000_000, 0, 400_000, 100_000);
  assert.ok(Math.abs((c ?? 0) - 3.0) < 1e-9, `atteso 3.0 (1M a 3.0/M), ottenuto ${c}`);
});

test("cache maggiore del prompt: l'input residuo clampa a zero, mai un credito", () => {
  // Dato incoerente del provider: il monte a tariffa piena non puo' andare
  // sotto zero e sottrarre costo alle altre voci.
  const c = costFromCatalog(entry(), 10, 0, 1_000_000, 0);
  assert.ok(Math.abs((c ?? 0) - 0.3) < 1e-9, `atteso 0.3 (1M a 0.3/M), ottenuto ${c}`);
});

test("bucketCost: modello non a catalogo vale zero, non fa sparire la voce", () => {
  // Scelta locale al footer costo-per-provider, che qui e' incapsulata: in una
  // RIPARTIZIONE una voce nascosta falserebbe le proporzioni delle altre, quindi
  // il modello sconosciuto contribuisce zero e resta elencato. Diverso dal `null`
  // di `costFromCatalog`, che per la cella singola e' la risposta onesta.
  //
  // MUTAZIONE che lo rende rosso: propagare il `null` invece dello zero.
  const bucket = {
    provider: "provider-mai-visto",
    model: "modello-mai-visto",
    inputTokens: 1_000_000,
    outputTokens: 1_000_000,
    cacheReadTokens: 0,
    cacheCreationTokens: 0,
  };
  assert.equal(bucketCost(bucket, [entry()]), 0);
  // Contro-prova sullo stesso bucket, col modello a catalogo: non e' lo zero di
  // un calcolo che non gira, e' quello di un modello che non conosciamo.
  const noto = { ...bucket, provider: "anthropic", model: "claude-sonnet-4-6" };
  assert.equal(bucketCost(noto, [entry()]), 18.0);
});

test("bucketCost passa al calcolo le quantita' di cache del bucket", () => {
  // E' il prezzatore che gira nel footer (`ActivityCostFooter` gli passa solo il
  // catalogo di `/api/models`): se smette di inoltrare i due conteggi di cache, i
  // token serviti dalla cache tornano a tariffa piena di input e il pannello
  // dichiara piu' del ledger.
  //
  // MUTAZIONE che lo rende rosso: togliere `bucket.cacheReadTokens` e
  // `bucket.cacheCreationTokens` dalla chiamata a `costFromCatalog` -> 3.0.
  const c = bucketCost(
    {
      provider: "anthropic",
      model: "claude-sonnet-4-6",
      inputTokens: 1_000_000,
      outputTokens: 0,
      cacheReadTokens: 400_000,
      cacheCreationTokens: 100_000,
    },
    [entry()],
  );
  assert.ok(Math.abs(c - (1.5 + 0.12 + 0.375)) < 1e-9, `atteso 1.995, ottenuto ${c}`);
});

test("traceCost scorpora la cache della trace e resta null fuori catalogo", () => {
  // Prezzatore del pannello trace inline. Stessa specie del bucket: se smette di
  // inoltrare i due campi di cache, il pannello dichiara piu' del ledger.
  //
  // MUTAZIONE che lo rende rosso: togliere `trace.cacheReadTokens` /
  // `trace.cacheCreationTokens` dalla chiamata a `costFromCatalog` -> 3.0.
  const base = {
    runId: "r",
    iteration: 1,
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    messagesSent: 1,
    toolsCount: 0,
    responseText: "",
    toolCalls: [],
    stopReason: "end_turn",
    timestamp: "2026-07-27T00:00:00Z",
    inputTokens: 1_000_000,
    outputTokens: 0,
    cacheReadTokens: 400_000,
    cacheCreationTokens: 100_000,
  };
  const c = traceCost(base, [entry()]);
  assert.ok(Math.abs((c ?? 0) - (1.5 + 0.12 + 0.375)) < 1e-9, `atteso 1.995, ottenuto ${c}`);

  // Trace persistita prima che i campi di cache esistessero: niente da
  // scorporare, tutto il prompt a tariffa piena. `undefined` non deve diventare
  // NaN lungo il calcolo.
  const vecchia = { ...base, cacheReadTokens: undefined, cacheCreationTokens: undefined };
  assert.equal(traceCost(vecchia, [entry()]), 3.0);

  // Modello fuori catalogo: `null`, non zero (la cella si nasconde).
  assert.equal(traceCost({ ...base, model: "modello-mai-visto" }, [entry()]), null);
});

test("formatCostUsd: soglie di precisione", () => {
  assert.equal(formatCostUsd(0), "$0.000");
  assert.equal(formatCostUsd(0.00005), "< $0.0001");
  assert.equal(formatCostUsd(1.23456), "$1.235");
});
