// Unit test della regola di lazy-fetch degli step storici (ADR 0037).
// Testiamo la funzione PURA shouldFetchRunSteps (l'hook useResolvedRunSteps
// richiederebbe un renderer React, non disponibile nel repo: la logica
// decisionale e' estratta e testata qui). Runner: node --test.

import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldFetchRunSteps } from "./use-run-steps-logic.ts";

test("fetch quando: step vuoti + abilitato + runId presente", () => {
  assert.equal(shouldFetchRunSteps(false, true, "run-1"), true);
});

test("niente fetch quando gli step sono gia' presenti (agentStepsMap popolato)", () => {
  // Caso turno LIVE (SSE) o storico gia' in cache: mai fetch.
  assert.equal(shouldFetchRunSteps(true, true, "run-1"), false);
});

test("niente fetch quando disabilitato (riga storica collassata)", () => {
  assert.equal(shouldFetchRunSteps(false, false, "run-1"), false);
});

test("niente fetch senza runId", () => {
  assert.equal(shouldFetchRunSteps(false, true, undefined), false);
  assert.equal(shouldFetchRunSteps(false, true, ""), false);
});
