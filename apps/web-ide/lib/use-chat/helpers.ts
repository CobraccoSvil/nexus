// Helper puri (non-React) usati dal hook useChat.
// Estratti da use-chat.ts (refactor god-file) MANTENENDO IDENTICO il comportamento.

import type { AgentStep, ChatMessage } from "../api-client";

/**
 * Estrae un id stabile del tool dallo step, se il backend lo include nel
 * payload (`toolInput.id`). Usato per correlare la coppia ToolUse(Running) ->
 * ToolResult(Completed) anche quando piu' step condividono lo stesso stepIndex.
 * Difensivo: toolInput e' Record<string, unknown>, l'id puo' mancare.
 */
function stepToolId(step: AgentStep): string | undefined {
  const raw = step.toolInput?.id;
  return typeof raw === "string" && raw.length > 0 ? raw : undefined;
}

/**
 * Punto unico (regola L) per inserire/aggiornare uno step in arrivo via SSE in
 * una lista di step, evitando di collassare step distinti che condividono lo
 * stesso stepIndex (difesa FIX 4).
 *
 * Regole di correlazione, in ordine:
 *  1. Se lo step in arrivo ha un toolId, fa match SOLO con uno step esistente
 *     con lo stesso toolId (merge corretto della coppia ToolUse->ToolResult).
 *  2. Altrimenti fa match per stepIndex, MA non sovrascrive uno step gia'
 *     terminato (completed/failed/skipped) con uno nuovo `running`: in quel caso
 *     accoda, cosi' due step distinti con indice ripetuto restano separati.
 *  3. Se non trova corrispondenza, accoda.
 */
export function mergeIncomingStep(current: AgentStep[], incoming: AgentStep): AgentStep[] {
  const incomingId = stepToolId(incoming);

  let matchIndex = -1;
  if (incomingId) {
    matchIndex = current.findIndex((s) => stepToolId(s) === incomingId);
  } else {
    matchIndex = current.findIndex((s) => {
      if (s.stepIndex !== incoming.stepIndex) return false;
      // Se l'esistente ha un toolId ma l'incoming no, non sono la stessa entita'.
      if (stepToolId(s)) return false;
      // Non rimpiazzare uno step gia' terminato con uno nuovo `running`:
      // sono due step distinti che riusano lo stesso indice -> accoda.
      const existingTerminal =
        s.status === "completed" || s.status === "failed" || s.status === "skipped";
      if (existingTerminal && incoming.status === "running") return false;
      return true;
    });
  }

  if (matchIndex >= 0) {
    const next = [...current];
    next[matchIndex] = incoming;
    return next;
  }
  return [...current, incoming];
}

/**
 * Inserisce o aggiorna un messaggio assistant sintetico (chiave: message.id).
 * Se esiste gia' un messaggio con lo stesso id lo sostituisce, altrimenti lo accoda.
 */
export function upsertSyntheticAssistantMessage(
  current: ChatMessage[],
  message: ChatMessage | null,
): ChatMessage[] {
  // message null: difesa residua. Dal FIX 5 createTerminalMessage costruisce
  // SEMPRE un messaggio per i run terminati (anche cancelled/superseded vuoti),
  // quindi questo ramo non scatta in pratica; resta come guard del tipo
  // ChatMessage | null. No-op.
  if (!message) {
    return current;
  }
  const index = current.findIndex((item) => item.id === message.id);
  if (index >= 0) {
    const next = [...current];
    next[index] = message;
    return next;
  }
  return [...current, message];
}

/** Stati di un run considerati terminali (run concluso, non piu' in esecuzione). */
export function isStatusTerminal(status: string): boolean {
  return (
    status === "completed" ||
    status === "failed" ||
    status === "timed_out" ||
    status === "cancelled" ||
    status === "interrupted" ||
    status === "loop_aborted" ||
    status === "provider_unavailable" ||
    // Esiti canonici macchina a stati (mig 0386): terminali.
    status === "completed_verified" ||
    status === "failed_diagnosed" ||
    // ADR 0034: blocked_needs_input e' TERMINALE — il run e' concluso con la
    // dichiarazione onesta "serve input umano"; il prossimo messaggio crea un
    // nuovo run (nessun resume esiste per questo stato, a differenza di
    // awaiting_confirmation che resta un run sospeso).
    status === "blocked_needs_input"
  );
}
