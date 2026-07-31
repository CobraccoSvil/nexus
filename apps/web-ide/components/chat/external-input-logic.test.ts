// Unit test del punto unico "prompt esterno vs bozza non inviata".
// Runner: node --test. Import con estensione .ts esplicita (loader ESM Node).
//
// I test chiamano la STESSA funzione a cui delega l'effect su externalInput di
// chat-panel: non ricostruiscono la condizione a mano. Se qualcuno rimettesse
// setInput(externalInput) secco nell'effect, questi test non lo vedrebbero dal
// componente, ma il guard sta nel fatto che il componente non ha piu' altra
// strada per calcolare il nuovo input.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planExternalInput,
  EXTERNAL_PROMPT_SEPARATOR,
} from "./external-input-logic.ts";

// ── il difetto osservato (31/07/2026): bozza sostituita in silenzio ─────────

test("prefill manuale con bozza presente: il testo utente SOPRAVVIVE", () => {
  // Riproduzione: 792 caratteri scritti, click su "Abilita Playwright",
  // il campo conteneva solo il prompt del pulsante. Ora la bozza resta in testa
  // e il prompt si accoda sotto il separatore.
  const draft = "messaggio scritto dall'utente e non ancora inviato";
  const prompt = "Risolvi il problema descritto sotto sul progetto attivo: ...";
  const plan = planExternalInput({
    currentDraft: draft,
    externalPrompt: prompt,
    autoSend: false,
  });
  assert.ok(plan.nextInput.includes(draft), "la bozza deve restare nel campo");
  assert.ok(plan.nextInput.includes(prompt), "il prompt del pulsante deve arrivare");
  assert.equal(plan.nextInput, `${draft}${EXTERNAL_PROMPT_SEPARATOR}${prompt}`);
  assert.equal(plan.draftToRestore, null);
});

test("composer vuoto: il prompt entra da solo (comportamento storico)", () => {
  const plan = planExternalInput({
    currentDraft: "",
    externalPrompt: "prompt del pannello",
    autoSend: false,
  });
  assert.equal(plan.nextInput, "prompt del pannello");
  assert.equal(plan.draftToRestore, null);
});

test("composer con soli spazi: trattato come vuoto", () => {
  const plan = planExternalInput({
    currentDraft: "   \n  ",
    externalPrompt: "prompt",
    autoSend: false,
  });
  assert.equal(plan.nextInput, "prompt");
});

// ── idempotenza: doppio click sullo stesso pulsante non duplica ─────────────

test("bozza identica al prompt (doppio click): nessuna duplicazione", () => {
  const prompt = "prompt del pannello";
  const plan = planExternalInput({
    currentDraft: prompt,
    externalPrompt: prompt,
    autoSend: false,
  });
  assert.equal(plan.nextInput, prompt);
});

test("secondo click dopo un accodamento: il campo resta invariato", () => {
  const draft = "bozza utente";
  const prompt = "prompt del pannello";
  const primoClick = planExternalInput({
    currentDraft: draft,
    externalPrompt: prompt,
    autoSend: false,
  });
  const secondoClick = planExternalInput({
    currentDraft: primoClick.nextInput,
    externalPrompt: prompt,
    autoSend: false,
  });
  assert.equal(secondoClick.nextInput, primoClick.nextInput);
});

// ── auto-send: handshake intatto, bozza messa da parte ──────────────────────

test("auto-send con bozza: il campo contiene ESATTAMENTE il prompt", () => {
  // L'invio automatico si arma solo quando input === autoSendPendingRef
  // (chat-panel): accodare qui romperebbe l'handshake e l'Auto Fix resterebbe
  // fermo. La bozza va in draftToRestore e torna dopo l'invio.
  const draft = "bozza utente da preservare";
  const prompt = "prompt auto fix";
  const plan = planExternalInput({
    currentDraft: draft,
    externalPrompt: prompt,
    autoSend: true,
  });
  assert.equal(plan.nextInput, prompt);
  assert.equal(plan.draftToRestore, draft);
});

test("auto-send senza bozza: niente da ripristinare", () => {
  const plan = planExternalInput({
    currentDraft: "",
    externalPrompt: "prompt auto fix",
    autoSend: true,
  });
  assert.equal(plan.nextInput, "prompt auto fix");
  assert.equal(plan.draftToRestore, null);
});

test("auto-send con bozza identica al prompt: nessun ripristino ridondante", () => {
  const prompt = "prompt auto fix";
  const plan = planExternalInput({
    currentDraft: prompt,
    externalPrompt: prompt,
    autoSend: true,
  });
  assert.equal(plan.nextInput, prompt);
  assert.equal(plan.draftToRestore, null);
});

// ── il separatore non introduce spazi sporchi ───────────────────────────────

test("bozza con newline in coda: un solo separatore, niente righe vuote extra", () => {
  const plan = planExternalInput({
    currentDraft: "bozza\n\n",
    externalPrompt: "prompt",
    autoSend: false,
  });
  assert.equal(plan.nextInput, `bozza${EXTERNAL_PROMPT_SEPARATOR}prompt`);
});
