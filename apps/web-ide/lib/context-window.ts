/**
 * Mappa di fallback delle context window per modello.
 * Usata solo se il catalogo DB (ai_price_catalog) non risponde o il modello
 * non e' presente. La fonte autoritativa resta sempre `getModels()`.
 */

const CONTEXT_WINDOW_DEFAULTS: Record<string, number> = {
  // Anthropic
  "claude-opus-4-7": 200_000,
  "claude-opus-4-6": 200_000,
  "claude-sonnet-4-6": 200_000,
  "claude-sonnet-4-5": 200_000,
  "claude-sonnet-3-7": 200_000,
  "claude-haiku-4-5-20251001": 200_000,
  "claude-haiku-4-5": 200_000,
  "claude-3-5-haiku-20241022": 200_000,
  "claude-3-5-sonnet-20241022": 200_000,
  // OpenAI
  "gpt-4o": 128_000,
  "gpt-4o-mini": 128_000,
  "gpt-4-turbo": 128_000,
  "o1-mini": 128_000,
  "o1-preview": 128_000,
  // Google
  "gemini-2.5-pro": 1_048_576,
  "gemini-2.5-flash": 1_048_576,
  "gemini-2.0-flash": 1_048_576,
  "gemini-1.5-pro": 2_097_152,
  "gemini-1.5-flash": 1_048_576,
  // DeepSeek
  "deepseek-chat": 128_000,
  "deepseek-reasoner": 128_000,
  // Mistral
  "mistral-large-latest": 128_000,
  "mistral-small-latest": 32_768,
};

/**
 * Stima conservativa della context window in token per un modello.
 * Ritorna `null` se sconosciuto (UI dovra' nascondere la percentuale).
 */
export function fallbackContextWindow(model: string | null | undefined): number | null {
  if (!model) return null;
  if (CONTEXT_WINDOW_DEFAULTS[model] != null) return CONTEXT_WINDOW_DEFAULTS[model];
  // prefisso matching: es. "claude-sonnet-4-6-20250101" → "claude-sonnet-4-6"
  for (const key of Object.keys(CONTEXT_WINDOW_DEFAULTS)) {
    if (model.startsWith(key)) return CONTEXT_WINDOW_DEFAULTS[key];
  }
  return null;
}
