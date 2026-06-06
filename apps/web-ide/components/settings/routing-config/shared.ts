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

export type ProviderName = "anthropic" | "openai" | "google" | "deepseek" | "mistral";

export type BehaviorMode = "veloce" | "economica" | "bilanciata" | "approfondita" | "dinamico" | "manuale";

export interface RoutingConfigState {
  providerHierarchy: ProviderName[];
  providerModels: Record<ProviderName, string>;
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

export const PURPOSE_TIER_OPTIONS: Array<{ value: string; label: string }> = [
  { value: "", label: "Statico (modello fisso)" },
  { value: "light", label: "Light" },
  { value: "medium", label: "Medium" },
  { value: "heavy", label: "Heavy" },
];

export const PROVIDERS: ProviderName[] = ["anthropic", "openai", "google", "deepseek", "mistral"];

export const BEHAVIOR_MODES: { value: BehaviorMode; label: string; desc: string }[] = [
  { value: "veloce",       label: "⚡ Veloce",       desc: "Massima velocità — modello più rapido per tier richiesto" },
  { value: "economica",    label: "💰 Economica",    desc: "Minimo costo — modello più economico capace di gestire il task" },
  { value: "bilanciata",   label: "⚖️ Bilanciata",   desc: "Qualità/costo ottimale (default)" },
  { value: "approfondita", label: "🔬 Approfondita", desc: "Massima qualità — scala automaticamente il tier" },
  { value: "dinamico",     label: "🤖 Dinamico",     desc: "Consulta il catalogo modelli in tempo reale: sceglie il modello ottimale per capability, tier e costo corrente" },
  { value: "manuale",      label: "🔧 Manuale",      desc: "Configura manualmente provider, modelli e catene per intent" },
];

// Routing matrix frontend — specchio di orchestrator.rs, con dimensione token-complessità
export interface MatrixEntry { provider: ProviderName; model: string; tokens?: string }

export const NEXUS_ROUTING_MATRIX: Record<string, Array<{ label: string } & MatrixEntry>> = {
  veloce: [
    { label: "Chat breve",      provider: "google",    model: "gemini-2.5-flash-lite",    tokens: "≤400 tk" },
    { label: "Chat media",      provider: "openai",    model: "gpt-4.1-mini",             tokens: "≤1500 tk" },
    { label: "Chat lunga",      provider: "mistral",   model: "mistral-small-4",          tokens: ">1500 tk" },
    { label: "Fix semplice",    provider: "openai",    model: "gpt-4.1-mini",             tokens: "≤3000 tk" },
    { label: "Fix complesso",   provider: "deepseek",  model: "deepseek-chat",            tokens: ">3000 tk" },
    { label: "Refactor",        provider: "deepseek",  model: "deepseek-chat" },
    { label: "Test",            provider: "openai",    model: "gpt-4.1-mini" },
    { label: "Docs",            provider: "mistral",   model: "mistral-small-4" },
    { label: "Architecture",    provider: "deepseek",  model: "deepseek-reasoner" },
  ],
  economica: [
    { label: "Chat breve",      provider: "openai",    model: "gpt-4.1-nano",             tokens: "≤400 tk" },
    { label: "Chat media",      provider: "mistral",   model: "open-mistral-nemo",             tokens: "≤1500 tk" },
    { label: "Chat lunga",      provider: "deepseek",  model: "deepseek-chat",            tokens: ">1500 tk" },
    { label: "Fix semplice",    provider: "openai",    model: "gpt-4.1-nano",             tokens: "≤3000 tk" },
    { label: "Fix complesso",   provider: "deepseek",  model: "deepseek-chat",            tokens: ">3000 tk" },
    { label: "Refactor",        provider: "deepseek",  model: "deepseek-chat" },
    { label: "Test",            provider: "mistral",   model: "open-mistral-nemo" },
    { label: "Docs",            provider: "mistral",   model: "open-mistral-nemo" },
    { label: "Architecture",    provider: "deepseek",  model: "deepseek-chat" },
  ],
  bilanciata: [
    { label: "Chat breve",      provider: "google",    model: "gemini-2.5-flash",         tokens: "≤400 tk" },
    { label: "Chat media",      provider: "openai",    model: "gpt-4.1-mini",             tokens: "≤1500 tk" },
    { label: "Chat lunga",      provider: "anthropic", model: "claude-haiku-4-5-20251001",tokens: ">1500 tk" },
    { label: "Fix semplice",    provider: "openai",    model: "gpt-4.1-mini",             tokens: "≤3000 tk" },
    { label: "Fix complesso",   provider: "anthropic", model: "claude-haiku-4-5-20251001",tokens: ">3000 tk" },
    { label: "Refactor",        provider: "anthropic", model: "claude-haiku-4-5-20251001" },
    { label: "Test",            provider: "openai",    model: "gpt-4.1-mini" },
    { label: "Docs",            provider: "openai",    model: "gpt-4.1" },
    { label: "Architecture",    provider: "anthropic", model: "claude-sonnet-4-6" },
  ],
  approfondita: [
    { label: "Chat breve",      provider: "mistral",   model: "mistral-small-4",          tokens: "≤400 tk" },
    { label: "Chat media",      provider: "deepseek",  model: "deepseek-chat",            tokens: "≤1500 tk" },
    { label: "Chat lunga",      provider: "anthropic", model: "claude-sonnet-4-6",        tokens: ">1500 tk" },
    { label: "Fix semplice",    provider: "anthropic", model: "claude-haiku-4-5-20251001",tokens: "≤3000 tk" },
    { label: "Fix complesso",   provider: "anthropic", model: "claude-sonnet-4-6",        tokens: ">3000 tk" },
    { label: "Refactor",        provider: "anthropic", model: "claude-sonnet-4-6" },
    { label: "Test",            provider: "mistral",   model: "codestral-latest" },
    { label: "Docs",            provider: "anthropic", model: "claude-sonnet-4-6" },
    { label: "Architecture",    provider: "anthropic", model: "claude-opus-4-6" },
  ],
};


export const PROVIDER_MODELS: Record<ProviderName, string[]> = {
  anthropic: ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5-20251001", "claude-3-haiku-20240307"],
  openai:    ["gpt-4.1-mini", "gpt-4.1", "gpt-4.1-nano", "o4-mini", "o3", "gpt-4o-mini"],
  google:    ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.5-flash-lite", "gemini-2.0-flash", "gemini-1.5-flash"],
  deepseek:  ["deepseek-chat", "deepseek-reasoner", "deepseek-coder"],
  mistral:   ["mistral-small-4", "mistral-large-2411", "codestral-latest", "open-mistral-nemo"],
};

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
  "default_model",
  "token_budget",
  "max_token_budget",
  "nexus_behavior_mode",
  "nexus_active_routing_pct",
  "provider_model_anthropic",
  "provider_model_openai",
  "provider_model_google",
  "provider_model_deepseek",
  "provider_model_mistral",
  "routing_chat_providers",
  "routing_fix_providers",
  "routing_refactor_providers",
  "routing_test_providers",
  "routing_docs_providers",
  "routing_architecture_providers",
]);

function normalizeProviderChain(values: string[]): ProviderName[] {
  const cleaned = values
    .map((value) => value.trim().toLowerCase())
    .filter((value): value is ProviderName => PROVIDERS.includes(value as ProviderName));
  const unique = cleaned.filter((value, index) => cleaned.indexOf(value) === index);
  // Aggiunge sempre i provider mancanti in coda (tutti e 5 sempre presenti)
  const result = unique.length > 0 ? unique : [];
  for (const p of PROVIDERS) {
    if (!result.includes(p)) result.push(p);
  }
  return result;
}

function parseProviderChain(value?: string): ProviderName[] {
  if (!value) return [...PROVIDERS];
  return normalizeProviderChain(value.split(","));
}

export function buildRoutingState(settings: SettingEntry[]): RoutingConfigState {
  const get = (key: string) => settings.find((setting) => setting.key === key)?.value ?? "";
  const providerHierarchy = parseProviderChain(get("provider_hierarchy") || get("default_provider"));

  return {
    providerHierarchy,
    providerModels: {
      // Fallback dal catalogo UI (PROVIDER_MODELS[0]), non da nomi hardcoded.
      // Il valore effettivo viene da nexus_provider_default_model in DB.
      anthropic: get("provider_model_anthropic") || PROVIDER_MODELS.anthropic[0],
      openai:    get("provider_model_openai")    || PROVIDER_MODELS.openai[0],
      google:    get("provider_model_google")    || PROVIDER_MODELS.google[0],
      deepseek:  get("provider_model_deepseek")  || PROVIDER_MODELS.deepseek[0],
      mistral:   get("provider_model_mistral")   || PROVIDER_MODELS.mistral[0],
    },
    intentChains: Object.fromEntries(
      ROUTING_INTENTS.map((intent) => [
        intent.key,
        parseProviderChain(get(`routing_${intent.key}_providers`) || providerHierarchy.join(",")),
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

export function labelProvider(provider: ProviderName): string {
  const labels: Record<ProviderName, string> = {
    anthropic: "Anthropic",
    openai:    "OpenAI",
    google:    "Google",
    deepseek:  "DeepSeek",
    mistral:   "Mistral",
  };
  return labels[provider] ?? provider;
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
