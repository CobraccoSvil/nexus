import { strict as assert } from "node:assert";
import { test } from "node:test";
import { stiliBarraUtente, type StiliBarraUtente } from "./user-header-logic.ts";

const COLORI = {
  border: "#333",
  text: "#eee",
  textMuted: "#999",
  accentBg: "#1e3a5f",
};

const CONTROLLI: (keyof StiliBarraUtente)[] = ["admin", "ide", "uscita"];

// I tre controlli portano etichette di lunghezza diversa e, per l'uscita,
// tradotta: "Esci" / "Logout" / "Cerrar sesion". Una larghezza fissa taglia la
// piu' lunga -- e' il difetto misurato il 02/08/2026 su /admin, dove "Esci"
// usciva dal riquadro di 26px. Il test guarda gli stili che il componente
// applica davvero, non una loro copia: rimettere `width` in uno qualunque dei
// tre lo fa rosseggiare.
test("nessun controllo della barra fissa la larghezza", () => {
  const stili = stiliBarraUtente(COLORI);
  for (const nome of CONTROLLI) {
    assert.equal(
      stili[nome].width,
      undefined,
      `${nome}: la larghezza deve seguire l'etichetta, non un numero`,
    );
  }
});

test("il quadrato resta come minimo, per il bersaglio cliccabile", () => {
  const stili = stiliBarraUtente(COLORI);
  for (const nome of CONTROLLI) {
    assert.equal(stili[nome].minWidth, 26, `${nome}: minWidth`);
    assert.equal(stili[nome].height, 26, `${nome}: height`);
  }
});

test("l'etichetta ha respiro orizzontale e non va a capo", () => {
  const stili = stiliBarraUtente(COLORI);
  for (const nome of CONTROLLI) {
    assert.equal(stili[nome].padding, "0 8px", `${nome}: padding`);
    // Senza questo, un controllo stretto manda l'etichetta a capo dentro
    // un'altezza di 26px: il testo sparirebbe invece di essere tagliato.
    assert.equal(stili[nome].whiteSpace, "nowrap", `${nome}: whiteSpace`);
  }
});

test("il bordo non mangia lo spazio dell'etichetta", () => {
  const stili = stiliBarraUtente(COLORI);
  for (const nome of CONTROLLI) {
    assert.equal(stili[nome].boxSizing, "border-box", `${nome}: boxSizing`);
  }
});

test("le differenze fra i controlli restano quelle di colore e tipografia", () => {
  const stili = stiliBarraUtente(COLORI);
  assert.equal(stili.admin.background, COLORI.accentBg);
  assert.equal(stili.ide.border, `1px solid ${COLORI.border}`);
  assert.equal(stili.uscita.color, COLORI.textMuted);
  assert.equal(stili.uscita.cursor, "pointer");
});
