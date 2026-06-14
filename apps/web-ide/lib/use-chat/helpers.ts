// Helper puri (non-React) usati dal hook useChat.
// Estratti da use-chat.ts (refactor god-file) MANTENENDO IDENTICO il comportamento.

import type { ChatMessage } from "../api-client";

/**
 * Inserisce o aggiorna un messaggio assistant sintetico (chiave: message.id).
 * Se esiste gia' un messaggio con lo stesso id lo sostituisce, altrimenti lo accoda.
 */
export function upsertSyntheticAssistantMessage(
  current: ChatMessage[],
  message: ChatMessage | null,
): ChatMessage[] {
  // message null: il run terminale non ha prodotto un messaggio da mostrare
  // (es. run cancellato senza risposta reale, vedi createTerminalMessage). No-op.
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
    // Esiti canonici macchina a stati (mig 0386): terminali. blocked_needs_input
    // NO: e' in attesa di input (come awaiting_confirmation), non terminale.
    status === "completed_verified" ||
    status === "failed_diagnosed"
  );
}
