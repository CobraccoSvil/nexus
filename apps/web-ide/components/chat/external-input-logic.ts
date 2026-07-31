// Punto unico (regola L) della decisione "un prompt esterno arriva nel composer
// mentre l'utente ha una bozza non inviata": TUTTI i pulsanti dei pannelli che
// compongono un prompt in chat (Problemi, Debug, Playwright, Ottimizzazione,
// Servizi, evento globale nexus:chat:send, ...) convergono su externalInput di
// chat-panel, che delega qui. La bozza dell'utente non viene MAI distrutta in
// silenzio: prima del fix, setInput(externalInput) sostituiva il campo e un
// messaggio scritto ma non inviato era irrecuperabile.

/** Separatore tra bozza utente e prompt accodato: la stessa riga "---" che i
 *  prompt dei pannelli usano internamente (chat-prompts.ts, operativePreamble). */
export const EXTERNAL_PROMPT_SEPARATOR = "\n\n---\n\n";

export type ExternalInputPlan = {
  /** Contenuto del composer dopo l'arrivo del prompt esterno. */
  nextInput: string;
  /** Bozza da rimettere nel composer dopo l'invio automatico. Valorizzata solo
   *  nel percorso auto-send: il prompt parte come messaggio a se', la bozza non
   *  ne fa parte e torna al suo posto quando il composer viene svuotato. */
  draftToRestore: string | null;
};

/** Decide come far convivere la bozza corrente e il prompt esterno.
 *
 *  - Composer vuoto (o bozza identica al prompt: doppio click sullo stesso
 *    pulsante): il prompt entra da solo, comportamento storico.
 *  - Prefill manuale con bozza presente: il prompt si ACCODA sotto un
 *    separatore, come gia' fa la dettatura vocale con il transcript; l'utente
 *    rivede tutto e decide. Se la bozza termina gia' col prompt (secondo click
 *    dopo un accodamento) il campo resta invariato.
 *  - Auto-send con bozza presente: il campo deve contenere ESATTAMENTE il
 *    prompt (handshake input === autoSendPendingRef che arma l'invio
 *    automatico), quindi la bozza si mette da parte e si restituisce in
 *    draftToRestore.
 */
export function planExternalInput(args: {
  currentDraft: string;
  externalPrompt: string;
  autoSend: boolean;
}): ExternalInputPlan {
  const { currentDraft, externalPrompt, autoSend } = args;
  const draft = currentDraft.trim();
  const prompt = externalPrompt.trim();

  if (!draft || draft === prompt) {
    return { nextInput: externalPrompt, draftToRestore: null };
  }
  if (autoSend) {
    return { nextInput: externalPrompt, draftToRestore: currentDraft };
  }
  if (draft.endsWith(prompt)) {
    return { nextInput: currentDraft, draftToRestore: null };
  }
  return {
    nextInput: `${currentDraft.trimEnd()}${EXTERNAL_PROMPT_SEPARATOR}${externalPrompt}`,
    draftToRestore: null,
  };
}
