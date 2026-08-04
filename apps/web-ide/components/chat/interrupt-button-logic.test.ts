// Unit test del pulsante di interruzione della barra attivita'.
// Runner: node --test. Import con estensione .ts esplicita (loader ESM Node).

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  activityStatusView,
  formatDuration,
  interruptButtonView,
  RUN_LONG_THRESHOLD_SECONDS,
} from "./interrupt-button-logic.ts";

// ── formatDuration ──────────────────────────────────────────────────────────

test("formatDuration: sotto il minuto in secondi, sopra in minuti+secondi", () => {
  assert.equal(formatDuration(0), "0s");
  assert.equal(formatDuration(59), "59s");
  assert.equal(formatDuration(60), "1m 0s");
  assert.equal(formatDuration(268), "4m 28s");
});

// ── etichetta ───────────────────────────────────────────────────────────────

test("l'etichetta non promette una cancellazione piu' forte", () => {
  const v = interruptButtonView({
    runElapsedSeconds: 268,
    secondsSinceLastStep: 268,
    isAgentStuck: true,
  });
  assert.equal(v.label, "Interrompi");
  // Il difetto originale, nella sua forma esatta: "Forza stop" faceva credere a
  // un'escalation che il backend non espone (una sola rotta /cancel).
  assert.ok(!/forza/i.test(v.label));
  assert.ok(/stessa azione del pulsante Stop/.test(v.title));
});

// ── il tooltip dichiara il MOTIVO vero, non quello che sembra ───────────────

test("run che lavora attivamente: il tooltip non dichiara inattivita'", () => {
  // Il caso che il nome della variabile a monte nascondeva: run avviato da 5
  // minuti che emette un passo ogni 3 secondi. Il pulsante compare comunque —
  // la soglia guarda la DURATA — e il tooltip deve dirlo senza suggerire che
  // l'agente sia fermo.
  const v = interruptButtonView({
    runElapsedSeconds: 300,
    secondsSinceLastStep: 3,
    isAgentStuck: false,
  });
  assert.ok(v.visible);
  assert.ok(v.title.includes("il run dura da 5m 0s"));
  assert.ok(v.title.includes("ultimo passo 3s fa"));
  assert.ok(!v.title.includes("nessun passo"));
});

test("run davvero fermo: il tooltip dichiara l'inattivita' col suo tempo", () => {
  const v = interruptButtonView({
    runElapsedSeconds: 400,
    secondsSinceLastStep: 268,
    isAgentStuck: true,
  });
  assert.ok(v.title.includes("il run dura da 6m 40s"));
  assert.ok(v.title.includes("nessun passo da 4m 28s"));
});

test("i due tempi restano distinti nel testo quando divergono", () => {
  // Un solo numero per due grandezze diverse e' la causa radice del malinteso:
  // se il tooltip ne stampasse uno solo, il test sopra passerebbe comunque.
  const v = interruptButtonView({
    runElapsedSeconds: 300,
    secondsSinceLastStep: 90,
    isAgentStuck: true,
  });
  assert.ok(v.title.includes("5m 0s"), "manca la durata del run");
  assert.ok(v.title.includes("1m 30s"), "manca l'inattivita'");
});

// ── visibilita' ─────────────────────────────────────────────────────────────

test("visibile solo oltre la soglia, che guarda la durata del run", () => {
  const sotto = interruptButtonView({
    runElapsedSeconds: RUN_LONG_THRESHOLD_SECONDS,
    secondsSinceLastStep: RUN_LONG_THRESHOLD_SECONDS,
    isAgentStuck: true,
  });
  assert.equal(sotto.visible, false);

  const sopra = interruptButtonView({
    runElapsedSeconds: RUN_LONG_THRESHOLD_SECONDS + 1,
    secondsSinceLastStep: 1,
    isAgentStuck: false,
  });
  assert.equal(sopra.visible, true);
});

// ── etichetta di stato: l'attesa vince sulla durata ─────────────────────────

test("run lungo E fermo: l'etichetta dichiara l'attesa, non l'elaborazione", () => {
  // Il caso soppresso dalla vecchia precedenza: run avviato da 8 minuti, fermo
  // da 4. La barra scriveva "AI in elaborazione" e chi leggeva aspettava.
  const s = activityStatusView({
    runElapsedSeconds: 480,
    secondsSinceLastStep: 268,
    isAgentStuck: true,
    busyLabel: "AI al lavoro",
  });
  assert.equal(s.text, "⚠ Agente in attesa da 4m 28s");
  assert.ok(!s.text.includes("elaborazione"));
  assert.ok(s.warn);
  // Il tooltip tiene comunque il secondo tempo: quale dei due si sta guardando
  // non deve essere una deduzione.
  assert.ok(s.title.includes("8m 0s"), "manca la durata del run nel tooltip");
});

test("run lungo ma attivo: nessun avviso, e resta scritto COSA sta facendo", () => {
  // Un run lungo che procede non e' un'anomalia. Il ramo che qui scriveva
  // "⚠ AI in elaborazione" in arancione doveva poi rassicurare nel tooltip
  // ("non e' fermo"), e per dirlo sacrificava l'unica informazione non
  // ricavabile dal pallino, dal cronometro e dal pulsante Interrompi: il
  // lavoro in corso.
  const s = activityStatusView({
    runElapsedSeconds: 300,
    secondsSinceLastStep: 4,
    isAgentStuck: false,
    busyLabel: "Subagente implement: al lavoro da 4m",
  });
  assert.equal(s.text, "Subagente implement: al lavoro da 4m");
  assert.ok(!s.warn, "un run lungo che procede non e' un avviso");
  assert.ok(!s.text.includes("⚠"));
  // I due tempi restano entrambi nel tooltip.
  assert.ok(s.title.includes("5m 0s"), "manca la durata del run");
  assert.ok(s.title.includes("4s"), "manca il tempo dall'ultimo passo");
});

test("run breve e fermo: l'attesa si vede anche sotto la soglia dei 2 minuti", () => {
  // La soglia dei 120s governa il pulsante, non l'etichetta: un agente fermo da
  // 70s su un run da 80s va detto subito.
  const s = activityStatusView({
    runElapsedSeconds: 80,
    secondsSinceLastStep: 70,
    isAgentStuck: true,
    busyLabel: "AI al lavoro",
  });
  assert.equal(s.text, "⚠ Agente in attesa da 1m 10s");
  assert.ok(s.warn);
});

test("run ordinario: etichetta di base, nessuna evidenza", () => {
  const s = activityStatusView({
    runElapsedSeconds: 12,
    secondsSinceLastStep: 2,
    isAgentStuck: false,
    busyLabel: "AI al lavoro",
  });
  assert.equal(s.text, "AI al lavoro");
  assert.equal(s.warn, false);
  assert.ok(!s.text.includes("⚠"));
});

// ── visibilita' del pulsante ────────────────────────────────────────────────

test("un'inattivita' lunga da sola NON fa comparire il pulsante", () => {
  // Documenta il comportamento reale (la soglia e' sulla durata): se un giorno
  // si vorra' agganciarla all'inattivita', questo test rosseggia e va aggiornato
  // insieme alla condizione, non di nascosto.
  const v = interruptButtonView({
    runElapsedSeconds: 30,
    secondsSinceLastStep: 30,
    isAgentStuck: true,
  });
  assert.equal(v.visible, false);
});
