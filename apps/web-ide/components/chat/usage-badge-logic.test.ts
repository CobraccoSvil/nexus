// Test del badge usage (ADR 0037 / regola M). Runner: `node --test` con
// type-stripping nativo, import con estensione .ts esplicita.
//
// L'invariante che questi test proteggono: l'output NON dipende dalla SCALA dei
// numeri. Il badge etichettava i token confrontando grandezze
// (`total > lastIn + lastOut + 50`), quindi lo stesso run cambiava etichetta al
// variare della cache o della riconciliazione col ledger.

import { test } from "node:test";
import assert from "node:assert/strict";
import { usageBadgeView } from "./usage-badge-logic.ts";

test("etichette fisse: non dipendono dalla scala dei numeri", () => {
  // Caso PRE-riconciliazione (contatori dell'ultimo turno) e caso POST
  // (cumulativi del run): stessa forma di etichetta. Con la vecchia soglia il
  // primo diceva "token" e il secondo "token totali (ultima chiamata: ...)".
  const pre = usageBadgeView({ totalTokens: 20_665, promptTokens: 19_482, completionTokens: 1_183 });
  const post = usageBadgeView({ totalTokens: 618_984, promptTokens: 610_251, completionTokens: 8_733 });
  assert.equal(pre.tokensLabel, "20.665 token del run");
  assert.equal(post.tokensLabel, "618.984 token del run");
  // NB it-IT raggruppa da 5 cifre in su (convenzione CLDR "min2"): 1183 resta
  // "1183", 20665 diventa "20.665". Atteso: e' il formato italiano corretto.
  assert.equal(pre.breakdownLabel, "19.482 in / 1183 out");
  assert.equal(post.breakdownLabel, "610.251 in / 8733 out");
});

test("il caso che faceva scattare la soglia non produce piu' un'etichetta diversa", () => {
  // total > in+out+50 accadeva per i token di CACHE, non per la cumulativita':
  // la soglia misurava la cosa sbagliata. Qui total supera in+out di 256.
  const v = usageBadgeView({ totalTokens: 21_000, promptTokens: 19_482, completionTokens: 1_262 });
  assert.equal(v.tokensLabel, "21.000 token del run");
  assert.ok(!v.tokensLabel?.includes("totali"), "nessuna etichetta condizionale");
  assert.ok(!v.breakdownLabel?.includes("ultima chiamata"), "nessun qualificatore dedotto");
});

test("il modello e' etichettato come FINALE, non come il modello del run", () => {
  // Misurato: run da 618.984 token attribuito a google, ma il 65% dei token e'
  // di mistral (cascade). Il campo porta l'ultima iterazione: dirlo.
  const v = usageBadgeView({ provider: "google", model: "gemini-3.5-flash", totalTokens: 618_984 });
  assert.equal(v.modelLabel, "modello finale: google/gemini-3.5-flash");
});

test("campi assenti: la parte corrispondente sparisce, niente zeri finti", () => {
  const vuoto = usageBadgeView({});
  assert.equal(vuoto.tokensLabel, undefined);
  assert.equal(vuoto.breakdownLabel, undefined);
  assert.equal(vuoto.costLabel, undefined);
  assert.equal(vuoto.modelLabel, undefined);
  // provider senza model -> nessuna attribuzione parziale
  assert.equal(usageBadgeView({ provider: "mistral" }).modelLabel, undefined);
});

test("costo: mostrato solo se > 0, con la currency dichiarata", () => {
  assert.equal(usageBadgeView({ totalCost: 0.0853 }).costLabel, "$0.0853 USD");
  assert.equal(usageBadgeView({ totalCost: 0.5429, currency: "EUR" }).costLabel, "$0.5429 EUR");
  // Costo 0 (prezzo del modello ignoto): non si stampa un "$0.0000" che
  // sembrerebbe "gratis".
  assert.equal(usageBadgeView({ totalCost: 0 }).costLabel, undefined);
});

test("solo output: breakdown presente anche senza token di input", () => {
  const v = usageBadgeView({ totalTokens: 1_183, completionTokens: 1_183 });
  assert.equal(v.breakdownLabel, "0 in / 1183 out");
});
