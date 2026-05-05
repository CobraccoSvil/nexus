"use client";

import { useEffect, useState } from "react";
import { useTheme, useThemeColors } from "../../lib/theme";
import { NexusMetricsPanel } from "./nexus-metrics-panel";
import { NexusWorkersPanel } from "./nexus-workers-panel";
import { listAdminPurposeModels, resolveInternalPurposeModel, updateAdminPurposeModel, type PurposeModelEntry } from "../../lib/api-client";

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

export interface SettingEntry {
  key: string;
  value: string;
  category: string;
  description: string;
  is_secret: boolean;
  has_value: boolean;
  updated_at: string;
}

type ProviderName = "anthropic" | "openai" | "google" | "deepseek" | "mistral";

type BehaviorMode = "veloce" | "economica" | "bilanciata" | "approfondita" | "dinamico" | "manuale";

interface RoutingConfigState {
  providerHierarchy: ProviderName[];
  providerModels: Record<ProviderName, string>;
  intentChains: Record<string, ProviderName[]>;
  tokenBudget: string;
  maxTokenBudget: string;
  behaviorMode: BehaviorMode;
  purposeModels: Record<string, { provider: ProviderName; model_id: string; notes?: string | null }>;
}

const PROVIDERS: ProviderName[] = ["anthropic", "openai", "google", "deepseek", "mistral"];

const BEHAVIOR_MODES: { value: BehaviorMode; label: string; desc: string }[] = [
  { value: "veloce",       label: "⚡ Veloce",       desc: "Massima velocità — modello più rapido per tier richiesto" },
  { value: "economica",    label: "💰 Economica",    desc: "Minimo costo — modello più economico capace di gestire il task" },
  { value: "bilanciata",   label: "⚖️ Bilanciata",   desc: "Qualità/costo ottimale (default)" },
  { value: "approfondita", label: "🔬 Approfondita", desc: "Massima qualità — scala automaticamente il tier" },
  { value: "dinamico",     label: "🤖 Dinamico",     desc: "Consulta il catalogo modelli in tempo reale: sceglie il modello ottimale per capability, tier e costo corrente" },
  { value: "manuale",      label: "🔧 Manuale",      desc: "Configura manualmente provider, modelli e catene per intent" },
];

// Routing matrix frontend — specchio di orchestrator.rs, con dimensione token-complessità
interface MatrixEntry { provider: ProviderName; model: string; tokens?: string }

const NEXUS_ROUTING_MATRIX: Record<string, Array<{ label: string } & MatrixEntry>> = {
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


const PROVIDER_MODELS: Record<ProviderName, string[]> = {
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
  "agent_parallel_enabled",
  "agent_parallel_max",
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

function buildRoutingState(settings: SettingEntry[]): RoutingConfigState {
  const get = (key: string) => settings.find((setting) => setting.key === key)?.value ?? "";
  const providerHierarchy = parseProviderChain(get("provider_hierarchy") || get("default_provider"));

  return {
    providerHierarchy,
    providerModels: {
      anthropic: get("provider_model_anthropic") || "claude-sonnet-4-6",
      openai:    get("provider_model_openai")    || "gpt-4.1-mini",
      google:    get("provider_model_google")    || "gemini-2.5-flash",
      deepseek:  get("provider_model_deepseek")  || "deepseek-chat",
      mistral:   get("provider_model_mistral")   || "mistral-small-4",
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

function moveProvider(chain: ProviderName[], provider: ProviderName, direction: -1 | 1): ProviderName[] {
  const index = chain.indexOf(provider);
  const target = index + direction;
  if (index < 0 || target < 0 || target >= chain.length) return chain;
  const next = [...chain];
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

function labelProvider(provider: ProviderName): string {
  const labels: Record<ProviderName, string> = {
    anthropic: "Anthropic",
    openai:    "OpenAI",
    google:    "Google",
    deepseek:  "DeepSeek",
    mistral:   "Mistral",
  };
  return labels[provider] ?? provider;
}

function buttonStyle(tc: ReturnType<typeof useThemeColors>, disabled: boolean) {
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

function inputStyle(_tc: ReturnType<typeof useThemeColors>) {
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

interface RoutingConfigProps {
  settings: SettingEntry[];
  onSaveComplete: () => Promise<void>;
}

export function RoutingConfig({ settings, onSaveComplete }: RoutingConfigProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();
  const [config, setConfig] = useState<RoutingConfigState>(() => buildRoutingState(settings));
  const [purposeLoading, setPurposeLoading] = useState(false);
  const [purposeError, setPurposeError] = useState<string | null>(null);
  const [purposeSaved, setPurposeSaved] = useState(false);
  const [purposeSaving, setPurposeSaving] = useState<Record<string, boolean>>({});
  const [purposeTestBusy, setPurposeTestBusy] = useState<Record<string, boolean>>({});
  const [purposeTestMsg, setPurposeTestMsg] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const [behaviorSaved, setBehaviorSaved] = useState(false);
  const [parallelEnabled, setParallelEnabled] = useState<boolean>(
    () => settings.find((s) => s.key === "agent_parallel_enabled")?.value === "true"
  );
  const [parallelMax, setParallelMax] = useState<number>(
    () => parseInt(settings.find((s) => s.key === "agent_parallel_max")?.value ?? "3", 10) || 3
  );
  const [parallelSaving, setParallelSaving] = useState(false);
  const [parallelSaved, setParallelSaved] = useState(false);
  const [parallelError, setParallelError] = useState<string | null>(null);

  // Nexus Active Routing Percentage (Q-Learning A/B)
  const [nexusRoutingPct, setNexusRoutingPct] = useState<number>(
    () => Math.max(0, Math.min(100,
      parseInt(settings.find((s) => s.key === "nexus_active_routing_pct")?.value ?? "0", 10) || 0
    ))
  );
  const [nexusPctSaving, setNexusPctSaving] = useState(false);
  const [nexusPctSaved, setNexusPctSaved] = useState(false);
  const [nexusPctError, setNexusPctError] = useState<string | null>(null);

  useEffect(() => {
    setConfig(buildRoutingState(settings));
    const parallelEnabledValue =
      settings.find((s) => s.key === "agent_parallel_enabled")?.value ?? "false";
    const parsedParallelMax =
      parseInt(settings.find((s) => s.key === "agent_parallel_max")?.value ?? "3", 10) || 3;
    setParallelEnabled(parallelEnabledValue.trim().toLowerCase() === "true");
    setParallelMax(Math.max(1, Math.min(5, parsedParallelMax)));
    const parsedNexusPct =
      parseInt(settings.find((s) => s.key === "nexus_active_routing_pct")?.value ?? "0", 10) || 0;
    setNexusRoutingPct(Math.max(0, Math.min(100, parsedNexusPct)));
  }, [settings]);

  useEffect(() => {
    let active = true;
    setPurposeLoading(true);
    setPurposeError(null);
    listAdminPurposeModels()
      .then((res) => {
        if (!active) return;
        const pm: RoutingConfigState["purposeModels"] = {};
        for (const it of (res.items ?? []) as PurposeModelEntry[]) {
          const prov = it.provider as ProviderName;
          if (!PROVIDERS.includes(prov)) continue;
          pm[it.purpose] = { provider: prov, model_id: it.model_id, notes: it.notes ?? null };
        }
        setConfig((prev) => ({ ...prev, purposeModels: pm }));
      })
      .catch((e) => {
        if (!active) return;
        setPurposeError(e instanceof Error ? e.message : "Impossibile caricare purpose models");
      })
      .finally(() => {
        if (!active) return;
        setPurposeLoading(false);
      });
    return () => { active = false; };
  }, []);

  const saveParallelSettings = async () => {
    setParallelSaving(true);
    setParallelSaved(false);
    setParallelError(null);
    try {
      const res = await fetch(`${API_BASE}/api/admin/settings`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        credentials: "include",
        body: JSON.stringify({
          settings: [
            { key: "agent_parallel_enabled", value: parallelEnabled ? "true" : "false" },
            { key: "agent_parallel_max", value: String(Math.max(1, Math.min(5, parallelMax))) },
          ],
        }),
      });
      const payload = await res.json().catch(() => null);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      if (payload && payload.status && payload.status !== "ok") {
        const errors = Array.isArray(payload.errors) ? payload.errors.join(" | ") : "Errore salvataggio";
        throw new Error(errors);
      }
      setParallelSaved(true);
      setTimeout(() => setParallelSaved(false), 2000);
      await onSaveComplete();
    } catch (saveError) {
      setParallelError(saveError instanceof Error ? saveError.message : "Salvataggio non riuscito");
    } finally {
      setParallelSaving(false);
    }
  };

  const saveNexusPct = async () => {
    setNexusPctSaving(true);
    setNexusPctSaved(false);
    setNexusPctError(null);
    try {
      const res = await fetch(`${API_BASE}/api/admin/settings`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        credentials: "include",
        body: JSON.stringify({
          settings: [
            { key: "nexus_active_routing_pct", value: String(Math.max(0, Math.min(100, nexusRoutingPct))) },
          ],
        }),
      });
      const payload = await res.json().catch(() => null);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      if (payload && payload.status && payload.status !== "ok") {
        const errors = Array.isArray(payload.errors) ? payload.errors.join(" | ") : "Errore salvataggio";
        throw new Error(errors);
      }
      setNexusPctSaved(true);
      setTimeout(() => setNexusPctSaved(false), 2000);
      await onSaveComplete();
    } catch (saveError) {
      setNexusPctError(saveError instanceof Error ? saveError.message : "Salvataggio non riuscito");
    } finally {
      setNexusPctSaving(false);
    }
  };

  const saveRouting = async () => {
    setSaving(true);
    setError(null);
    setSaved(false);

    const primaryProvider = config.providerHierarchy[0] ?? "anthropic";
    const primaryModel = config.providerModels[primaryProvider];

    try {
      const res = await fetch(`${API_BASE}/api/admin/settings`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        credentials: "include",
        body: JSON.stringify({
          settings: [
            { key: "provider_hierarchy", value: config.providerHierarchy.join(",") },
            { key: "default_provider", value: primaryProvider },
            { key: "default_model", value: primaryModel },
            { key: "nexus_behavior_mode",       value: config.behaviorMode },
            { key: "provider_model_anthropic",  value: config.providerModels.anthropic },
            { key: "provider_model_openai",     value: config.providerModels.openai },
            { key: "provider_model_google",     value: config.providerModels.google },
            { key: "provider_model_deepseek",   value: config.providerModels.deepseek },
            { key: "provider_model_mistral",    value: config.providerModels.mistral },
            { key: "token_budget", value: config.tokenBudget },
            { key: "max_token_budget", value: config.maxTokenBudget },
            ...ROUTING_INTENTS.map((intent) => ({
              key: `routing_${intent.key}_providers`,
              value: config.intentChains[intent.key].join(","),
            })),
          ],
        }),
      });

      if (!res.ok) throw new Error(`HTTP ${res.status}`);

      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
      await onSaveComplete();
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "Save failed");
    } finally {
      setSaving(false);
    }
  };

  const extraSettings = settings.filter((setting) => !MANAGED_ROUTING_KEYS.has(setting.key));

  const PURPOSE_KEYS: Array<{ key: string; label: string; desc: string }> = [
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
  ];

  const savePurposeModel = async (purpose: string) => {
    const pm = config.purposeModels[purpose];
    if (!pm) return;
    setPurposeSaving((prev) => ({ ...prev, [purpose]: true }));
    setPurposeError(null);
    setPurposeSaved(false);
    try {
      await updateAdminPurposeModel(purpose, {
        provider: pm.provider,
        model_id: pm.model_id,
        notes: pm.notes ?? null,
      });
      setPurposeSaved(true);
      setTimeout(() => setPurposeSaved(false), 2000);
    } catch (e) {
      setPurposeError(e instanceof Error ? e.message : "Errore salvataggio purpose model");
    } finally {
      setPurposeSaving((prev) => ({ ...prev, [purpose]: false }));
    }
  };

  const testPurposeModel = async (purpose: string) => {
    setPurposeTestBusy((prev) => ({ ...prev, [purpose]: true }));
    setPurposeTestMsg((prev) => ({ ...prev, [purpose]: "" }));
    try {
      const res = await resolveInternalPurposeModel(purpose);
      setPurposeTestMsg((prev) => ({
        ...prev,
        [purpose]: `OK: ${res.provider}/${res.model} (${res.rationale || "purpose_model"})`,
      }));
    } catch (e) {
      setPurposeTestMsg((prev) => ({
        ...prev,
        [purpose]: `ERRORE: ${e instanceof Error ? e.message : "test fallito"}`,
      }));
    } finally {
      setPurposeTestBusy((prev) => ({ ...prev, [purpose]: false }));
    }
  };

  return (
    <div className="flex-col-gap-20">
      <div className="card">
        <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "flex-start", marginBottom: 16, gap: 16 }}>
          <div>
            <h2 className="text-xl font-bold" style={{ margin: 0 }}>Routing AI</h2>
            <p className="text-base text-muted" style={{ margin: "6px 0 0" }}>
              Imposta la gerarchia globale dei provider, i modelli preferiti e gli override per intent. La chat usera questa chain in runtime.
            </p>
          </div>
          <div className="flex-row-gap-8">
            {saved && <span className="text-sm font-semibold" style={{ color: tc.success }}>Salvato</span>}
            <button
              onClick={saveRouting}
              disabled={saving}
              className="btn btn-primary"
              style={{
                background: saving ? tc.bgInput : tc.accent,
                color: "#fff",
              }}
            >
              {saving ? "Salvataggio..." : "Salva routing"}
            </button>
          </div>
        </div>

        {error && (
          <div
            className="text-base"
            style={{
              padding: "10px 14px",
              marginBottom: 16,
              borderRadius: 8,
              border: `1px solid ${tc.error}`,
              background: resolved === "dark" ? "#2d1215" : "#fef2f2",
              color: "var(--color-error)",
            }}>

            {error}
          </div>
        )}

        <div style={{ display: "grid", gap: 16 }}>
          {/* ── Modalità Nexus ── */}
          <div className="card-sm" style={{ background: tc.bgHover }}>
            <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
              <div>
                <div className="text-base font-bold">Modalità Nexus</div>
                <div className="text-sm text-muted" style={{ marginTop: 3 }}>
                  {BEHAVIOR_MODES.find((m) => m.value === config.behaviorMode)?.desc}
                </div>
              </div>
              {behaviorSaved && <span className="text-sm font-semibold" style={{ color: tc.success }}>Salvato ✓</span>}
            </div>
            <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
              {BEHAVIOR_MODES.map((mode) => (
                <button
                  key={mode.value}
                  onClick={() => setConfig((c) => ({ ...c, behaviorMode: mode.value }))}
                  title={mode.desc}
                  className={`btn ${config.behaviorMode === mode.value ? "btn-primary" : "btn-secondary"}`}
                  style={{
                    border: `1px solid ${config.behaviorMode === mode.value ? tc.accent : tc.border}`,
                    background: config.behaviorMode === mode.value ? tc.accent : tc.bgInput,
                    color: config.behaviorMode === mode.value ? "#fff" : tc.text,
                  }}
                >
                  {mode.label}
                </button>
              ))}
            </div>

            {/* Preview per modalità con matrice statica */}
            {NEXUS_ROUTING_MATRIX[config.behaviorMode] && (
              <div style={{ marginTop: 14 }}>
                <div style={{ fontSize: 12, fontWeight: 600, color: "var(--color-textSecondary)", marginBottom: 8 }}>
                  Routing per questa modalità — scala automaticamente in base a intent e complessità:
                </div>
                <div style={{ display: "grid", gap: 3 }}>
                  {NEXUS_ROUTING_MATRIX[config.behaviorMode].map((entry, i) => (
                    <div key={i} style={{
                      display: "grid",
                      gridTemplateColumns: "130px 1fr auto",
                      gap: 8,
                      alignItems: "center",
                      padding: "5px 8px",
                      borderRadius: 6,
                      background: "var(--color-bgInput)",
                    }}>
                      <span style={{ fontSize: 11, color: "var(--color-textMuted)", fontWeight: 600 }}>{entry.label}</span>
                      <span style={{ fontSize: 12 }}>
                        <span style={{ color: "var(--color-accent)", fontWeight: 600 }}>{labelProvider(entry.provider)}</span>
                        <span style={{ color: "var(--color-textMuted)" }}> / </span>
                        <span style={{ color: "var(--color-text)", fontFamily: "monospace", fontSize: 11 }}>{entry.model}</span>
                      </span>
                      {entry.tokens && (
                        <span style={{ fontSize: 10, color: "var(--color-textMuted)", whiteSpace: "nowrap" }}>{entry.tokens}</span>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            )}

            {/* Preview per modalità Dinamico */}
            {config.behaviorMode === "dinamico" && (
              <div style={{
                marginTop: 14, padding: "10px 12px", borderRadius: 8,
                background: "var(--color-bgInput)", border: "1px solid var(--color-border)",
              }}>
                <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 6 }}>🤖 Come funziona il routing Dinamico</div>
                <div style={{ fontSize: 12, color: "var(--color-textMuted)", lineHeight: 1.6 }}>
                  Per ogni messaggio l&apos;orchestratore analizza:
                  <br />① <strong>Intent</strong> (fix / chat / architettura…) e <strong>complessità</strong> (token stimati)
                  <br />② Determina il <strong>tier richiesto</strong> (light / medium / heavy) e la <strong>capability</strong> necessaria
                  <br />③ Interroga il <strong>catalogo modelli</strong> ordinando per costo — sceglie il più economico che soddisfa i requisiti
                  <br />④ Se il catalogo è vuoto, usa la matrice Bilanciata come fallback
                </div>
              </div>
            )}
          </div>

          {/* ── Purpose models (DB-driven) ── */}
          <div className="card-sm" style={{ background: tc.bgHover }}>
            <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
              <div>
                <div className="text-base font-bold">Purpose models</div>
                <div className="text-sm text-muted" style={{ marginTop: 3 }}>
                  Modelli per task interni (non routing utente). Includono il fallback automatico su loop tool-use.
                </div>
              </div>
              {purposeLoading && <span className="text-sm text-muted">Caricamento…</span>}
              {purposeSaved && <span className="text-sm font-semibold" style={{ color: tc.success }}>Salvato ✓</span>}
            </div>
            {purposeError && (
              <div style={{
                padding: "8px 10px",
                borderRadius: 8,
                border: `1px solid ${tc.error}`,
                background: resolved === "dark" ? "#2d1215" : "#fef2f2",
                color: "var(--color-error)",
                marginBottom: 10,
                fontSize: 12,
              }}>
                {purposeError}
              </div>
            )}
            <div style={{ display: "grid", gap: 10 }}>
              {PURPOSE_KEYS.map((p) => {
                const pm = config.purposeModels[p.key] ?? { provider: "anthropic" as ProviderName, model_id: PROVIDER_MODELS.anthropic[0], notes: null };
                const savingThis = !!purposeSaving[p.key];
                const testBusy = !!purposeTestBusy[p.key];
                const testMsg = purposeTestMsg[p.key];
                return (
                  <div key={p.key} style={{
                    display: "grid",
                    gridTemplateColumns: "170px 160px 1fr auto auto",
                    gap: 10,
                    alignItems: "center",
                    padding: "8px 10px",
                    borderRadius: 8,
                    border: `1px solid ${tc.border}`,
                    background: "var(--color-bgInput)",
                  }}>
                    <div style={{ minWidth: 0 }}>
                      <div style={{ fontSize: 12, fontWeight: 700, color: tc.text, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {p.label}
                      </div>
                      <div style={{ fontSize: 11, color: tc.textMuted, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {p.desc}
                      </div>
                      <div style={{ fontSize: 10, color: tc.textMuted, marginTop: 2, fontFamily: "monospace" }}>{p.key}</div>
                      {!!testMsg && (
                        <div style={{ marginTop: 4, fontSize: 10, color: testMsg.startsWith("OK:") ? tc.success : tc.error }}>
                          {testMsg}
                        </div>
                      )}
                    </div>

                    <select
                      value={pm.provider}
                      onChange={(e) => {
                        const provider = e.target.value as ProviderName;
                        const firstModel = PROVIDER_MODELS[provider]?.[0] ?? "";
                        setConfig((c) => ({
                          ...c,
                          purposeModels: {
                            ...c.purposeModels,
                            [p.key]: { ...pm, provider, model_id: firstModel || pm.model_id },
                          },
                        }));
                      }}
                      style={{ ...inputStyle(tc), padding: "6px 8px", fontSize: 12 }}
                    >
                      {PROVIDERS.map((prov) => (
                        <option key={prov} value={prov}>{labelProvider(prov)}</option>
                      ))}
                    </select>

                    <select
                      value={pm.model_id}
                      onChange={(e) => {
                        const model_id = e.target.value;
                        setConfig((c) => ({
                          ...c,
                          purposeModels: { ...c.purposeModels, [p.key]: { ...pm, model_id } },
                        }));
                      }}
                      style={{ ...inputStyle(tc), padding: "6px 8px", fontSize: 12 }}
                    >
                      {(PROVIDER_MODELS[pm.provider] ?? []).map((m) => (
                        <option key={m} value={m}>{m}</option>
                      ))}
                    </select>

                    <button
                      onClick={() => void savePurposeModel(p.key)}
                      disabled={savingThis}
                      className="btn btn-secondary"
                      style={{
                        padding: "6px 10px",
                        background: savingThis ? tc.bgInput : tc.bgCard,
                        color: tc.text,
                        border: `1px solid ${tc.border}`,
                        borderRadius: 8,
                        cursor: savingThis ? "wait" : "pointer",
                        fontSize: 12,
                        whiteSpace: "nowrap",
                      }}
                      title="Salva questo purpose model"
                    >
                      {savingThis ? "…" : "Salva"}
                    </button>

                    {p.key === "loop_fallback_default" ? (
                      <button
                        onClick={() => void testPurposeModel(p.key)}
                        disabled={testBusy}
                        className="btn btn-secondary"
                        style={{
                          padding: "6px 10px",
                          background: testBusy ? tc.bgInput : tc.bgCard,
                          color: tc.text,
                          border: `1px solid ${tc.border}`,
                          borderRadius: 8,
                          cursor: testBusy ? "wait" : "pointer",
                          fontSize: 12,
                          whiteSpace: "nowrap",
                        }}
                        title="Testa la risoluzione runtime del fallback su loop"
                      >
                        {testBusy ? "…" : "Test"}
                      </button>
                    ) : (
                      <div />
                    )}
                  </div>
                );
              })}
            </div>
          </div>

          {/* ── Configurazione manuale (visibile solo in modalità Manuale) ── */}
          {config.behaviorMode === "manuale" && <>
          <div className="card-sm" style={{ background: tc.bgHover }}>
            <div className="text-base font-bold" style={{ marginBottom: 6 }}>Gerarchia globale provider</div>
            <div className="text-sm text-muted" style={{ marginBottom: 12 }}>
              Il primo provider pronto viene usato per la chat. Se fallisce, l'orchestratore prova il successivo.
            </div>
            <div className="flex-col-gap-8">
              {config.providerHierarchy.map((provider, index) => (
                <div
                  key={provider}
                  className="flex-row"
                  style={{
                    justifyContent: "space-between",
                    gap: 12,
                    padding: "10px 12px",
                    borderRadius: 8,
                    background: "var(--color-bgInput)",
                    border: "1px solid var(--color-border)",
                  }}
                >
                  <div>
                    <div style={{ fontSize: 13, fontWeight: 700, color: tc.text }}>
                      {index + 1}. {labelProvider(provider)}
                    </div>
                    <div style={{ fontSize: 12, color: "var(--color-textMuted)" }}>{config.providerModels[provider]}</div>
                  </div>
                  <div style={{ display: "flex", gap: 6 }}>
                    <button
                      onClick={() => setConfig((current) => ({ ...current, providerHierarchy: moveProvider(current.providerHierarchy, provider, -1) }))}
                      disabled={index === 0}
                      style={buttonStyle(tc, index === 0)}
                    >
                      Su
                    </button>
                    <button
                      onClick={() => setConfig((current) => ({ ...current, providerHierarchy: moveProvider(current.providerHierarchy, provider, 1) }))}
                      disabled={index === config.providerHierarchy.length - 1}
                      style={buttonStyle(tc, index === config.providerHierarchy.length - 1)}
                    >
                      Giu
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>

          <div
            style={{
              display: "grid",
              gap: 12,
              gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))",
            }}
          >
            {PROVIDERS.map((provider) => (
              <div
                key={provider}
                className="card-sm"
                style={{
                  background: "var(--color-bgHover)",
                }}
              >
                <div className="text-base font-bold" style={{ marginBottom: 10 }}>{labelProvider(provider)} model</div>
                <select
                  value={config.providerModels[provider]}
                  onChange={(event) =>
                    setConfig((current) => ({
                      ...current,
                      providerModels: {
                        ...current.providerModels,
                        [provider]: event.target.value,
                      },
                    }))
                  }
                  style={{ ...inputStyle(tc), cursor: "pointer" }}
                >
                  <option value="">— auto (routing Nexus) —</option>
                  {PROVIDER_MODELS[provider].map((m) => (
                    <option key={m} value={m}>{m}</option>
                  ))}
                </select>
              </div>
            ))}
          </div>

          <div className="card-sm" style={{ background: tc.bgHover }}>
            <div className="text-base font-bold" style={{ marginBottom: 6 }}>Override per intent</div>
            <div className="text-sm text-muted" style={{ marginBottom: 12 }}>
              Ogni intent puo avere una catena diversa. Se lasci la stessa sequenza della gerarchia globale, il comportamento resta uniforme.
            </div>
            <div style={{ display: "grid", gap: 12 }}>
              {ROUTING_INTENTS.map((intent) => (
                <div
                  key={intent.key}
                  style={{
                    padding: 12,
                    borderRadius: 8,
                    border: "1px solid var(--color-border)",
                    background: "var(--color-bgInput)",
                  }}
                >
                  <div className="flex-row" style={{ justifyContent: "space-between", gap: 12, marginBottom: 10 }}>
                    <div className="text-base font-bold">{intent.label}</div>
                    <button
                      onClick={() =>
                        setConfig((current) => ({
                          ...current,
                          intentChains: {
                            ...current.intentChains,
                            [intent.key]: [...current.providerHierarchy],
                          },
                        }))
                      }
                      style={buttonStyle(tc, false)}
                    >
                      Allinea al globale
                    </button>
                  </div>
                  <div className="flex-col-gap-8">
                    {config.intentChains[intent.key].map((provider, index) => (
                      <div
                        key={`${intent.key}-${provider}`}
                        className="flex-row"
                        style={{
                          justifyContent: "space-between",
                          gap: 12,
                          padding: "8px 10px",
                          borderRadius: 8,
                          border: "1px solid var(--color-border)",
                          background: "var(--color-bgCard)",
                        }}
                      >
                        <span style={{ fontSize: 12, fontWeight: 600 }}>
                          {index + 1}. {labelProvider(provider)}
                        </span>
                        <div style={{ display: "flex", gap: 6 }}>
                          <button
                            onClick={() =>
                              setConfig((current) => ({
                                ...current,
                                intentChains: {
                                  ...current.intentChains,
                                  [intent.key]: moveProvider(current.intentChains[intent.key], provider, -1),
                                },
                              }))
                            }
                            disabled={index === 0}
                            style={buttonStyle(tc, index === 0)}
                          >
                            Su
                          </button>
                          <button
                            onClick={() =>
                              setConfig((current) => ({
                                ...current,
                                intentChains: {
                                  ...current.intentChains,
                                  [intent.key]: moveProvider(current.intentChains[intent.key], provider, 1),
                                },
                              }))
                            }
                            disabled={index === config.intentChains[intent.key].length - 1}
                            style={buttonStyle(tc, index === config.intentChains[intent.key].length - 1)}
                          >
                            Giu
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          </div>
          </>}
        </div>
      </div>

      {/* Sezione Agenti Paralleli */}
      <div className="card">
        <div className="flex-row" style={{ gap: 8, marginBottom: 4 }}>
          <span className="text-xl font-bold">⚡ Agenti Paralleli</span>
          {parallelEnabled && (
            <span style={{
              fontSize: 10,
              fontWeight: 700,
              background: "#22c55e20",
              color: "#22c55e",
              border: "1px solid #22c55e40",
              borderRadius: 6,
              padding: "1px 7px",
            }}>
              ATTIVO
            </span>
          )}
        </div>
        <div className="text-sm text-muted" style={{ marginBottom: 14 }}>
          Permette all&apos;orchestratore di lanciare più agenti contemporaneamente per accelerare task complessi.
          L&apos;agente può usare il tool <code style={{ fontFamily: "monospace", fontSize: 11 }}>dispatch_subtask</code> per delegare sotto-task in parallelo.
        </div>

        <div className="flex-row" style={{ gap: 12, marginBottom: 12 }}>
          {/* Toggle */}
          <button
            onClick={() => setParallelEnabled((v) => !v)}
            style={{
              width: 44,
              height: 24,
              borderRadius: 12,
              border: "none",
              background: parallelEnabled ? tc.accent : tc.border,
              cursor: "pointer",
              position: "relative",
              transition: "background 0.2s",
              flexShrink: 0,
            }}
          >
            <span style={{
              position: "absolute",
              top: 3,
              left: parallelEnabled ? 22 : 3,
              width: 18,
              height: 18,
              borderRadius: "50%",
              background: "#fff",
              transition: "left 0.2s",
              boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
            }} />
          </button>
          <span className="text-base" style={{ color: tc.text }}>
            {parallelEnabled ? "Abilitato" : "Disabilitato"}
          </span>
        </div>

        {parallelEnabled && (
          <div className="flex-row" style={{ gap: 10, marginBottom: 12 }}>
            <label className="text-sm" style={{ color: "var(--color-textSecondary)", minWidth: 160 }}>
              Max agenti paralleli (1–5):
            </label>
            <input
              type="number"
              min={1}
              max={5}
              value={parallelMax}
              onChange={(e) => setParallelMax(parseInt(e.target.value, 10) || 1)}
              style={{
                width: 60,
                padding: "4px 8px",
                borderRadius: 6,
                border: "1px solid var(--color-border)",
                background: "var(--color-bgInput)",
                color: "var(--color-text)",
                fontSize: 13,
                textAlign: "center",
              }}
            />
          </div>
        )}

        <button
          onClick={saveParallelSettings}
          disabled={parallelSaving}
          style={{
            padding: "6px 18px",
            borderRadius: 8,
            border: "none",
            background: parallelSaved ? "#22c55e" : tc.accent,
            color: "#fff",
            fontWeight: 600,
            fontSize: 13,
            cursor: parallelSaving ? "wait" : "pointer",
            opacity: parallelSaving ? 0.7 : 1,
          }}
        >
          {parallelSaving ? "Salvo..." : parallelSaved ? "✓ Salvato" : "Salva impostazioni agenti"}
        </button>
        {parallelError && (
          <div
            style={{
              marginTop: 10,
              padding: "8px 10px",
              borderRadius: 8,
              border: `1px solid ${tc.error}`,
              background: resolved === "dark" ? "#2d1215" : "#fef2f2",
              color: "var(--color-error)",
              fontSize: 12,
            }}
          >
            {parallelError}
          </div>
        )}
      </div>

      {/* Sezione Nexus Active Routing (Q-Learning A/B) */}
      <div className="card">
        <div className="flex-row" style={{ gap: 8, marginBottom: 4 }}>
          <span className="text-xl font-bold">🧠 Nexus Active Routing (Q-Learning)</span>
          {nexusRoutingPct > 0 && (
            <span style={{
              fontSize: 10,
              fontWeight: 700,
              background: "#6366f120",
              color: "#6366f1",
              border: "1px solid #6366f140",
              borderRadius: 6,
              padding: "1px 7px",
            }}>
              {nexusRoutingPct}% ATTIVO
            </span>
          )}
        </div>
        <div className="text-sm text-muted" style={{ marginBottom: 14 }}>
          Percentuale di richieste chat gestite dal router Q-Learning di Nexus anziché dal routing classico.
          Il router seleziona automaticamente l&apos;agent type ottimale (Coder, Tester, Architect…)
          basandosi su 60+ tipi di agente e dati storici di performance.
          Imposta 0% per disabilitare, 100% per routing Q-Learning completo.
        </div>

        {/* Preset rapidi */}
        <div className="flex-row" style={{ gap: 6, marginBottom: 12, flexWrap: "wrap" }}>
          {[0, 10, 25, 50, 75, 100].map((pct) => (
            <button
              key={pct}
              onClick={() => setNexusRoutingPct(pct)}
              style={{
                padding: "3px 12px",
                borderRadius: 6,
                border: `1px solid ${nexusRoutingPct === pct ? "#6366f1" : tc.border}`,
                background: nexusRoutingPct === pct ? "#6366f120" : tc.bgInput,
                color: nexusRoutingPct === pct ? "#6366f1" : tc.textSecondary,
                fontSize: 11,
                cursor: "pointer",
                fontWeight: nexusRoutingPct === pct ? 700 : 400,
              }}
            >
              {pct === 0 ? "Off" : `${pct}%`}
            </button>
          ))}
        </div>

        {/* Slider + valore numerico */}
        <div className="flex-row" style={{ gap: 12, marginBottom: 14 }}>
          <input
            type="range"
            min={0}
            max={100}
            step={5}
            value={nexusRoutingPct}
            onChange={(e) => setNexusRoutingPct(parseInt(e.target.value, 10))}
            style={{ flex: 1, accentColor: "#6366f1", cursor: "pointer" }}
          />
          <span style={{ fontSize: 13, fontWeight: 700, color: "#6366f1", minWidth: 36, textAlign: "right" }}>
            {nexusRoutingPct}%
          </span>
          <input
            type="number"
            min={0}
            max={100}
            value={nexusRoutingPct}
            onChange={(e) => setNexusRoutingPct(Math.max(0, Math.min(100, parseInt(e.target.value, 10) || 0)))}
            style={{
              width: 64,
              padding: "4px 8px",
              borderRadius: 6,
              border: "1px solid var(--color-border)",
              background: "var(--color-bgInput)",
              color: "var(--color-text)",
              fontSize: 13,
              textAlign: "center",
            }}
          />
        </div>

        <button
          onClick={saveNexusPct}
          disabled={nexusPctSaving}
          style={{
            padding: "6px 18px",
            borderRadius: 8,
            border: "none",
            background: nexusPctSaved ? "#22c55e" : "#6366f1",
            color: "#fff",
            fontWeight: 600,
            fontSize: 13,
            cursor: nexusPctSaving ? "wait" : "pointer",
            opacity: nexusPctSaving ? 0.7 : 1,
          }}
        >
          {nexusPctSaving ? "Salvo..." : nexusPctSaved ? "✓ Salvato" : "Salva routing Nexus"}
        </button>
        {nexusPctError && (
          <div
            style={{
              marginTop: 10,
              padding: "8px 10px",
              borderRadius: 8,
              border: `1px solid ${tc.error}`,
              background: resolved === "dark" ? "#2d1215" : "#fef2f2",
              color: "var(--color-error)",
              fontSize: 12,
            }}
          >
            {nexusPctError}
          </div>
        )}
      </div>

      {/* Nexus Router — Metriche Live */}
      <NexusMetricsPanel />

      {/* Nexus Learning Workers — Status */}
      <NexusWorkersPanel />

      {extraSettings.length > 0 && (
        <div className="card">
          <div className="text-xl font-bold" style={{ marginBottom: 6 }}>Routing avanzato</div>
          <div className="text-sm text-muted" style={{ marginBottom: 12 }}>
            Qui restano visibili eventuali chiavi extra non gestite dal pannello principale.
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {extraSettings.map((setting) => (
              <div key={setting.key} style={{ fontSize: 12, color: tc.text }}>
                <strong>{setting.key}</strong>: {setting.value || <span style={{ color: "var(--color-textMuted)" }}>vuoto</span>}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
