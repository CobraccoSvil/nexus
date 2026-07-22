// Unit test del calcolo costo dal catalogo (node --test, type-stripping).
//
// Il difetto che questi test presidiano: prima il listino era una mappa scritta
// a mano in questo modulo, e un modello assente veniva prezzato zero (o, con il
// vecchio match `includes()`, con la riga di un altro modello). Il costo ora
// viene dal catalogo del DB e "non lo so" e' un esito distinto da "gratis".
import { test } from "node:test";
import assert from "node:assert/strict";
import { costFromCatalog, findCatalogEntry, formatCostUsd } from "./model-catalog.ts";
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

test("i token di cache si scorporano dall'input e pagano la loro tariffa", () => {
  // 1M input di cui 400k letti da cache: 600k a 3.0/M + 400k a 0.3/M.
  const c = costFromCatalog(entry(), 1_000_000, 0, 400_000);
  assert.ok(Math.abs((c ?? 0) - (1.8 + 0.12)) < 1e-9, `atteso 1.92, ottenuto ${c}`);
});

test("senza tariffa di cache nel catalogo i token restano a tariffa piena", () => {
  // Niente rapporto inventato (il vecchio codice assumeva input * 0.1, giusto
  // per Anthropic e sbagliato per gli altri): si sovrastima, dichiaratamente.
  const c = costFromCatalog(entry({ cacheReadCostPerMillionTokens: null }), 1_000_000, 0, 400_000);
  assert.equal(c, 3.0);
});

test("formatCostUsd: soglie di precisione", () => {
  assert.equal(formatCostUsd(0), "$0.000");
  assert.equal(formatCostUsd(0.00005), "< $0.0001");
  assert.equal(formatCostUsd(1.23456), "$1.235");
});
