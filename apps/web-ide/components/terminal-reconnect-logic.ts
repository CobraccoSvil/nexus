/**
 * Ritmo dei tentativi di riconnessione del terminale.
 *
 * Il difetto che ha reso necessario questo punto: dopo sei tentativi il pannello
 * si arrendeva DEFINITIVAMENTE, scriveva «Riapri la scheda per riprovare» e non
 * ci provava piu' — nemmeno quando il backend tornava disponibile. Ma il riavvio
 * del backend e' un evento NORMALE in sviluppo (tre volte il 29/07/2026, per
 * altrettanti deploy), e ogni volta lasciava dietro di se' un terminale morto con
 * un messaggio d'errore congelato al momento del guasto. L'utente doveva
 * accorgersene e agire, per un servizio che nel frattempo era tornato su.
 *
 * Il criterio e' che la riconnessione NON si arrende: i primi tentativi sono
 * ravvicinati e crescono in fretta (per riprendersi subito da un'interruzione
 * breve), poi si assestano su un intervallo tranquillo e continuano finche' la
 * scheda resta aperta. A regime e' una richiesta ogni mezzo minuto: non costa
 * nulla, e recupera da sola.
 *
 * Chiudere il tentativo quando la scheda si chiude NON e' compito di questa
 * funzione: se ne occupa gia' il flag di smontaggio del componente, che azzera il
 * timer. Qui si decide solo il RITMO.
 */

/** Ritardo minimo: sotto, una raffica di tentativi somiglierebbe a un attacco. */
const RITARDO_INIZIALE_MS = 1200;

/** Tetto del backoff, e passo costante una volta raggiunto. */
const RITARDO_MASSIMO_MS = 30000;

/**
 * Ritardo prima del tentativo numero `tentativo` (0 = il primo dopo la caduta).
 *
 * Cresce come 1,2s / 2,4s / 4,8s / 9,6s / 19,2s e poi resta a 30s per sempre.
 * Nessun valore di `tentativo` produce un ritardo nullo o infinito: un ritardo
 * nullo trasformerebbe la riconnessione in un ciclo stretto, e non esiste un
 * numero di tentativi oltre il quale smettere.
 */
export function ritardoRiconnessioneMs(tentativo: number): number {
  const n = Number.isFinite(tentativo) && tentativo > 0 ? Math.floor(tentativo) : 0;
  // 2^n cresce in fretta: oltre il tetto il calcolo e' inutile e a numeri grandi
  // darebbe Infinity, che come ritardo significherebbe «mai piu'».
  if (n >= 32) return RITARDO_MASSIMO_MS;
  return Math.min(RITARDO_MASSIMO_MS, RITARDO_INIZIALE_MS * 2 ** n);
}

/**
 * Il tentativo e' ancora nella fase ravvicinata, o si e' assestato sul passo
 * costante? Serve solo a scegliere COSA scrivere nel terminale: durante la fase
 * ravvicinata il numero del tentativo e' informazione utile, a regime diventa
 * rumore che scorre.
 */
export function inFaseRavvicinata(tentativo: number): boolean {
  return ritardoRiconnessioneMs(tentativo) < RITARDO_MASSIMO_MS;
}
