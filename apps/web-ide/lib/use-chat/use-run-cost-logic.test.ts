// Unit test della ripartizione che il footer costo-per-provider mostra.
//
// PERCHE' PARTE DALLA FIXTURE (regola O). Le voci non si costruiscono a mano:
// nascono dal wire che il produttore Rust emette (`corpo_session_usage`,
// crates/mcp-core/src/billing.rs), letto dall'adapter di produzione
// (`sessionUsageDalWire`) e passato alle stesse funzioni che gira il footer.
// Fabbricare qui delle voci fisserebbe l'assunto da verificare: e' esattamente
// cosi' che il difetto del 10/08/2026 e' rimasto invisibile: il footer componeva
// la ripartizione dalle TRACCE, i test la componevano dalle stesse trace, ed
// entrambi ignoravano il ledger — che nelle stesse 12 ore diceva un'altra cosa.
//
// Runner: `node --test` con type-stripping nativo, import con estensione .ts.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { sessionUsageDalWire, type SessionUsageWire } from "../api/session-usage-wire.ts";
import { etichetteVociCosto } from "./activity-stream.ts";
import {
  perimetroGiaNoto,
  vistaCostoRun,
  vociCostoDalLedger,
  type RipartizioneRun,
} from "./use-run-cost-logic.ts";
import type { CurrentRunUsage } from "../../components/chat/token-usage-bar-logic.ts";

const FIXTURE = join(import.meta.dirname, "..", "api", "__wire__", "session-usage.json");

/** Il perimetro del run come arriva dal wire reale. */
function perimetroDalWire(): CurrentRunUsage {
  const corpo = JSON.parse(readFileSync(FIXTURE, "utf8")) as SessionUsageWire;
  const run = sessionUsageDalWire(corpo).currentRun;
  assert.ok(run, "la fixture deve portare il perimetro di un run");
  return run;
}

/** L'intera ripartizione di SESSIONE della stessa fixture: serve per il modello
 *  la cui etichetta contiene un secondo `/`, che nel run non compare. */
function ripartizioneDiSessione() {
  const corpo = JSON.parse(readFileSync(FIXTURE, "utf8")) as SessionUsageWire;
  return sessionUsageDalWire(corpo).breakdown;
}

test("le voci del footer sono quelle del ledger, e sommano al totale del run", () => {
  const vista = vistaCostoRun({ stato: "noto", perimetro: perimetroDalWire() });
  assert.equal(vista.modo, "voci");
  if (vista.modo !== "voci") return;

  // La proprieta' che il footer prometteva e non manteneva: l'elenco somma al
  // numero che gli sta accanto, perche' vengono dalla stessa lettura.
  const sommaToken = vista.voci.reduce((acc, v) => acc + v.tokens, 0);
  const sommaCosto = vista.voci.reduce((acc, v) => acc + v.costUsd, 0);
  assert.equal(sommaToken, vista.totalTokens);
  assert.ok(Math.abs(sommaCosto - vista.totalCostUsd) < 1e-9);
  assert.equal(vista.voci[0].provider, "mistral");
  assert.equal(vista.voci[0].model, "mistral-small-latest");
});

test("il provider e' il PRIMO segmento dell'etichetta, non l'ultimo", () => {
  // `groq/openai/gpt-oss-20b`: il ledger compone `provider || '/' || model` e il
  // modello contiene a sua volta un `/`. Tagliando sull'ultimo separatore la
  // voce finirebbe attribuita a `groq/openai`, con colore ed etichetta di un
  // provider che non esiste.
  //
  // MUTAZIONE: `lastIndexOf` al posto di `indexOf` in `vociCostoDalLedger` fa
  // rosseggiare entrambe le asserzioni qui sotto.
  const voci = vociCostoDalLedger(ripartizioneDiSessione());
  const groq = voci.find((v) => v.provider === "groq");
  assert.ok(groq, `nessuna voce groq: ${voci.map((v) => v.provider).join(", ")}`);
  assert.equal(groq.model, "openai/gpt-oss-20b");
});

test("due voci dello stesso provider non danno due etichette identiche", () => {
  // Il difetto del 29/07/2026: la barra mostrava "openrouter" due volte con
  // importi diversi ($2,9436 e $0,0011) e si leggeva come un doppio conteggio.
  // Il conto era giusto — `x-ai/grok-4.5` e `z-ai/glm-4.7-flash` — ma
  // l'etichetta mostrava meta' della chiave con cui le voci sono aggregate.
  //
  // Le voci passano dal produttore vero: e' il ledger a comporre queste
  // etichette, e il footer nomina cio' che riceve.
  const voci = vociCostoDalLedger([
    { model: "openrouter/x-ai/grok-4.5", tokens: 1_200, costUsd: 2.9436 },
    { model: "openrouter/z-ai/glm-4.7-flash", tokens: 50, costUsd: 0.0011 },
    { model: "google/gemini-3.1-flash-lite", tokens: 80, costUsd: 0.0004 },
  ]);
  const etichette = etichetteVociCosto(voci);
  assert.equal(new Set(etichette).size, etichette.length, "due voci, due etichette");
  // Il modello compare col solo nome: il prefisso dice chi lo ha fatto, che nel
  // provider e' gia' implicito.
  assert.ok(etichette.includes("openrouter grok-4.5"));
  assert.ok(etichette.includes("openrouter glm-4.7-flash"));
  // Il provider che compare una volta sola resta corto: la barra ha poco spazio
  // e li' il modello non distingue nulla.
  assert.ok(etichette.includes("google"));
});

test("un'etichetta senza separatore resta tutta provider", () => {
  // Non dovrebbe accadere (il ledger compone sempre `provider/model`), ma se
  // accade non si inventa un modello: senza nulla da distinguere, l'etichetta
  // non si allunga.
  const voci = vociCostoDalLedger([{ model: "mistral", tokens: 10, costUsd: 0 }]);
  assert.equal(voci[0].provider, "mistral");
  assert.equal(voci[0].model, "");
  assert.deepEqual(etichetteVociCosto(voci), ["mistral"]);
});

test("totale senza ripartizione non produce voci inventate", () => {
  // Il caso del backend che non parla ancora questo contratto: il totale e' una
  // misura e si mostra, la ripartizione non c'e' e si dichiara. Il ripiego sulle
  // trace rimetterebbe in video il difetto appena chiuso, e per giunta
  // indistinguibile dal caso buono (regola Q).
  const perimetro = { ...perimetroDalWire(), breakdown: [] };
  const vista = vistaCostoRun({ stato: "noto", perimetro });
  assert.equal(vista.modo, "solo_totale");
  if (vista.modo !== "solo_totale") return;
  assert.equal(vista.totalCostUsd, 0.1272);
});

test("nessuna riga di ledger non e' un errore di lettura", () => {
  // Un turno appena partito: il ledger non ha ancora righe finalizzate. E' una
  // MISURA («non ha speso»), e non va confusa con «non sono riuscito a
  // leggere», che ha la stessa faccia e un rimedio diverso.
  const perimetro: CurrentRunUsage = {
    runId: "r",
    runCount: 1,
    totalTokens: 0,
    totalCostUsd: 0,
    breakdown: [],
  };
  assert.equal(vistaCostoRun({ stato: "noto", perimetro }).modo, "nessun_consumo");

  const fallita: RipartizioneRun = { stato: "non_disponibile", motivo: "HTTP 503" };
  const vista = vistaCostoRun(fallita);
  assert.equal(vista.modo, "non_disponibile");
  if (vista.modo !== "non_disponibile") return;
  assert.equal(vista.motivo, "HTTP 503");
});

test("in lettura non e' zero: il footer non afferma nulla finche' non sa", () => {
  assert.equal(vistaCostoRun({ stato: "in_lettura" }).modo, "in_lettura");
});

test("il perimetro gia' noto vale solo per il SUO run", () => {
  // E' il criterio che decide se parte una richiesta. Se rispondesse si' per un
  // run qualunque, il footer di ogni turno storico mostrerebbe il costo del
  // turno IN CORSO — lo stesso genere di scambio di perimetri misurato
  // l'08/08/2026 sul contatore ($2,6024 contro $0,1272).
  const noto = perimetroDalWire();
  assert.equal(perimetroGiaNoto(noto, noto.runId), true);
  assert.equal(perimetroGiaNoto(noto, "50187da9-0000-0000-0000-000000000000"), false);
  assert.equal(perimetroGiaNoto(null, noto.runId), false);
  assert.equal(perimetroGiaNoto(undefined, noto.runId), false);
});
