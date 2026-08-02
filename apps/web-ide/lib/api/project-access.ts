// Cosa il client FA davanti a un 403 su un progetto. Gemello frontend del
// vocabolario CHIUSO `AccessDenial` (crates/nexus-types/src/error_presentation.rs).
//
// Il difetto storico: la decisione si prendeva su una FRASE ITALIANA nel corpo
// della risposta — `errText.includes("Progetto non accessibile")` dentro
// `fetchJson` — e su quella frase il client buttava via le chiavi `localStorage`
// del progetto e reindirizzava. Una riformulazione del messaggio, una
// traduzione o un 403 di tutt'altra origine la cambiavano in silenzio nei due
// versi: non cancellare quando il progetto era davvero sparito, o cancellare lo
// stato di chi non doveva perderlo (regola M).
//
// Auto-contenuto di proposito (nessun import), come error-render.ts: deve
// restare eseguibile da `node --test` senza toolchain React.

/** L'UNICO codice che autorizza a dimenticare un progetto: la riga di
 *  appartenenza non esiste piu' (eliminato, oppure l'utente non ne fa parte) e
 *  quindi ogni riferimento locale a quell'id e' morto.
 *
 *  Gli altri codici di accesso negato NON sono equivalenti: con
 *  `project_permission_denied` il progetto e' vivo e i riferimenti locali
 *  restano validi. */
export const PROGETTO_INACCESSIBILE = "project_not_accessible";

/** Il codice canonico di un 403 autorizza a buttare via lo stato locale del
 *  progetto?
 *
 *  FALLBACK DICHIARATO: codice assente (endpoint non ancora migrato, backend
 *  indietro rispetto al frontend durante un deploy) o sconosciuto ritorna
 *  `false`. In dubbio non si cancella: fra i due danni, buttare via lo stato di
 *  chi non doveva perderlo e' il peggiore — uno stato stale produce un toast di
 *  errore fino al reload, una cancellazione indebita perde il lavoro locale. */
export function autorizzaOblioProgetto(code: string | null | undefined): boolean {
  return code === PROGETTO_INACCESSIBILE;
}

/** L'id del progetto a cui la chiamata si riferiva, dal path
 *  `/api/projects/{uuid}`. `null` quando l'URL non nomina un progetto: senza id
 *  non c'e' niente da dimenticare.
 *
 *  Dal PATH e non dalla query: e' il progetto che il server ha appena
 *  rifiutato, non quello che la pagina sta guardando. */
export function progettoDallUrl(url: string): string | null {
  return url.match(/\/api\/projects\/([0-9a-f-]{36})/i)?.[1] ?? null;
}
