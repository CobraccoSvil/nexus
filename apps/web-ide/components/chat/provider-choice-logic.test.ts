// Unit test della scelta di provider del composer (preferenza vs pin).
// Runner: node --test. Import con estensione .ts esplicita (loader ESM Node).
//
// I test chiamano le STESSE funzioni che chiamano chat-panel (`doSend` ->
// providerChoiceForSend) e composer (tooltip, stile, badge): non ricostruiscono
// la condizione a mano. Se qualcuno rimettesse la deduzione "selezionato =
// forzato" in uno dei due, questi test la vedrebbero.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  forceButtonView,
  isProviderPinned,
  providerChoiceForSend,
  providerSelectTitle,
  PROVIDER_AUTO,
} from "./provider-choice-logic.ts";

// ── cosa viaggia sul wire ───────────────────────────────────────────────────

test("dropdown + Forza attivo: il pin viaggia dichiarato", () => {
  const wire = providerChoiceForSend({
    selectedProvider: "deepseek",
    forceProvider: true,
  });
  assert.equal(wire.providerOverride, "deepseek");
  assert.equal(wire.providerOverrideMode, "pinned");
});

test("dropdown senza Forza: preferenza, non pin", () => {
  // IL DIFETTO, nella sua forma esatta: il pulsante "Forza" non arrivava al
  // backend, quindi la sola selezione dal dropdown sarebbe diventata un vincolo
  // duro — mentre il tooltip del pulsante spento promette l'opposto.
  const wire = providerChoiceForSend({
    selectedProvider: "deepseek",
    forceProvider: false,
  });
  assert.equal(wire.providerOverride, "deepseek");
  assert.equal(
    wire.providerOverrideMode,
    "preferred",
    "col pulsante spento la richiesta deve conservare il fallback",
  );
});

test("Auto senza hint: nessun provider e nessun vincolo", () => {
  const wire = providerChoiceForSend({
    selectedProvider: PROVIDER_AUTO,
    forceProvider: false,
  });
  assert.equal(wire.providerOverride, undefined);
  assert.equal(wire.providerOverrideMode, "preferred");
});

test("hint esterno su Auto: preferenza, mai pin", () => {
  // L'hint (es. generazione documenti: serve un modello capace) e' una scelta
  // del sistema, non un ordine dell'utente: togliergli il fallback lo
  // renderebbe solo piu' fragile.
  const wire = providerChoiceForSend({
    selectedProvider: PROVIDER_AUTO,
    forceProvider: false,
    hintProvider: "google",
  });
  assert.equal(wire.providerOverride, "google");
  assert.equal(wire.providerOverrideMode, "preferred");
});

test("Forza rimasto attivo dopo il ritorno ad Auto non pinna l'hint", () => {
  // `forceProvider` e' uno stato locale che sopravvive al ritorno del dropdown
  // su "Auto": il pulsante sparisce dalla barra ma nessuno lo rimette a false.
  // Senza la congiunzione, un invio guidato da un hint erediterebbe un "Forza"
  // premuto prima per un ALTRO provider.
  const wire = providerChoiceForSend({
    selectedProvider: PROVIDER_AUTO,
    forceProvider: true,
    hintProvider: "google",
  });
  assert.equal(wire.providerOverride, "google");
  assert.equal(
    wire.providerOverrideMode,
    "preferred",
    "un pulsante che l'utente non vede piu' non puo' vincolare la richiesta",
  );
});

test("il dropdown vince sull'hint quando c'e' una scelta esplicita", () => {
  const wire = providerChoiceForSend({
    selectedProvider: "mistral",
    forceProvider: true,
    hintProvider: "google",
  });
  assert.equal(wire.providerOverride, "mistral");
  assert.equal(wire.providerOverrideMode, "pinned");
});

test("gli identificatori sul wire sono i due canonici e basta", () => {
  const valori = new Set<string>();
  for (const selectedProvider of [PROVIDER_AUTO, "deepseek"]) {
    for (const forceProvider of [true, false]) {
      valori.add(
        providerChoiceForSend({ selectedProvider, forceProvider }).providerOverrideMode,
      );
    }
  }
  assert.deepEqual([...valori].sort(), ["pinned", "preferred"]);
});

// ── i tooltip dicono il vero nei due stati ──────────────────────────────────

test("pulsante spento: il tooltip promette il fallback, e il wire lo mantiene", () => {
  const input = { selectedProvider: "deepseek", forceProvider: false };
  const vista = forceButtonView(input.selectedProvider, input.forceProvider, "study");
  // La frase promette che il routing puo' cambiare provider...
  assert.match(vista.title, /preferenza/i);
  assert.match(vista.title, /fallback attivo/i);
  assert.ok(!/nessun ripiego/i.test(vista.title));
  // ...e la richiesta che parte in quello stato la mantiene. E' l'accoppiamento
  // che mancava: la frase e il fatto erano in due posti che non si parlavano.
  assert.equal(providerChoiceForSend(input).providerOverrideMode, "preferred");
});

test("pulsante attivo in Studio: il tooltip dichiara il vincolo, e il wire lo porta", () => {
  const input = { selectedProvider: "deepseek", forceProvider: true };
  const vista = forceButtonView(input.selectedProvider, input.forceProvider, "study");
  assert.match(vista.title, /solo a deepseek/i);
  assert.ok(
    !/pu[o'] scegliere un altro provider/i.test(vista.title),
    "col pin non esiste alcun altro provider da scegliere",
  );
  assert.equal(providerChoiceForSend(input).providerOverrideMode, "pinned");
});

test("il tooltip del dropdown distingue i tre stati", () => {
  assert.match(providerSelectTitle(PROVIDER_AUTO, false, "study"), /Routing automatico/);

  const preferenza = providerSelectTitle("deepseek", false, "study");
  assert.match(preferenza, /Preferenza deepseek/);
  assert.match(preferenza, /fallback attivo/i);

  const pin = providerSelectTitle("deepseek", true, "study");
  assert.match(pin, /PINNATO/);
  assert.match(pin, /nessun ripiego/i);
  assert.ok(
    !/fallback attivo/i.test(pin),
    "col pin non c'e' fallback: dirlo sarebbe la vecchia frase falsa col segno invertito",
  );
});

// ── dove il pin NON arriva: la frase deve dirlo ─────────────────────────────
//
// Il vincolo duro viaggia fino al gateway solo sul turno singolo (`study`). In
// `confirm` — che e' il DEFAULT della UI — e in `automatic` l'handler devia su
// spawn_agent_run e passa il solo nome del provider: il pin muore li' e
// l'esecutore conserva il failover cross-provider. Promettere "va solo a X" in
// quelle modalita' sarebbe la stessa bugia da cui il pin e' nato, riscritta in
// una frase nuova.

for (const modo of ["confirm", "automatic"] as const) {
  test(`pulsante attivo in ${modo}: la frase dice che il pin vale solo in Studio`, () => {
    const vista = forceButtonView("deepseek", true, modo);
    assert.match(vista.title, /solo in modalita' Studio/i);
    assert.match(vista.title, /punto di partenza/i);
    assert.ok(
      !/va solo a deepseek/i.test(vista.title),
      `in ${modo} il run puo' cambiare fornitore: promettere il contrario e' falso`,
    );
    assert.equal(
      vista.label,
      "Forza",
      "senza spunta: il segno di conferma direbbe che il vincolo e' attivo",
    );

    const titolo = providerSelectTitle("deepseek", true, modo);
    assert.match(titolo, /solo in modalita' Studio/i);
    assert.ok(!/PINNATO/.test(titolo));
  });
}

test("in Studio il pulsante attivo mostra la spunta, altrove no", () => {
  assert.equal(forceButtonView("deepseek", true, "study").label, "Forza ✓");
  assert.equal(forceButtonView("deepseek", true, "confirm").label, "Forza");
  // Spento: nessuna spunta in nessuna modalita'.
  assert.equal(forceButtonView("deepseek", false, "study").label, "Forza");
});

// ── il predicato che governa colore e badge ─────────────────────────────────

test("isProviderPinned: solo scelta esplicita + pulsante attivo", () => {
  assert.equal(isProviderPinned("deepseek", true), true);
  assert.equal(isProviderPinned("deepseek", false), false);
  assert.equal(isProviderPinned(PROVIDER_AUTO, true), false);
  assert.equal(isProviderPinned(PROVIDER_AUTO, false), false);
});
