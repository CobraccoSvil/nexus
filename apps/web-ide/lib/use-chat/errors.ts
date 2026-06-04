// Normalizzazione dei messaggi di errore mostrati in chat.
// Funzione pura estratta da use-chat.ts (refactor god-file) senza cambiamenti di comportamento.

export function formatChatError(error: unknown, fallback: string): string {
  if (error instanceof DOMException && error.name === "AbortError") {
    return "La richiesta e' stata interrotta (timeout di rete o navigazione). Riprova.";
  }
  const raw = error instanceof Error ? error.message : fallback;
  const normalized = raw.trim();
  const lower = normalized.toLowerCase();

  if (lower.includes("aborted") || lower.includes("abort")) {
    return "La richiesta e' stata interrotta. Riprova tra qualche secondo.";
  }
  if (
    lower.includes("429") ||
    lower.includes("rate limit") ||
    lower.includes("rate_limit") ||
    lower.includes("quota")
  ) {
    return "Il provider AI e' temporaneamente in rate limit. Riprovo in fallback automatico; se persiste, attendi qualche secondo e ripeti.";
  }
  if (
    (lower.includes("not_found_error") || lower.includes("not found")) &&
    lower.includes("model")
  ) {
    return "Il modello selezionato non e' disponibile presso il provider corrente. Prova un modello diverso o lascia la selezione automatica.";
  }
  if (lower.includes("connection error")) {
    return "Connessione al provider interrotta durante l'esecuzione. Ho mantenuto lo stato del run; puoi riprovare subito.";
  }
  if (
    lower.includes("transport error") ||
    lower.includes("status: unavailable") ||
    lower.includes("connection refused")
  ) {
    return "Connessione interna ai servizi AI temporaneamente non disponibile. Riprova tra pochi secondi.";
  }
  if (lower.includes("timeout")) {
    return "La richiesta e' andata in timeout. Riprova tra poco o con un prompt piu' breve.";
  }
  const compact = normalized.replace(/\s+/g, " ");
  if (compact.startsWith("{") || compact.startsWith("[")) {
    return fallback;
  }
  if (compact.length > 220) {
    return `${compact.slice(0, 220)}...`;
  }
  return compact || fallback;
}
