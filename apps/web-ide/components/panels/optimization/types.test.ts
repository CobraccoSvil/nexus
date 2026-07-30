// Unit test di retryHintForCategory: il matching deve usare i valori di
// `category` che i detector emettono davvero (crates/mcp-quality/src/lib.rs),
// non un vocabolario "documentation"/"commenti" mai prodotto da nessun
// produttore. Runner: node --test.

import { test } from "node:test";
import assert from "node:assert/strict";
import { retryHintForCategory } from "./types.ts";

const DEFAULT_HINT =
  "Riprova: applica il fix specifico per il problema descritto sopra. Se il pattern segnalato non è effettivamente presente nel codice, segnalalo come falso positivo invece di modificare il file.";

test("retryHintForCategory: category 'docs' riceve il suggerimento doc-specific, non il default", () => {
  const hint = retryHintForCategory("docs", "Public function without documentation");
  assert.notEqual(hint, DEFAULT_HINT);
  assert.match(hint, /TSDoc|JSDoc|doc-comment/);
});

test("retryHintForCategory: category 'comments' riceve il suggerimento doc-specific, non il default", () => {
  const hint = retryHintForCategory("comments", "No comments in large file");
  assert.notEqual(hint, DEFAULT_HINT);
  assert.match(hint, /TSDoc|JSDoc|doc-comment/);
});

test("retryHintForCategory: il vocabolario legacy 'documentation'/'commenti' (mai emesso dai detector) non e' piu' agganciato — verifica che il ramo morto sia rimosso", () => {
  // Nessun detector emette questi valori: il comportamento per category
  // sconosciuta e' sempre il default generico, a prescindere dal titolo.
  assert.equal(retryHintForCategory("documentation", "qualcosa"), DEFAULT_HINT);
  assert.equal(retryHintForCategory("commenti", "qualcosa"), DEFAULT_HINT);
});
