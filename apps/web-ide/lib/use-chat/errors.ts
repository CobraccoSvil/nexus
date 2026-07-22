// La frase d'errore mostrata in chat.
//
// Qui vivevano sette rami `lower.includes("429" | "rate limit" | "quota" |
// "not found" | "connection error" | "transport error" | "timeout")` piu' un
// troncamento a 220 caratteri: una classificazione fatta sul TESTO gia'
// appiattito (regola M), cieca a ogni formulazione nuova di un provider e pronta
// a sbagliare quando una di quelle parole compariva per caso in un body.
//
// Ora la frase arriva dal backend, reso dove i fatti erano vivi, e questo modulo
// si limita a delegare al punto unico frontend.

import { userMessage } from "../api/error-render";

export function formatChatError(error: unknown, fallback: string): string {
  return userMessage(error, fallback);
}
