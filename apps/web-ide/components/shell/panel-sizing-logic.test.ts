// Test di regressione per i vincoli orizzontali dello shell. Runner: node --test.
//
// Copre il tetto piu' generoso sotto la soglia narrow/mobile, la stabilita' dei
// vincoli su viewport ampi (comportamento invariato) e il collasso della colonna
// flessibile su finestre strette.
//
// I test compongono le STESSE funzioni che usa `ide-shell.tsx` per arrivare allo
// spazio disponibile (regola O): nessuno ricalcola a mano `viewport - chrome`,
// altrimenti misurerebbe la propria aritmetica invece della cascata vera.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  rightSidebarBounds,
  clampRightWidth,
  mainAreaAvailableWidth,
  chatHeadFitsInline,
  CHAT_HEAD_REENTRY_GUARD,
} from "./panel-sizing-logic.ts";

// Larghezze persistite misurate sullo stack dev (workspace layout di Beaty-Book).
const LEFT_WIDTH = 195;
const RIGHT_WIDTH = 280;

// Riproduce la cascata di `ide-shell.tsx`: dallo spazio disponibile ai due binari
// del grid `minmax(0, 1fr) ${effectiveRightWidth}px`.
function columns(viewportWidth: number, opts: { sidebar?: boolean; rightWidth?: number } = {}) {
  const sidebar = opts.sidebar ?? true;
  const available = mainAreaAvailableWidth(viewportWidth, LEFT_WIDTH, sidebar);
  const fixed = clampRightWidth(opts.rightWidth ?? RIGHT_WIDTH, viewportWidth, available);
  return { available, fixed, flexible: available - fixed };
}

test("viewport ampio (>=1280): percentuale 0.6 e cap 620 invariati", () => {
  const b = rightSidebarBounds(1600, mainAreaAvailableWidth(1600, LEFT_WIDTH, true));
  assert.equal(b.min, 280);
  // 1600 * 0.6 = 960 -> clampato al cap assoluto 620.
  assert.equal(b.max, 620);
});

test("viewport ampio esattamente al confine 1280: regime largo (0.6)", () => {
  const b = rightSidebarBounds(1280, mainAreaAvailableWidth(1280, LEFT_WIDTH, true));
  // 1280 non e' < 1280 -> non narrow.
  assert.equal(b.min, 280);
  assert.equal(b.max, 620);
});

test("viewport narrow (<1280): tetto piu' generoso (0.72, cap 760)", () => {
  // Sidebar nascosta: lo spazio e' ampio e il cap narrow resta il vincolo che morde.
  const b = rightSidebarBounds(1200, mainAreaAvailableWidth(1200, LEFT_WIDTH, false));
  assert.equal(b.min, 280);
  // 1200 * 0.72 = 864 -> clampato al cap narrow 760.
  assert.equal(b.max, 760);
});

test("viewport mobile (<980): min della colonna fissa scende a 240", () => {
  const b = rightSidebarBounds(900, mainAreaAvailableWidth(900, LEFT_WIDTH, false));
  assert.equal(b.min, 240);
  // Qui a mordere e' lo spazio: disponibile 854 meno il minimo della colonna
  // flessibile (340) = 514. Il tetto per percentuale (900 * 0.72 = 648) e' piu' largo.
  assert.equal(b.max, 514);
});

test("il tetto non puo' mangiarsi il minimo della colonna flessibile", () => {
  // 1024 con sidebar: disponibile 777. Il cap narrow direbbe 760 e lascerebbe 17px
  // alla chat -- bastava trascinare il divisorio per riprodurre il collasso.
  const available = mainAreaAvailableWidth(1024, LEFT_WIDTH, true);
  assert.equal(available, 777);
  assert.equal(rightSidebarBounds(1024, available).max, 777 - 340);
  // Anche chiedendo una larghezza assurda, alla colonna flessibile resta il minimo.
  assert.equal(columns(1024, { rightWidth: 9999 }).flexible, 340);
});

test("regressione: su finestre strette la colonna flessibile non collassa mai", () => {
  // Misurato sul DOM vivo prima del fix: 600 -> 79px, 480 -> -41, 375 -> -113.
  for (const vw of [1280, 1024, 834, 768, 700, 600, 520, 480, 375, 320]) {
    const { flexible } = columns(vw);
    assert.ok(flexible > 0, `viewport ${vw}: colonna flessibile ${flexible}px`);
  }
});

test("regressione: quando i pannelli sono due, entrambi reggono il loro contenuto", () => {
  // Il minimo della colonna flessibile e' 340 (misurato: sotto, i bottoni
  // dell'header della chat finiscono fuori dal bordo).
  for (const vw of [1280, 1024, 834]) {
    const available = mainAreaAvailableWidth(vw, LEFT_WIDTH, true);
    const { min } = rightSidebarBounds(vw, available);
    const { fixed, flexible } = columns(vw);
    assert.ok(fixed >= min, `viewport ${vw}: colonna fissa ${fixed}px < ${min}`);
    assert.ok(flexible >= 340, `viewport ${vw}: colonna flessibile ${flexible}px < 340`);
  }
});

test("viewport ampi: nessuna regressione dove lo spazio bastava gia'", () => {
  // La tabella del difetto riporta 1280 -> 728 e 1024 -> 497: invariati.
  assert.equal(columns(1280).flexible, 728);
  assert.equal(columns(1024).flexible, 497);
});

test("834: i due pannelli restano, ma la chat non scende piu' sotto il suo minimo", () => {
  // Prima del fix: chat 313 e colonna fissa 280 -- ma a 313 i bottoni "+ / rinomina
  // / elimina" dell'header erano gia' tagliati fuori dal bordo. Ora la colonna
  // fissa cede i 27px che mancavano invece di tenerseli.
  const c = columns(834);
  assert.equal(c.flexible, 340);
  assert.equal(c.fixed, 253);
});

test("768: niente piu' due pannelli entrambi inservibili", () => {
  // Prima del fix: chat 247 su un contenuto che ne chiedeva 275 (sfondava).
  // 527 di spazio non bastano a 240 + 340: resta un pannello solo.
  const c = columns(768);
  assert.equal(c.fixed, 0);
  assert.equal(c.flexible, 527);
});

test("soglia pannello singolo: il punto critico, misurato da entrambi i lati", () => {
  // Il regime mobile chiede 240 (colonna fissa) + 340 (flessibile) = 580px.
  // Sopra la soglia i pannelli restano DUE (non degradare troppo presto)...
  assert.deepEqual(rightSidebarBounds(900, 580), { min: 240, max: 240 });
  // ...sotto la soglia ne resta uno SOLO (non schiacciare l'altro a zero).
  assert.deepEqual(rightSidebarBounds(900, 579), { min: 0, max: 0 });
  // E il pannello superstite prende tutto lo spazio, non una briciola.
  assert.equal(clampRightWidth(280, 900, 579), 0);
});

test("pannello singolo: sotto la soglia la colonna flessibile prende tutto", () => {
  const c = columns(600);
  assert.equal(c.fixed, 0);
  assert.equal(c.flexible, c.available);
  // Prima del fix erano 79px.
  assert.equal(c.flexible, 359);
});

test("max non scende mai sotto il min, su tutto lo spettro", () => {
  for (let vw = 240; vw <= 2560; vw += 1) {
    for (const sidebar of [true, false]) {
      const b = rightSidebarBounds(vw, mainAreaAvailableWidth(vw, LEFT_WIDTH, sidebar));
      assert.ok(b.max >= b.min, `viewport ${vw} (sidebar ${sidebar}): max ${b.max} < min ${b.min}`);
    }
  }
});

test("clampRightWidth: default 500 resta valido su viewport tipici", () => {
  // Su viewport ampio 500 e' dentro [280, 620].
  assert.equal(clampRightWidth(500, 1600, mainAreaAvailableWidth(1600, LEFT_WIDTH, true)), 500);
  // Su viewport narrow 500 e' dentro [280, 760] e lo spazio (953) non morde.
  assert.equal(clampRightWidth(500, 1200, mainAreaAvailableWidth(1200, LEFT_WIDTH, true)), 500);
});

test("clampRightWidth: valori fuori range riportati ai bordi", () => {
  assert.equal(clampRightWidth(9999, 1600, mainAreaAvailableWidth(1600, LEFT_WIDTH, false)), 620);
  assert.equal(clampRightWidth(10, 1600, mainAreaAvailableWidth(1600, LEFT_WIDTH, false)), 280);
  assert.equal(clampRightWidth(9999, 1200, mainAreaAvailableWidth(1200, LEFT_WIDTH, false)), 760);
});

// chatHeadFitsInline: la REGOLA di confronto testata state-machine. La misura
// vera (row.scrollWidth vs host.clientWidth) e il rendering riga<->popover sono
// verificati nel browser, non qui: un test che asserisse una soglia senza provare
// il rendering non proverebbe niente (regola O). Qui si fissa solo la matematica
// dell'isteresi, cioe' cio' che il browser da solo non mostrerebbe a colpo
// d'occhio (l'oscillazione al confine).

test("chatHeadFitsInline: senza misura parte dalla riga (default ottimista)", () => {
  assert.equal(chatHeadFitsInline(0, 0, true), true);
  assert.equal(chatHeadFitsInline(300, 0, false), true);
  assert.equal(chatHeadFitsInline(0, 420, true), true);
});

test("chatHeadFitsInline: in riga si resta finche' non si sfonda", () => {
  // naturale <= disponibile: ci sta.
  assert.equal(chatHeadFitsInline(500, 420, true), true);
  assert.equal(chatHeadFitsInline(420, 420, true), true);
  // naturale > disponibile: collassa al popover.
  assert.equal(chatHeadFitsInline(400, 420, true), false);
});

test("chatHeadFitsInline: dal popover si torna in riga solo oltre la banda morta", () => {
  const natural = 420;
  // Appena sopra il naturale ma dentro la guard: resta raccolta (niente rientro).
  assert.equal(chatHeadFitsInline(natural + CHAT_HEAD_REENTRY_GUARD - 1, natural, false), false);
  // Oltre la guard: rientra in riga.
  assert.equal(chatHeadFitsInline(natural + CHAT_HEAD_REENTRY_GUARD, natural, false), true);
});

test("chatHeadFitsInline: la banda morta evita l'oscillazione al confine", () => {
  // A una larghezza appena sopra il naturale, lo stato e' STABILE in entrambi i
  // versi: chi e' in riga ci resta, chi e' raccolto NON rientra. Senza isteresi i
  // due si contraddirebbero e un pixel di ResizeObserver farebbe sfarfallare.
  const width = 425;
  const natural = 420;
  assert.equal(chatHeadFitsInline(width, natural, true), true); // resta in riga
  assert.equal(chatHeadFitsInline(width, natural, false), false); // resta raccolto
});
