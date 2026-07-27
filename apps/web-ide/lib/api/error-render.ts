// La resa di un errore, dal lato browser. Gemello frontend del punto unico
// `crates/nexus-types/src/error_presentation.rs`.
//
// Il difetto storico: il backend rispondeva un solo campo `error` che portava il
// body grezzo del provider ("mistral HTTP 429: {...}") o la catena diagnostica dei
// log. Il frontend non aveva NIENTE di leggibile, quindi ogni superficie si e'
// arrangiata a indovinare: sette `lower.includes("429" | "timeout" | "quota"...)`
// in `formatChatError`, una tabella di regex in `global-action-feedback-provider`,
// un `humanizeTraceText` sul nastro. Tutte violavano la regola M — lo stato tecnico
// si legge dai segnali strutturati alla fonte, non dalla prosa — e sbagliavano nei
// due versi: un "429" comparso per caso in un body faceva dire "rate limit", e un
// errore vero ma dall'aria tecnica spariva.
//
// Ora la frase arriva GIA' scritta, dal punto in cui status, codice, provider e
// modello erano ancora vivi. Qui non si classifica: si LEGGE un campo.
//
// Auto-contenuto di proposito (nessun import): deve restare eseguibile da
// `node --test` senza toolchain React.

/** La resa trasportata dal backend. `code` e' l'identificatore canonico su cui
 *  decidere icona/azione (regola N: inglese, univoco), MAI il testo di `message`.
 *  `detail` e' il tecnico integrale, da mostrare solo a richiesta. */
export interface RenderedError {
  code: string;
  message: string;
  detail: string;
}

/** Frasi per i due casi che il backend non puo' rendere, perche' la risposta non
 *  e' mai arrivata: sono decisi da SEGNALI del browser (name della DOMException,
 *  tipo dell'eccezione di fetch), non dal testo. */
const MSG_INTERROTTA =
  "La richiesta e' stata interrotta (timeout di rete o navigazione). Riprova.";
const MSG_RETE =
  "Impossibile raggiungere il server. Controlla la connessione e riprova.";

/** Le tre chiavi additive del contratto (`user_message`/`user_code`/`user_detail`,
 *  scritte da `RenderedError::write_into` lato Rust), se il payload le porta.
 *
 *  Ritorna `null` quando non ci sono: un endpoint non ancora migrato non deve
 *  produrre una frase inventata. Questa funzione NON classifica e non guarda
 *  `error`. */
export function readRenderedError(payload: unknown): RenderedError | null {
  if (!payload || typeof payload !== "object") return null;
  const p = payload as Record<string, unknown>;
  const message = typeof p.user_message === "string" ? p.user_message.trim() : "";
  if (!message) return null;
  return {
    code: typeof p.user_code === "string" && p.user_code ? p.user_code : "unspecified",
    message,
    detail: typeof p.user_detail === "string" ? p.user_detail : "",
  };
}

/** La frase da mostrare per un errore catturato in un `catch`.
 *
 *  Legge un CAMPO (`rendered`, popolato da `fetchJson`) o un SEGNALE del browser;
 *  non ispeziona mai `message`. Quando non c'e' nessuno dei due usa il `fallback`
 *  del chiamante, che descrive l'AZIONE fallita ("Invio messaggio fallito.") ed e'
 *  piu' utile all'utente di un testo tecnico troncato a 220 caratteri. */
export function userMessage(error: unknown, fallback: string): string {
  // Abort: `name` e' un segnale strutturato della DOMException. La stringa
  // "timeout" e' il motivo che passiamo noi a `controller.abort("timeout")` in
  // _shared.ts — un identificatore nostro, non prosa di terzi.
  if (error instanceof DOMException && error.name === "AbortError") return MSG_INTERROTTA;
  if (error === "timeout") return MSG_INTERROTTA;

  const rendered = (error as { rendered?: RenderedError } | null)?.rendered;
  if (rendered && typeof rendered.message === "string" && rendered.message.trim()) {
    return rendered.message;
  }

  // fetch() rigetta con TypeError quando la richiesta non parte (DNS,
  // connessione rifiutata, offline): tipo dell'eccezione, non suo testo.
  if (error instanceof TypeError) return MSG_RETE;

  return fallback;
}
