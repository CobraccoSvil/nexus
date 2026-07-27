// Unit test della lettura della resa (node --test, type-stripping).
//
// I payload qui sotto sono nella forma che scrive `RenderedError::write_into`
// (crates/nexus-types/src/error_presentation.rs), l'unico punto che compone le
// tre chiavi: `PipelineError::to_body` nel gateway e `api_error_rendered` in
// mcp-core delegano entrambi a quello.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readRenderedError, userMessage } from "./error-render.ts";

/** Corpo d'errore del gateway con la cascata provider esaurita: `error`/`code`/
 *  `details` storici PIU' le tre chiavi additive. */
const CORPO_GATEWAY = {
  error:
    'tutti i provider hanno fallito -> mistral (mistral HTTP 429: {"error":{"message":"Requests rate limit exceeded","code":"rate_limit_exceeded"}})',
  code: "PROVIDER_ERROR",
  details: { primary_cause: "transient", failures: [{ provider: "mistral", status: 429 }] },
  user_message:
    "mistral (mistral-small-latest) ha applicato un limite di frequenza: troppe richieste ravvicinate. Requests rate limit exceeded",
  user_code: "provider_rate_limited",
  user_detail:
    'tutti i provider hanno fallito -> mistral (mistral HTTP 429: {"error":{"code":"rate_limit_exceeded"}})',
};

test("legge le tre chiavi e non tocca i campi storici", () => {
  const r = readRenderedError(CORPO_GATEWAY);
  assert.ok(r);
  assert.equal(r.code, "provider_rate_limited");
  assert.ok(r.message.includes("mistral-small-latest"));
  assert.ok(!r.message.includes("{"), "il blob non deve entrare nella frase");
  assert.ok(r.detail.includes("429"), "il tecnico integrale resta raggiungibile");
});

test("nessuna resa trasportata: null, mai una frase inventata", () => {
  // Endpoint non ancora migrato: il chiamante deve accorgersene e ripiegare,
  // non ricevere una frase dedotta dal testo tecnico (regola M).
  assert.equal(readRenderedError({ error: "API error 500: Internal Server Error" }), null);
  assert.equal(readRenderedError({ user_message: "   " }), null);
  assert.equal(readRenderedError(null), null);
  assert.equal(readRenderedError("stringa"), null);
});

test("user_code assente ma frase presente: la frase vale, il codice degrada", () => {
  const r = readRenderedError({ user_message: "Il servizio non risponde." });
  assert.ok(r);
  assert.equal(r.code, "unspecified");
  assert.equal(r.detail, "");
});

test("userMessage legge il campo rendered, non il testo dell'errore", () => {
  // Il `message` di ApiError resta il formato storico e contiene il blob: e' il
  // motivo per cui la frase deve viaggiare in un CAMPO separato.
  const errore = Object.assign(
    new Error(`API error 500: Internal Server Error - ${CORPO_GATEWAY.error}`),
    { status: 500, rendered: readRenderedError(CORPO_GATEWAY) },
  );
  const frase = userMessage(errore, "Invio messaggio fallito.");
  assert.equal(frase, CORPO_GATEWAY.user_message);
  assert.ok(!frase.includes("API error 500"));
});

test("senza resa vince il fallback dell'azione, non il testo tecnico troncato", () => {
  // Il comportamento storico incollava in chat la prima riga del messaggio
  // tecnico tagliata a 220 caratteri. Un utente non ci fa nulla: il fallback
  // dice almeno QUALE azione e' fallita.
  const errore = new Error('API error 500: Internal Server Error - {"raw":"blob"}');
  assert.equal(userMessage(errore, "Invio messaggio fallito."), "Invio messaggio fallito.");
});

test("abort e rete: decisi dal segnale, non dal messaggio", () => {
  const abort = new DOMException("The operation was aborted.", "AbortError");
  assert.ok(userMessage(abort, "fallback").includes("interrotta"));
  // `controller.abort("timeout")` in _shared.ts rigetta con questo valore.
  assert.ok(userMessage("timeout", "fallback").includes("interrotta"));
  // fetch() rigetta con TypeError quando la richiesta non parte.
  assert.ok(userMessage(new TypeError("Failed to fetch"), "fallback").includes("server"));
  // Un errore che PARLA di timeout senza esserlo non deve piu' ingannare: la
  // vecchia formatChatError decideva proprio su questa sottostringa.
  assert.equal(
    userMessage(new Error("il modello ha risposto: timeout non gestito"), "Azione fallita."),
    "Azione fallita.",
  );
});
