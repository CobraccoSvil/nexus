// Test del punto unico che decide quale elemento porta il badge di stato di
// un turno storico (RunStatusBadge persistente vs riga ActivityHistoryRow).
// Runner: `node --test` con type-stripping nativo, import con estensione .ts
// esplicita (vedi memoria webide-node-test-import-estensione).
//
// L'invariante che questi test proteggono: un run CHIUSO seguito
// immediatamente da un run nuovo nella stessa chat non deve MAI restare senza
// indicatore di stato. Misurato 31/07/2026, progetto bacheca-attivita, run
// 51fe77ce (failed_diagnosed): col vecchio codice, il turno chiuso perdeva
// contemporaneamente RunStatusBadge (soppresso perche' non piu' l'ultimo
// turno) E la riga storica (il gate `hasRunData` la azzerava quando il client
// non aveva piu' in memoria meta-step/step/trace per quel runId) -- nessuna
// delle due sorgenti rendeva lo stato.

import { test } from "node:test";
import assert from "node:assert/strict";
import { runStatusBadgeSource } from "./run-status-display-logic.ts";

test("nastro disattivato: sempre il badge persistente, in ogni posizione", () => {
  assert.equal(
    runStatusBadgeSource({ activityStreamEnabled: false, hasRunId: true, isLastAssistantRun: true }),
    "persistent-badge",
  );
  assert.equal(
    runStatusBadgeSource({ activityStreamEnabled: false, hasRunId: true, isLastAssistantRun: false }),
    "persistent-badge",
  );
});

test("nessun runId: sempre il badge persistente (nessuna riga storica possibile)", () => {
  assert.equal(
    runStatusBadgeSource({ activityStreamEnabled: true, hasRunId: false, isLastAssistantRun: false }),
    "persistent-badge",
  );
});

test("nastro attivo, ultimo turno: badge persistente (accanto al nastro espanso)", () => {
  assert.equal(
    runStatusBadgeSource({ activityStreamEnabled: true, hasRunId: true, isLastAssistantRun: true }),
    "persistent-badge",
  );
});

test("nastro attivo, turno storico (non ultimo): riga storica compatta", () => {
  assert.equal(
    runStatusBadgeSource({ activityStreamEnabled: true, hasRunId: true, isLastAssistantRun: false }),
    "history-row",
  );
});

test("il caso misurato: run chiuso + run successivo immediato non lascia MAI senza sorgente", () => {
  // Appena il run B parte e completa, il run A (ora storico) smette di
  // essere l'ultimo turno: la decisione per A deve passare a "history-row",
  // MAI restare "persistent-badge" con un badge poi soppresso altrove e MAI
  // degenerare in "nessuna sorgente" (per costruzione la funzione ritorna
  // sempre uno dei due valori tipizzati, mai un terzo stato "assente").
  const perRunA = runStatusBadgeSource({
    activityStreamEnabled: true,
    hasRunId: true,
    isLastAssistantRun: false, // A non e' piu' l'ultimo, ora lo e' B
  });
  assert.equal(perRunA, "history-row");
  // ActivityHistoryRow rende SEMPRE badge/trail/costo dai campi persistiti
  // del messaggio (runStatus/totalTokens/totalCost), indipendentemente da
  // quanti dati di nastro dettagliato il client ha ancora in memoria per quel
  // runId -- quindi "history-row" garantisce la visibilita' dello stato anche
  // quando hasRunData e' false (vedi ActivityHistoryRow, che non gate-a la
  // propria intestazione su questo).
});
