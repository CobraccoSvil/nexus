// Test del contatore sotto la chat. Runner: `node --test` con type-stripping
// nativo, import con estensione .ts esplicita.
//
// L'invariante che questi test proteggono: i due numeri mostrati vengono SEMPRE
// dallo stesso perimetro, e cio' che non e' stato misurato non prende l'aspetto
// di una misura.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  usageBarView,
  rapportoBarra,
  costoLeggibile,
  NON_MISURATO,
  type SessionUsageState,
} from "./token-usage-bar-logic.ts";

/** I numeri veri della sessione `ec643216` di gestione-corsi, 08/08/2026. */
const MISURATO: SessionUsageState = {
  stato: "noto",
  sessione: { totalTokens: 27_813_580, totalCostUsd: 2.6024 },
  run: {
    runId: "50187da9-a188-4861-a89a-67af5c1587b1",
    totalTokens: 720_874,
    totalCostUsd: 0.1272,
    runCount: 1,
  },
};

test("i due numeri vengono dallo stesso perimetro, e il tooltip lo dichiara", () => {
  const v = usageBarView(MISURATO);
  assert.equal(v.visibile, true);
  assert.equal(v.misurato, true);
  assert.equal(v.tokensLabel, "27.8M token");
  assert.equal(v.costLabel, "$2.60");
  // Il perimetro non e' lasciato all'intuito: e' il difetto misurato l'08/08,
  // dove nessuna etichetta diceva a quale insieme si riferisse ciascun numero.
  assert.match(v.titolo, /TUTTA la conversazione/);
  assert.match(v.titolo, /sub-run inclusi/);
});

test("il consumo del run e' una riga DISTINTA, non il numero principale", () => {
  const v = usageBarView(MISURATO);
  // Il principale resta la sessione...
  assert.equal(v.costLabel, "$2.60");
  // ...e il run vive in una riga sua, col numero di run che compongono il
  // perimetro.
  assert.equal(v.runLabel, "720.874 token - $0.13 (1 run)");
  // MUTAZIONE: far scrivere a `usageBarView` il costo del run nel `costLabel`
  // fa rosseggiare la prima asserzione — ed e' esattamente la confusione fra
  // perimetri da cui questo modulo nasce ($2,6024 contro $0,1272: venti volte).
});

test("piu' run nel perimetro: il conteggio si legge", () => {
  const v = usageBarView({
    ...MISURATO,
    run: { ...MISURATO.run!, runCount: 4 },
  } as SessionUsageState);
  assert.match(v.runLabel!, /\(4 run\)/);
});

test("senza perimetro di run non si inventa una riga", () => {
  const v = usageBarView({ stato: "noto", sessione: MISURATO.sessione, run: null });
  assert.equal(v.runLabel, undefined);
});

test("lettura fallita: il contatore resta VISIBILE e dichiara di non sapere", () => {
  const v = usageBarView({ stato: "non_disponibile", motivo: "API error 500" });
  // Visibile di proposito: sparire lo renderebbe indistinguibile da una chat
  // che non ha ancora speso nulla.
  assert.equal(v.visibile, true);
  assert.equal(v.misurato, false);
  assert.equal(v.tokensLabel, NON_MISURATO);
  assert.equal(v.costLabel, NON_MISURATO);
  assert.match(v.titolo, /non leggibile/);
  // MUTAZIONE: far ricadere questo ramo sull'ultimo valore noto fa rosseggiare
  // le due asserzioni sui marcatori. Era il comportamento precedente, e il modo
  // in cui il costo di un altro insieme e' rimasto in video per un intero run.
  assert.notEqual(v.tokensLabel, "27.8M token");
});

test("prima della lettura il contatore non mostra zeri", () => {
  const v = usageBarView({ stato: "in_attesa" });
  assert.equal(v.visibile, false);
  assert.equal(v.misurato, false);
  // "0 token - $0.00" sarebbe un'affermazione: che non e' stato speso nulla.
  assert.notEqual(v.tokensLabel, "0 token");
  assert.notEqual(v.costLabel, "$0.00");
});

test("sessione senza consumo: nessuna barra, ma e' una MISURA", () => {
  const v = usageBarView({
    stato: "noto",
    sessione: { totalTokens: 0, totalCostUsd: 0 },
    run: null,
  });
  assert.equal(v.visibile, false);
  assert.equal(v.misurato, true, "zero letto dal ledger e' un dato, non un'assenza");
});

test("il costo si legge a qualunque scala", () => {
  assert.equal(costoLeggibile(0), "$0.00");
  assert.equal(costoLeggibile(2.6024), "$2.60");
  assert.equal(costoLeggibile(0.1272), "$0.13");
  // Sotto il centesimo due decimali direbbero "$0.00" su una spesa reale: e'
  // il caso dei sub-run brevi, che sono tanti e in somma pesano.
  assert.equal(costoLeggibile(0.0001), "$0.0001");
  assert.notEqual(costoLeggibile(0.004), "$0.00");
});

test("un dato non misurato non produce una percentuale", () => {
  // Una barra riempita al 40% su un numero che non c'e' afferma qualcosa.
  assert.equal(
    rapportoBarra({ misurato: false, totalCostUsd: 2, budgetUsd: 5 }),
    null,
  );
  assert.deepEqual(rapportoBarra({ misurato: true, totalCostUsd: 2, budgetUsd: 5 }), {
    valore: 0.4,
    base: "budget",
  });
});

test("senza budget il rapporto e' il riempimento del contesto, che e' un altro dato", () => {
  // `lastInputTokens` e' il prompt dell'ULTIMA iterazione: non ha niente a che
  // vedere col cumulativo di spesa, e infatti ha un campo suo.
  const r = rapportoBarra({
    misurato: true,
    totalCostUsd: 2.6024,
    contextWindow: 128_000,
    lastInputTokens: 96_000,
  });
  assert.deepEqual(r, { valore: 0.75, base: "ctx" });
  // Niente contesto e niente budget: nessun rapporto da mostrare.
  assert.equal(rapportoBarra({ misurato: true, totalCostUsd: 2.6024 }), null);
});
