// Unit test della decisione presa su un 403 (node --test, type-stripping).
//
// La composizione e' quella REALE di `fetchJson` in _shared.ts —
// `autorizzaOblioProgetto(readRenderedError(payload)?.code)` — quindi il test
// attraversa entrambi i produttori (regola O) invece di leggere `user_code` per
// conto proprio.
//
// I payload sono nella forma che scrive `RenderedError::write_into`
// (crates/nexus-types/src/error_presentation.rs), con i codici del vocabolario
// chiuso `AccessDenial` emessi da `load_project_context`
// (crates/mcp-core/src/projects/mod.rs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { autorizzaOblioProgetto, progettoDallUrl } from "./project-access.ts";
import { readRenderedError } from "./error-render.ts";

/** La domanda come se la pone `fetchJson`: dal corpo grezzo alla conseguenza. */
function dimenticaIlProgetto(payload: unknown): boolean {
  return autorizzaOblioProgetto(readRenderedError(payload)?.code);
}

/** Il 403 di progetto sparito, come lo scrive `api_error_rendered`. */
const CORPO_PROGETTO_SPARITO = {
  error:
    "Il progetto non esiste piu' oppure non e' piu' fra quelli a cui hai accesso. | detail: Progetto non accessibile",
  user_message: "Il progetto non esiste piu' oppure non e' piu' fra quelli a cui hai accesso.",
  user_code: "project_not_accessible",
  user_detail: "Progetto non accessibile",
};

/** Il 403 di permesso negato: stesso status, conseguenza OPPOSTA — il progetto
 *  e' vivo e i riferimenti locali restano validi. */
const CORPO_PERMESSO_NEGATO = {
  error:
    "Non hai i permessi necessari per questa operazione sul progetto. | detail: Non hai permessi Git su questo progetto",
  user_message: "Non hai i permessi necessari per questa operazione sul progetto.",
  user_code: "project_permission_denied",
  user_detail: "Non hai permessi Git su questo progetto",
};

test("solo il codice canonico autorizza a dimenticare il progetto", () => {
  assert.equal(dimenticaIlProgetto(CORPO_PROGETTO_SPARITO), true);
  assert.equal(
    dimenticaIlProgetto(CORPO_PERMESSO_NEGATO),
    false,
    "un permesso negato non e' un progetto sparito",
  );
});

test("la decisione non passa dal testo del messaggio", () => {
  // La frase italiana storica nei soli campi di testo: e' il corpo che il
  // vecchio `includes("Progetto non accessibile")` trattava come autorizzazione.
  assert.equal(dimenticaIlProgetto({ error: "Progetto non accessibile" }), false);
  assert.equal(
    dimenticaIlProgetto({
      user_message: "Progetto non accessibile",
      user_code: "provider_rejected",
      user_detail: "",
    }),
    false,
    "la frase non deve poter scavalcare il codice",
  );
  // E al contrario: riformulare il messaggio non deve spegnere la pulizia.
  assert.equal(
    dimenticaIlProgetto({
      user_message: "Questo progetto non c'e' piu'.",
      user_code: "project_not_accessible",
      user_detail: "",
    }),
    true,
  );
});

test("fallback dichiarato: senza codice non si cancella niente", () => {
  // Endpoint non ancora migrato, o backend indietro durante un deploy.
  for (const corpo of [null, undefined, "", 42, {}, { error: "403 Forbidden" }]) {
    assert.equal(dimenticaIlProgetto(corpo), false, `corpo ${JSON.stringify(corpo)}`);
  }
  // Codice sconosciuto (versione futura del vocabolario): non e' un permesso a
  // cancellare.
  assert.equal(
    dimenticaIlProgetto({ user_message: "x", user_code: "project_archived" }),
    false,
  );
  // `readRenderedError` non restituisce resa senza `user_message`: nemmeno un
  // `user_code` giusto da solo autorizza, perche' un corpo cosi' non e' una resa.
  assert.equal(
    dimenticaIlProgetto({ user_message: "   ", user_code: "project_not_accessible" }),
    false,
  );
});

test("l'id da dimenticare viene dal path, non dalla query", () => {
  const uuid = "3f6c1d2e-9a4b-4c8d-8e7f-0a1b2c3d4e5f";
  assert.equal(progettoDallUrl(`/api/projects/${uuid}/files?path=x`), uuid);
  assert.equal(progettoDallUrl(`/api/projects/${uuid}`), uuid);
  assert.equal(progettoDallUrl("/api/chat/sessions"), null);
  assert.equal(
    progettoDallUrl(`/api/chat/sessions?project=${uuid}`),
    null,
    "il progetto rifiutato e' quello del path, non quello che la pagina guarda",
  );
});
