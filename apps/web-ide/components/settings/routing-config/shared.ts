import { useThemeColors } from "../../../lib/theme";

export const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

export interface SettingEntry {
  key: string;
  value: string;
  category: string;
  description: string;
  is_secret: boolean;
  has_value: boolean;
  updated_at: string;
}

// Nome provider dinamico (dal registry). Alias `string` mantenuto per leggibilita'
// delle firme; la lista reale arriva dal registry a runtime, non piu' hardcoded.
export type ProviderName = string;

export type BehaviorMode = "veloce" | "economica" | "bilanciata" | "approfondita" | "dinamico" | "manuale";

export interface RoutingConfigState {
  providerHierarchy: ProviderName[];
  intentChains: Record<string, ProviderName[]>;
  tokenBudget: string;
  maxTokenBudget: string;
  behaviorMode: BehaviorMode;
  purposeModels: Record<string, PurposeModelConfig>;
}

export interface PurposeModelConfig {
  provider: ProviderName;
  model_id: string;
  notes?: string | null;
  tier?: string | null;
  required_capability?: string | null;
  requires_tool_use?: boolean;
}

// Scala di capacita' a 5 livelli (light < medium < high < heavy < frontier),
// allineata a ai_price_catalog.performance_tier (mig 0528) e ai CHECK vivi (0547).
export const PURPOSE_TIER_OPTIONS: Array<{ value: string; label: string }> = [
  { value: "", label: "Statico (modello fisso)" },
  { value: "light", label: "Light" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "heavy", label: "Heavy" },
  { value: "frontier", label: "Frontier" },
];

// Lista di PARTENZA, valida solo nell'istante fra il primo render e la risposta
// di GET /api/admin/provider-registry, che e' la fonte autoritativa. Serve a non
// far nascere la pagina con catene vuote (verrebbero salvate come tali).
//
// NON e' piu' una rete per il fetch fallito: se il registry non risponde la
// pagina lo DICE (`providersError` in index.tsx). Prima taceva, e restare su
// questi cinque nomi significava nascondere groq, openrouter e vertex facendoli
// sembrare inesistenti.
export const FALLBACK_PROVIDERS: string[] = ["anthropic", "openai", "google", "deepseek", "mistral"];

export const BEHAVIOR_MODES: { value: BehaviorMode; label: string; desc: string }[] = [
  { value: "veloce",       label: "Veloce",       desc: "Massima velocità — modello più rapido per tier richiesto" },
  { value: "economica",    label: "Economica",    desc: "Minimo costo — modello più economico capace di gestire il task" },
  { value: "bilanciata",   label: "Bilanciata",   desc: "Qualità/costo ottimale (default)" },
  { value: "approfondita", label: "Approfondita", desc: "Massima qualità — scala automaticamente il tier" },
  { value: "dinamico",     label: "Dinamico",     desc: "Consulta il catalogo modelli in tempo reale: sceglie il modello ottimale per capability, tier e costo corrente" },
  { value: "manuale",      label: "Manuale",      desc: "Configura manualmente provider, modelli e catene per intent" },
];

// La preview del routing e' ora REALE (dalla matrice DB corrente) via
// GET /api/models/routing-preview -> RoutingPreviewEntry in lib/api/models.ts.
// La vecchia NEXUS_ROUTING_MATRIX statica (mirror hardcoded, con modelli stale) e
// PROVIDER_MODELS sono state rimosse: i modelli per provider arrivano dal catalog.

export const ROUTING_INTENTS = [
  { key: "chat", label: "Chat" },
  { key: "fix", label: "Fix" },
  { key: "refactor", label: "Refactor" },
  { key: "test", label: "Test" },
  { key: "docs", label: "Docs" },
  { key: "architecture", label: "Architecture" },
];

export const MANAGED_ROUTING_KEYS = new Set([
  "provider_hierarchy",
  "default_provider",
  "token_budget",
  "max_token_budget",
  "nexus_behavior_mode",
  "nexus_active_routing_pct",
  "routing_chat_providers",
  "routing_fix_providers",
  "routing_refactor_providers",
  "routing_test_providers",
  "routing_docs_providers",
  "routing_architecture_providers",
]);

// Normalizza una catena provider contro la lista dinamica `providers` (dal registry):
// tiene solo i provider noti nell'ordine dato, poi accoda quelli mancanti (tutti i
// provider disponibili restano sempre presenti nella catena).
function normalizeProviderChain(values: string[], providers: string[]): ProviderName[] {
  const known = new Set(providers);
  const cleaned = values
    .map((value) => value.trim().toLowerCase())
    .filter((value) => known.has(value));
  const unique = cleaned.filter((value, index) => cleaned.indexOf(value) === index);
  const result = [...unique];
  for (const p of providers) {
    if (!result.includes(p)) result.push(p);
  }
  return result;
}

function parseProviderChain(value: string | undefined, providers: string[]): ProviderName[] {
  if (!value) return [...providers];
  return normalizeProviderChain(value.split(","), providers);
}

export function buildRoutingState(settings: SettingEntry[], providers: string[]): RoutingConfigState {
  const get = (key: string) => settings.find((setting) => setting.key === key)?.value ?? "";
  const providerHierarchy = parseProviderChain(get("provider_hierarchy") || get("default_provider"), providers);

  return {
    providerHierarchy,
    intentChains: Object.fromEntries(
      ROUTING_INTENTS.map((intent) => [
        intent.key,
        parseProviderChain(get(`routing_${intent.key}_providers`) || providerHierarchy.join(","), providers),
      ]),
    ) as Record<string, ProviderName[]>,
    tokenBudget: get("token_budget") || "4096",
    maxTokenBudget: get("max_token_budget") || "32000",
    behaviorMode: (get("nexus_behavior_mode") || "manuale") as BehaviorMode,
    purposeModels: {},
  };
}

export function moveProvider(chain: ProviderName[], provider: ProviderName, direction: -1 | 1): ProviderName[] {
  const index = chain.indexOf(provider);
  const target = index + direction;
  if (index < 0 || target < 0 || target >= chain.length) return chain;
  const next = [...chain];
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

export function labelProvider(provider: string): string {
  const labels: Record<string, string> = {
    anthropic:  "Anthropic",
    openai:     "OpenAI",
    google:     "Google",
    deepseek:   "DeepSeek",
    mistral:    "Mistral",
    groq:       "Groq",
    openrouter: "OpenRouter",
    perplexity: "Perplexity",
    vllm:       "vLLM",
    ollama:     "Ollama",
  };
  return labels[provider] ?? (provider.charAt(0).toUpperCase() + provider.slice(1));
}

export function buttonStyle(tc: ReturnType<typeof useThemeColors>, disabled: boolean) {
  return {
    padding: "4px 10px",
    borderRadius: 6,
    border: "1px solid var(--color-border)",
    background: disabled ? tc.bgHover : tc.bgInput,
    color: disabled ? tc.textMuted : tc.textSecondary,
    fontSize: 11,
    cursor: disabled ? "not-allowed" : "pointer",
    fontFamily: "inherit",
  } as const;
}

export function inputStyle(_tc: ReturnType<typeof useThemeColors>) {
  return {
    width: "100%",
    padding: "8px 12px",
    borderRadius: 6,
    border: "1px solid var(--color-border)",
    background: "var(--color-bgInput)",
    color: "var(--color-text)",
    fontSize: 13,
    fontFamily: "inherit",
    boxSizing: "border-box" as const,
  };
}

export const PURPOSE_KEYS: Array<{ key: string; label: string; desc: string }> = [
  { key: "loop_fallback_default", label: "Loop fallback", desc: "Modello usato quando un agent va in loop su tool call ripetute." },
  { key: "admin_fallback_default", label: "Admin fallback", desc: "Fallback admin per prompt/templates quando serve un modello più capace." },
  { key: "docs_generator", label: "Docs generator", desc: "Generazione documenti (report, release notes, ecc.)." },
  { key: "custom_instructions", label: "Custom instructions", desc: "Generazione istruzioni custom per progetti." },
  { key: "chat_title_generator", label: "Chat title", desc: "Generazione titolo sessioni chat." },
  { key: "chat_feedback_generator", label: "Chat feedback", desc: "Generazione feedback/riassunti." },
  { key: "google_batch", label: "Google batch", desc: "Fallback per batch tasks (se configurato)." },
  { key: "agent_tier_opus", label: "Agent tier: Opus", desc: "Tier per agent ad alto impatto." },
  { key: "agent_tier_sonnet", label: "Agent tier: Sonnet", desc: "Tier per agent general purpose." },
  { key: "agent_tier_haiku", label: "Agent tier: Haiku", desc: "Tier per task rapidi/low-cost." },
  { key: "wiki_code_docs_enricher", label: "Wiki code-docs", desc: "Arricchimento schede descrittive dei file di codice per la knowledge base" },
];
