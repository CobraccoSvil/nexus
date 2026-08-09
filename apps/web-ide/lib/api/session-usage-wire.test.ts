// Il confine wire Rust -> TS di `GET /api/billing/session-usage`.
//
// Runner: `node --test` con type-stripping nativo, import con estensione .ts.
//
// PERCHE' QUESTO TEST ESISTE (regola O). Il difetto gemello e' gia' accaduto su
// questo stesso confine: `provider-badge.tsx` dichiarava un tipo LOCALE in
// snake_case mentre il wire di `/api/models` era camelCase, ogni lettura era
// `undefined`, e un `?? 0` a valle la trasformava in «costo zero». Il footer
// mostro' `$0.00` per mesi con i test di ENTRAMBI i lati verdi: ciascuno
// misurava la propria idea del contratto, nessuno la giunzione.
//
// Qui la fixture e' UNA SOLA e non e' scritta a mano: e' il JSON che il test
// Rust asserisce essere il prodotto di `corpo_session_usage`
// (`crates/mcp-core/src/billing.rs`). Questo test lo dà in pasto all'adapter che
// la produzione usa davvero, con `fetch` sostituito. Rinominare un campo da un
// lato fa rosseggiare quel lato; aggiornare la fixture per placarlo fa
// rosseggiare l'altro.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  sessionUsageDalWire,
  urlSessionUsage,
  type SessionUsageWire,
} from "./session-usage-wire.ts";

const FIXTURE = join(import.meta.dirname, "__wire__", "session-usage.json");

/** Il corpo che il produttore Rust emette, letto dal file condiviso.
 *
 *  Il cast e' l'unico punto in cui il JSON diventa il tipo dichiarato, ed e'
 *  volutamente qui e non nel modulo: e' esattamente l'assunto che questi test
 *  verificano, cioe' che la forma sul disco combaci con quella dichiarata. */
function corpoDalWire(): SessionUsageWire {
  return JSON.parse(readFileSync(FIXTURE, "utf8")) as SessionUsageWire;
}

test("l'adapter legge il wire reale: nessun campo undefined, nessuno zero finto", () => {
  const usage = sessionUsageDalWire(corpoDalWire());

  // I due numeri del contatore. Con un nome di campo disallineato questi
  // sarebbero `undefined` — e senza `?? 0` a mascherarli, il test lo vede.
  assert.equal(usage.totalTokens, 27_813_580);
  assert.equal(usage.totalCostUsd, 2.6024);
  assert.equal(typeof usage.totalTokens, "number");
  assert.equal(typeof usage.totalCostUsd, "number");

  // La ripartizione somma al totale che le sta sopra: stessa fonte, stesso
  // filtro. Se un giorno le due query divergessero, il totale non tornerebbe.
  const sommaToken = usage.breakdown.reduce((acc, b) => acc + b.tokens, 0);
  const sommaCosto = usage.breakdown.reduce((acc, b) => acc + b.costUsd, 0);
  assert.equal(sommaToken, usage.totalTokens, "la ripartizione non somma ai token totali");
  assert.ok(
    Math.abs(sommaCosto - usage.totalCostUsd) < 1e-9,
    `la ripartizione non somma al costo totale: ${sommaCosto} contro ${usage.totalCostUsd}`,
  );
  for (const voce of usage.breakdown) {
    assert.equal(typeof voce.costUsd, "number", `costo non letto per ${voce.model}`);
  }
});

test("il perimetro del run e' un insieme DIVERSO da quello della sessione", () => {
  const usage = sessionUsageDalWire(corpoDalWire());

  assert.ok(usage.currentRun, "il consumo del run deve essere letto dal wire");
  assert.equal(usage.currentRun!.totalTokens, 720_874);
  assert.equal(usage.currentRun!.totalCostUsd, 0.1272);
  assert.equal(usage.currentRun!.runCount, 1);

  // Il punto della misura dell'08/08/2026: i due perimetri differiscono di venti
  // volte sul costo e di due ordini di grandezza sui token. Mostrarne uno al
  // posto dell'altro non e' un'approssimazione.
  assert.notEqual(usage.currentRun!.totalCostUsd, usage.totalCostUsd);
  assert.ok(usage.currentRun!.totalTokens < usage.totalTokens);
});

test("il run viaggia come parametro, o il backend non puo' calcolarne il perimetro", () => {
  const conRun = urlSessionUsage(
    "",
    "http://localhost",
    "ec643216-d236-4a99-b47c-e6010ad6a809",
    "50187da9-a188-4861-a89a-67af5c1587b1",
  );
  assert.match(conRun, /session_id=ec643216-d236-4a99-b47c-e6010ad6a809/);
  assert.match(conRun, /run_id=50187da9-a188-4861-a89a-67af5c1587b1/);

  // Senza run non si manda un parametro vuoto: il backend distingue «non me lo
  // hai chiesto» da «me lo hai chiesto e non esiste».
  const senzaRun = urlSessionUsage("", "http://localhost", "ec643216-d236-4a99-b47c-e6010ad6a809");
  assert.ok(!senzaRun.includes("run_id"), `run_id non richiesto ma presente: ${senzaRun}`);
});

test("senza run richiesto il consumo di run e' ASSENTE, non zero", () => {
  const corpo = corpoDalWire();
  corpo.current_run = null;

  const usage = sessionUsageDalWire(corpo);
  assert.equal(usage.currentRun, null);
  // MUTAZIONE: far ritornare all'adapter `{ totalTokens: 0, totalCostUsd: 0 }`
  // al posto di `null` fa rosseggiare qui. Uno zero direbbe «questo run non ha
  // consumato nulla»: su un contatore di spesa e' l'affermazione piu'
  // rassicurante che si possa fare senza aver misurato niente.
  assert.notDeepEqual(usage.currentRun, { totalTokens: 0, totalCostUsd: 0 });
});

test("un backend che non parla ancora questo contratto non produce un run a zero", () => {
  const corpo = corpoDalWire();
  delete corpo.current_run;

  const usage = sessionUsageDalWire(corpo);
  assert.equal(usage.currentRun, null);
  // I due numeri principali restano leggibili: il campo nuovo e' additivo.
  assert.equal(usage.totalTokens, 27_813_580);
});

test("i nomi del wire sono quelli, non quelli che sembrano", () => {
  // Il difetto camelCase in forma esplicita: un corpo con i nomi «naturali» per
  // un lettore TS non produce numeri plausibili, produce `undefined`. Senza
  // `?? 0` a coprirlo, chi legge lo vede subito.
  const finto = {
    session_id: "x",
    totalTokens: 27_813_580,
    totalCostUsd: 2.6024,
    breakdown: [],
  } as unknown as SessionUsageWire;

  const usage = sessionUsageDalWire(finto);
  assert.equal(usage.totalTokens, undefined);
  assert.equal(usage.totalCostUsd, undefined);
  // MUTAZIONE: aggiungere `?? 0` in `sessionUsageDalWire` fa passare questi
  // campi a `0` e rosseggiare queste due asserzioni. E' il difetto gia' accaduto
  // sul footer costo-per-provider, rimasto invisibile per mesi proprio cosi'.
});
