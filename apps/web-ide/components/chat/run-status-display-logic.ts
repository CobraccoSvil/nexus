// Punto unico (regola L) di UNA decisione: quando il nastro attivita' (ADR
// 0037, flag `chat.activity_stream_enabled`) e' abilitato, un turno assistant
// con runId mostra il proprio ESITO tramite il badge PERSISTENTE
// (RunStatusBadge, sempre disponibile da message.runStatus) oppure tramite la
// riga storica compatta (ActivityHistoryRow, che porta lo stesso stato +
// trail provider + costo).
//
// Prima questa decisione era SPARSA in due punti indipendenti di
// message-list.tsx: la condizione di soppressione di RunStatusBadge (guardava
// activityStreamEnabled + runId + "non e' l'ultimo turno") e il gate
// `hasRunData` davanti al blocco che sceglie tra MessageActivityStream e
// ActivityHistoryRow (guardava se il client aveva ANCORA in memoria
// meta-step/step/trace per quello specifico runId). Le due condizioni
// POTEVANO divergere: un turno chiuso un istante prima che ne partisse un
// altro nella stessa chat smette di essere "l'ultimo turno" (sopprime
// RunStatusBadge) ma puo' non avere piu' dati di nastro in memoria per quel
// runId (hasRunData=false, il blocco storico si azzerava con `return null`) --
// risultato: NESSUNA delle due sorgenti rendeva lo stato, restava visibile
// solo il pie' di costo (badge "usage", indipendente). Misurato 31/07/2026,
// progetto bacheca-attivita, run 51fe77ce (failed_diagnosed).
//
// Il fix e' strutturale: questa funzione e' l'UNICA fonte della decisione, e
// ActivityHistoryRow (chiamata dal lato "history-row") non dipende MAI da
// hasRunData per la propria intestazione (badge/trail/costo vengono dai campi
// PERSISTITI del messaggio, regola M) -- hasRunData resta rilevante SOLO per
// decidere se espandere il nastro dettagliato, non se mostrare lo stato.

export type RunStatusBadgeSource = "persistent-badge" | "history-row";

/**
 * Quale elemento porta l'indicatore di stato per QUESTO turno.
 *
 * - Senza runId, o col nastro disattivato: sempre il badge persistente
 *   (nessuna riga storica esiste in questi casi).
 * - Con nastro attivo: l'ULTIMO turno assistant resta sul badge persistente
 *   (accanto, se disponibile, al nastro ESPANSO che narra i dettagli); ogni
 *   turno PRECEDENTE passa alla riga storica compatta, che lo mostra SEMPRE
 *   (indipendentemente da quanti dati di dettaglio il client ha ancora in
 *   memoria per quel run).
 */
export function runStatusBadgeSource(params: {
  activityStreamEnabled: boolean;
  hasRunId: boolean;
  isLastAssistantRun: boolean;
}): RunStatusBadgeSource {
  if (!params.hasRunId || !params.activityStreamEnabled) return "persistent-badge";
  return params.isLastAssistantRun ? "persistent-badge" : "history-row";
}
