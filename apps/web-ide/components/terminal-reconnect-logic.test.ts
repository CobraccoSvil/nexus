import { test } from "node:test";
import assert from "node:assert/strict";
import { inFaseRavvicinata, ritardoRiconnessioneMs } from "./terminal-reconnect-logic.ts";

test("la riconnessione non si arrende mai", () => {
  // Il difetto: dopo sei tentativi il pannello scriveva «Riapri la scheda per
  // riprovare» e non ci provava piu', nemmeno col backend tornato su. Il
  // riavvio del backend e' normale (tre volte il 29/07/2026 per altrettanti
  // deploy) e ogni volta lasciava un terminale morto.
  //
  // Nessun numero di tentativi puo' produrre un ritardo che significhi «mai
  // piu'»: ne' infinito, ne' NaN.
  for (const n of [0, 1, 5, 6, 7, 50, 1000, 100000]) {
    const d = ritardoRiconnessioneMs(n);
    assert.ok(Number.isFinite(d), `tentativo ${n}: ritardo non finito (${d})`);
    assert.ok(d > 0, `tentativo ${n}: ritardo nullo o negativo (${d})`);
    assert.ok(d <= 30000, `tentativo ${n}: ritardo oltre il tetto (${d})`);
  }
});

test("i primi tentativi sono ravvicinati, poi il passo si assesta", () => {
  // Ravvicinati per riprendersi subito da un'interruzione breve...
  assert.equal(ritardoRiconnessioneMs(0), 1200);
  assert.equal(ritardoRiconnessioneMs(1), 2400);
  assert.equal(ritardoRiconnessioneMs(2), 4800);
  // ...poi il tetto, e non si muove piu': a regime una richiesta ogni 30s.
  assert.equal(ritardoRiconnessioneMs(5), 30000);
  assert.equal(ritardoRiconnessioneMs(6), 30000);
  assert.equal(ritardoRiconnessioneMs(99), 30000);
  // Il ritardo non decresce MAI: un backoff che torna indietro produce raffiche.
  let prec = 0;
  for (let n = 0; n <= 12; n++) {
    const d = ritardoRiconnessioneMs(n);
    assert.ok(d >= prec, `tentativo ${n}: il ritardo e' diminuito (${prec} -> ${d})`);
    prec = d;
  }
});

test("la fase ravvicinata finisce quando il ritardo tocca il tetto", () => {
  // Serve solo a scegliere il messaggio: durante la fase ravvicinata il numero
  // del tentativo informa, a regime sarebbe rumore che scorre.
  assert.ok(inFaseRavvicinata(0));
  assert.ok(inFaseRavvicinata(3));
  assert.ok(!inFaseRavvicinata(5));
  assert.ok(!inFaseRavvicinata(200));
});

test("un conteggio malformato non rompe il ritmo", () => {
  // `retryCount` arriva da un contatore vivo: se mai diventasse negativo o non
  // numerico, il ritardo deve restare valido invece di propagare NaN in un
  // setTimeout (che equivarrebbe a un ritardo nullo, cioe' a una raffica).
  for (const n of [-1, -100, Number.NaN, Number.POSITIVE_INFINITY]) {
    const d = ritardoRiconnessioneMs(n as number);
    assert.ok(Number.isFinite(d) && d >= 1200 && d <= 30000, `input ${n} -> ${d}`);
  }
});
