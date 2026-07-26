import { strict as assert } from "node:assert";
import { test } from "node:test";
import { etichettaVisibile, flatten } from "./auto-width-select-logic.ts";

const OPZIONI = [
  { value: "auto", label: "Auto" },
  { value: "continuous", label: "continuo" },
];

test("misura l'etichetta selezionata, non la piu' lunga della lista", () => {
  assert.equal(etichettaVisibile(OPZIONI, "continuous"), "continuo");
});

test("valore orfano: misura la PRIMA opzione, che e' quella che il select mostra", () => {
  // Se questo tornasse il valore grezzo, la pillola sarebbe larga quanto una
  // stringa che nessuno vede sullo schermo.
  assert.equal(etichettaVisibile(OPZIONI, "provider-rimosso-dal-catalogo"), "Auto");
});

test("valore assente (select non controllato senza defaultValue): prima opzione", () => {
  assert.equal(etichettaVisibile(OPZIONI, undefined), "Auto");
});

test("lista vuota: stringa vuota, niente eccezioni", () => {
  assert.equal(etichettaVisibile([], "qualsiasi"), "");
});

test("cerca anche dentro i gruppi, non solo fra le opzioni di primo livello", () => {
  const conGruppi = [
    { value: "auto", label: "Auto" },
    { label: "I miei profili", options: [{ value: "p1", label: "Assistente C#" }] },
    { label: "Profili di sistema", options: [{ value: "s1", label: "Revisore" }] },
  ];
  assert.equal(etichettaVisibile(conGruppi, "p1"), "Assistente C#");
  assert.equal(etichettaVisibile(conGruppi, "s1"), "Revisore");
});

test("flatten conserva l'ordine di dichiarazione fra opzioni sciolte e gruppi", () => {
  const conGruppi = [
    { value: "a", label: "A" },
    { label: "G", options: [{ value: "b", label: "B" }, { value: "c", label: "C" }] },
    { value: "d", label: "D" },
  ];
  assert.deepEqual(
    flatten(conGruppi).map((o) => o.value),
    ["a", "b", "c", "d"],
  );
});
