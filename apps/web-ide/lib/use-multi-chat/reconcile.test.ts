// Unit test del punto unico di riconciliazione tab chat (useMultiChat).
// Runner: `node --test` con type-stripping nativo (Node >= 22.18 / 24). Import
// con estensione .ts esplicita, richiesta dal loader ESM di Node.
//
// Regressione coperta (incidente 2026-07-20, cambio progetto nel web-ide):
// al passaggio di progetto le tab del progetto precedente non devono mai
// sopravvivere alla riconciliazione — ne' come tab aperte ne' come attiva.

import { test } from "node:test";
import assert from "node:assert/strict";
import { reconcileSessionTabs } from "./reconcile.ts";

test("tab persistite di un altro progetto vengono scartate, fallback prima sessione", () => {
  // localStorage del progetto nuovo vuoto o stantio: gli id del progetto
  // precedente non esistono tra le sessioni correnti.
  const { tabs, active } = reconcileSessionTabs(
    ["nuova-1", "nuova-2"],
    ["vecchia-1", "vecchia-2"],
    "vecchia-1",
  );
  assert.deepEqual(tabs, ["nuova-1"]);
  assert.equal(active, "nuova-1");
});

test("persistenza valida: tab e attiva sopravvivono", () => {
  const { tabs, active } = reconcileSessionTabs(
    ["s1", "s2", "s3"],
    ["s2", "s3"],
    "s2",
  );
  assert.deepEqual(tabs, ["s2", "s3"]);
  assert.equal(active, "s2");
});

test("attiva stantia con tab valide: si attiva l'ultima tab aperta", () => {
  const { tabs, active } = reconcileSessionTabs(
    ["s1", "s2"],
    ["s1", "s2"],
    "cancellata",
  );
  assert.deepEqual(tabs, ["s1", "s2"]);
  assert.equal(active, "s2");
});

test("attiva che punta a una sessione esistente ma non aperta come tab: non viene attivata", () => {
  // L'attiva deve essere sempre una tab aperta: attivare una sessione fuori
  // dalle tab renderebbe il pannello e la barra tab incoerenti.
  const { tabs, active } = reconcileSessionTabs(
    ["s1", "s2", "s3"],
    ["s1"],
    "s3",
  );
  assert.deepEqual(tabs, ["s1"]);
  assert.equal(active, "s1");
});

test("progetto senza sessioni: nessuna tab, nessuna attiva", () => {
  const { tabs, active } = reconcileSessionTabs([], ["vecchia-1"], "vecchia-1");
  assert.deepEqual(tabs, []);
  assert.equal(active, null);
});

test("nessuna persistenza: si apre la prima sessione", () => {
  const { tabs, active } = reconcileSessionTabs(["s1", "s2"], [], null);
  assert.deepEqual(tabs, ["s1"]);
  assert.equal(active, "s1");
});
