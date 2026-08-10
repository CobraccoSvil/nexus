import { test } from "node:test";
import assert from "node:assert/strict";
import { renderSpendCap, type SpendCap } from "./provider-spend-cap.ts";

/**
 * Il caso reale misurato il 10/08/2026: openrouter e kimi spendono e nessun
 * presidio li ferma. Devono essere VISIBILI e richiedere un intervento.
 *
 * MUTAZIONE che lo fa rosseggiare: mettere `requiresAction: false` su
 * `uncapped_spending`, oppure farlo ricadere nel `default` che ritorna `null`
 * (cioe' tornare a non mostrare nulla, che e' il difetto di partenza).
 */
test("un fornitore che spende senza tetto chiede un intervento", () => {
  const reso = renderSpendCap("uncapped_spending");
  assert.ok(reso, "la riga non deve sparire: era il difetto");
  assert.equal(reso.requiresAction, true);
  assert.match(reso.label, /senza tetto/);
});

test("un fornitore senza tetto e senza spesa si mostra ma non allarma", () => {
  const reso = renderSpendCap("uncapped_idle");
  assert.ok(reso);
  assert.equal(reso.requiresAction, false);
});

/** Un tetto regolare non aggiunge righe: la barra lo dice gia'. */
test("un tetto regolare non produce etichetta", () => {
  assert.equal(renderSpendCap("capped"), null);
});

/**
 * Regola Q lato consumatore: l'ignoto non degrada. Un backend che non dichiara
 * il campo non prova che un tetto ci sia, e non accusa nessuno.
 */
test("l'ignoto non diventa ne' allarme ne' rassicurazione", () => {
  assert.equal(renderSpendCap(undefined), null);
  const reso = renderSpendCap("undetermined");
  assert.ok(reso);
  assert.equal(reso.requiresAction, false);
});

/** Ogni variante del wire ha una resa: una aggiunta lato Rust non resta muta. */
test("tutte le varianti dichiarate sono rese", () => {
  const varianti: SpendCap[] = ["capped", "uncapped_spending", "uncapped_idle", "undetermined"];
  for (const v of varianti) {
    assert.doesNotThrow(() => renderSpendCap(v), `variante ${v}`);
  }
});
