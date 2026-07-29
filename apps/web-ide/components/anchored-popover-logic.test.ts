import { test } from "node:test";
import assert from "node:assert/strict";
import { posizionePopoverAncorato } from "./anchored-popover-logic.ts";

/** La campanella del centro notifiche come sta nella barra di stato del run:
 *  28x28, in basso a destra del pannello chat. Misure prese dal caso reale. */
function campanella(top: number, right = 612) {
  return { left: right - 28, right, top, bottom: top + 28 };
}
const SCHERMO = { width: 875, height: 683 };
const PANNELLO = { width: 300, maxHeight: 380 };

test("il popover non sfora mai lo spazio disponibile nel verso scelto", () => {
  // Il difetto originale: il pannello dichiarava 380px di altezza dentro una
  // barra alta 48px con overflow:hidden, e si vedeva per 48px. Qualunque
  // altezza restituita maggiore dello spazio libero rifarebbe lo stesso danno,
  // solo contro il bordo del viewport invece che contro quello della barra.
  for (const top of [10, 60, 200, 379, 500, 660]) {
    const p = posizionePopoverAncorato(campanella(top), SCHERMO, PANNELLO);
    const spazio =
      p.verso === "alto" ? campanella(top).top : SCHERMO.height - campanella(top).bottom;
    assert.ok(
      p.maxHeight <= spazio,
      `top=${top} verso=${p.verso}: maxHeight ${p.maxHeight} eccede lo spazio ${spazio}`,
    );
    assert.ok(p.maxHeight >= 0, `top=${top}: altezza negativa`);
    assert.ok(p.top >= 0, `top=${top}: il popover inizia sopra il viewport`);
    assert.ok(
      p.top + p.maxHeight <= SCHERMO.height,
      `top=${top}: il popover finisce sotto il viewport`,
    );
  }
});

test("il verso lo decide lo spazio, non una preferenza fissa", () => {
  // In basso (il caso reale: la barra sta sopra il composer) si apre verso
  // l'alto, dove c'e' tutto il nastro.
  const inBasso = posizionePopoverAncorato(campanella(600), SCHERMO, PANNELLO);
  assert.equal(inBasso.verso, "alto");
  // Ancorato in cima non c'e' spazio sopra: aprirsi verso l'alto darebbe un
  // pannello schiacciato contro il bordo.
  const inAlto = posizionePopoverAncorato(campanella(12), SCHERMO, PANNELLO);
  assert.equal(inAlto.verso, "basso");
});

test("il popover resta dentro i bordi laterali", () => {
  // Allineato al bordo destro della campanella quando c'e' spazio.
  const normale = posizionePopoverAncorato(campanella(600), SCHERMO, PANNELLO);
  assert.equal(normale.left, 612 - 300);
  // Campanella vicino al bordo sinistro: allineare a destra darebbe un left
  // negativo, cioe' meta' pannello fuori schermo.
  const aSinistra = posizionePopoverAncorato(campanella(600, 120), SCHERMO, PANNELLO);
  assert.ok(aSinistra.left >= 0, "il popover esce dal bordo sinistro");
  // Viewport piu' stretto del pannello (finestra molto ridotta): vince il bordo
  // sinistro, cosi' il testo resta leggibile da capo invece che troncato.
  const stretto = posizionePopoverAncorato(campanella(600, 200), { width: 260, height: 683 }, PANNELLO);
  assert.ok(stretto.left >= 0, "su schermo stretto il popover parte fuori");
});

test("una campanella a ridosso del bordo non produce altezze assurde", () => {
  // Senza il taglio a zero, lo spazio negativo diventerebbe un maxHeight
  // negativo: il pannello sparirebbe invece di ridursi.
  const p = posizionePopoverAncorato(
    { left: 584, right: 612, top: 2, bottom: 30 },
    { width: 875, height: 40 },
    PANNELLO,
  );
  assert.ok(p.maxHeight >= 0);
  assert.ok(Number.isFinite(p.top));
});
