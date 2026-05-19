/**
 * Modello catalog frontend — single source di verita' per i dati statici
 * dei modelli AI usati dalle UI: prezzi, context window, lista per provider.
 *
 * **NOTA (CLAUDE.md §G)**: i nomi modello qui presenti sono hardcoded in
 * deroga temporanea. Il fix corretto e' farli fluire da:
 *
 *   - `ai_price_catalog` (provider+model -> input/output/cacheRead price,
 *     `capabilities` JSONB con `thinking`, ecc. — mig 0006 + 0170)
 *   - `nexus_routing_matrix` (intent x behavior_mode -> provider+model_id)
 *   - `nexus_purpose_model`  (purpose -> provider+model_id)
 *
 * via endpoint Next API che proxa al brain Python o direttamente al mcp-core.
 * Quando la migrazione sara' completata, tutti i Record/Array qui sotto
 * spariranno (sostituiti da hook fetch+cache) e questo file potra' essere
 * cancellato.
 *
 * Inoltre i 5 file frontend che usavano il dato (inline-trace-panel,
 * ai-trace-panel, profile-editor, context-window, routing-config) ora
 * condividono questo modulo invece di duplicarlo. Questa unificazione e' il
 * primo passo del refactor: rende il prossimo step (API) gestibile in un
 * solo punto.
 */

// ── Prezzi per milione di token (USD) — aggiornati aprile 2026 ─────────────

export interface ModelPrice {
  input: number;
  output: number;
  /** Tariffa per token di prompt caching read; default = input * 0.1. */
  cacheRead?: number;
}

export const MODEL_PRICING: Record<string, ModelPrice> = {
  // Anthropic
  "claude-opus-4-6":             { input: 5.0,   output: 25.0,  cacheRead: 0.50 },
  "claude-sonnet-4-6":           { input: 3.0,   output: 15.0,  cacheRead: 0.30 },
  "claude-haiku-4-5-20251001":   { input: 0.80,  output: 4.0,   cacheRead: 0.08 },
  "claude-3-haiku-20240307":     { input: 0.25,  output: 1.25  },
  // OpenAI
  "o3":                          { input: 10.0,  output: 40.0  },
  "o4-mini":                     { input: 1.10,  output: 4.40  },
  "gpt-4.1":                     { input: 2.0,   output: 8.0   },
  "gpt-4.1-mini":                { input: 0.40,  output: 1.60  },
  "gpt-4.1-nano":                { input: 0.10,  output: 0.40  },
  "gpt-4o":                      { input: 2.50,  output: 10.0  },
  "gpt-4o-mini":                 { input: 0.15,  output: 0.60  },
  "gpt-4-turbo":                 { input: 10.0,  output: 30.0  },
  "o1":                          { input: 15.0,  output: 60.0  },
  "o1-mini":                     { input: 3.0,   output: 12.0  },
  // Google
  "gemini-2.5-pro":              { input: 1.25,  output: 10.0  },
  "gemini-2.5-flash":            { input: 0.15,  output: 0.60  },
  "gemini-2.5-flash-lite":       { input: 0.10,  output: 0.40  },
  "gemini-2.0-flash":            { input: 0.10,  output: 0.40  },
  "gemini-1.5-pro":              { input: 1.25,  output: 5.0   },
  "gemini-1.5-flash":            { input: 0.075, output: 0.30  },
  // DeepSeek
  "deepseek-chat":               { input: 0.28,  output: 0.42  },
  "deepseek-reasoner":           { input: 0.55,  output: 2.19  },
  "deepseek-coder":              { input: 0.28,  output: 0.42  },
  // Mistral
  "mistral-large-2411":          { input: 2.0,   output: 6.0   },
  "mistral-small-4":             { input: 0.15,  output: 0.60  },
  "codestral-latest":            { input: 0.20,  output: 0.60  },
  "open-mistral-nemo":           { input: 0.15,  output: 0.15  },
};

/**
 * Calcola il costo in USD per un'interazione, dato il modello e i conteggi
 * token. Ritorna `null` se il modello non e' nel catalogo (UI nasconde la
 * cella). Formula: input rimanente (al netto del cache) * pi + cache * pc +
 * output * po, diviso per 1_000_000.
 */
export function calcModelCost(
  model: string,
  inputTokens: number,
  outputTokens: number,
  cacheReadTokens = 0,
): number | null {
  const key = Object.keys(MODEL_PRICING).find((k) =>
    model.toLowerCase().includes(k.toLowerCase()),
  );
  if (!key) return null;
  const p = MODEL_PRICING[key];
  // I cacheReadTokens NON si sottraggono dagli inputTokens: sono token gia'
  // contati nell'input ma fatturati a tariffa ridotta (cacheRead) invece
  // della tariffa piena (input).
  const billableInput = Math.max(0, inputTokens - cacheReadTokens);
  const inputCost  = (billableInput   * p.input)                          / 1_000_000;
  const cacheCost  = (cacheReadTokens * (p.cacheRead ?? p.input * 0.1))   / 1_000_000;
  const outputCost = (outputTokens    * p.output)                         / 1_000_000;
  return inputCost + cacheCost + outputCost;
}

/** Format USD con soglie di precisione adattive. */
export function formatCostUsd(usd: number): string {
  if (usd <= 0)     return "$0.000";
  if (usd < 0.0001) return "< $0.0001";
  if (usd < 0.001)  return `$${usd.toFixed(5)}`;
  if (usd < 0.01)   return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(3)}`;
}

// ── Context window per modello (token) ─────────────────────────────────────

/**
 * Mappa di fallback delle context window per modello. Usata solo se il
 * catalogo DB (`ai_price_catalog`) non risponde o il modello non e' presente.
 * Fonte autoritativa: `getModels()` del brain.
 */
export const MODEL_CONTEXT_WINDOW: Record<string, number> = {
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
 * Stima conservativa della context window in token per un modello. Ritorna
 * `null` se sconosciuto (UI nasconde la percentuale).
 */
export function fallbackContextWindow(model: string | null | undefined): number | null {
  if (!model) return null;
  if (MODEL_CONTEXT_WINDOW[model] != null) return MODEL_CONTEXT_WINDOW[model];
  // prefisso matching: es. "claude-sonnet-4-6-20250101" → "claude-sonnet-4-6"
  for (const key of Object.keys(MODEL_CONTEXT_WINDOW)) {
    if (model.startsWith(key)) return MODEL_CONTEXT_WINDOW[key];
  }
  return null;
}

// ── Lista modelli per provider (UI dropdown) ───────────────────────────────

export const PROVIDER_MODELS: Record<string, string[]> = {
  anthropic: ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5-20251001", "claude-3-haiku-20240307"],
  openai:    ["gpt-4.1-mini", "gpt-4.1", "gpt-4.1-nano", "o4-mini", "o3", "gpt-4o-mini"],
  google:    ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.5-flash-lite", "gemini-2.0-flash", "gemini-1.5-flash"],
  deepseek:  ["deepseek-chat", "deepseek-reasoner", "deepseek-coder"],
  mistral:   ["mistral-small-4", "mistral-large-2411", "codestral-latest", "open-mistral-nemo"],
  auto: [],
  "": [],
};
