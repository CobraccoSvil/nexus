import assert from "node:assert/strict";
import { test } from "node:test";

import { localeDaiTag } from "./locale-browser.ts";

const DISPONIBILI = ["en", "it", "es"];

// IL difetto: il provider partiva da "en" e non chiedeva mai al browser quale
// lingua l'utente dichiarasse. Chi non aveva mai aperto il selettore vedeva
// l'inglese, mentre il dizionario italiano era completo (391 chiavi).
// MUTAZIONE: far tornare la funzione sempre `null` -> il provider resta su "en"
// e i test qui sotto rosseggiano tutti tranne quelli sul caso negativo.

test("la lingua regionale trova il dizionario della sua lingua", () => {
  // `it-IT` e `it-CH` sono lo stesso dizionario: i dizionari sono per lingua,
  // non per regione, e un confronto esatto non avrebbe mai trovato nulla —
  // nessun browser dichiara `it` nudo.
  assert.equal(localeDaiTag(["it-IT"], DISPONIBILI), "it");
  assert.equal(localeDaiTag(["it-CH"], DISPONIBILI), "it");
  assert.equal(localeDaiTag(["ES-ar"], DISPONIBILI), "es");
});

test("l'ordine e' quello dell'utente, non il primo che capita", () => {
  // Il caso che un `languages[0]` sbaglierebbe: la lingua preferita non ha un
  // dizionario, la seconda si'. Arrendersi al primo darebbe il ripiego inglese
  // a chi ha dichiarato di volere l'italiano.
  assert.equal(localeDaiTag(["de-CH", "it-IT", "en-US"], DISPONIBILI), "it");
  assert.equal(localeDaiTag(["it-IT", "en-US"], DISPONIBILI), "it");
  assert.equal(localeDaiTag(["en-GB", "it-IT"], DISPONIBILI), "en");
});

test("nessun dizionario per cio' che l'utente chiede: si dichiara, non si inventa", () => {
  // `null` e non `"en"`: chi chiama deve poter distinguere «vuole il tedesco
  // che non abbiamo» da «vuole l'inglese». Oggi portano alla stessa schermata,
  // ma sono decisioni diverse — la prima e' un dizionario da aggiungere.
  assert.equal(localeDaiTag(["de-DE", "fr"], DISPONIBILI), null);
  assert.equal(localeDaiTag([], DISPONIBILI), null);
});

test("un tag malformato non fa cadere la scelta", () => {
  // `navigator.languages` viene dal browser e non e' sotto il nostro controllo:
  // una voce non-stringa o vuota deve essere saltata, non far fallire tutto.
  const sporchi = ["", null as unknown as string, 42 as unknown as string, "it"];
  assert.equal(localeDaiTag(sporchi, DISPONIBILI), "it");
});
