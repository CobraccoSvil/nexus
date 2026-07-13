"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import {
  getProviderRegistry,
  getModels,
  type ProviderRegistryEntry,
} from "../../lib/api/models";
import {
  ProviderSettings,
  GatewayStatusBanner,
  type ProviderSettingsProps,
  type SettingEntry,
} from "./provider-settings";
import { useProviderBudgets, ProviderBudgetRow } from "./provider-budget";

// Etichetta leggibile di un provider (fallback: capitalizza il nome grezzo).
function labelProvider(name: string): string {
  const labels: Record<string, string> = {
    anthropic: "Anthropic", openai: "OpenAI", google: "Google", deepseek: "DeepSeek",
    mistral: "Mistral", groq: "Groq", openrouter: "OpenRouter", perplexity: "Perplexity",
    vllm: "vLLM", ollama: "Ollama",
  };
  return labels[name] ?? (name.charAt(0).toUpperCase() + name.slice(1));
}

interface Partition {
  byProvider: Record<string, SettingEntry[]>;
  advanced: SettingEntry[];
  other: SettingEntry[];
}

/**
 * Ripartisce le chiavi della categoria providers (funzione pura, testabile):
 * - a un provider `p` appartengono key_setting/enabled_setting/base_url_setting del
 *   registry e ogni chiave `{name}_*` (copre google_vertex_*, google_batch_*, ...).
 *   I nomi provider sono disgiunti sul separatore "_", quindi nessuna collisione.
 * - `provider.*` / `providers.*` -> impostazioni avanzate globali (cooldown, timeout...).
 * - il resto (es. ollama_url) -> "altri setting".
 */
export function partitionProviderItems(items: SettingEntry[], registry: ProviderRegistryEntry[]): Partition {
  const byProvider: Record<string, SettingEntry[]> = {};
  for (const p of registry) byProvider[p.name] = [];
  const advanced: SettingEntry[] = [];
  const other: SettingEntry[] = [];

  const exactOwner = new Map<string, string>(); // key esatta -> provider
  for (const p of registry) {
    if (p.keySetting) exactOwner.set(p.keySetting, p.name);
    if (p.enabledSetting) exactOwner.set(p.enabledSetting, p.name);
    if (p.baseUrlSetting) exactOwner.set(p.baseUrlSetting, p.name);
  }

  for (const item of items) {
    const exact = exactOwner.get(item.key);
    if (exact) {
      byProvider[exact].push(item);
      continue;
    }
    if (item.key.startsWith("provider.") || item.key.startsWith("providers.")) {
      advanced.push(item);
      continue;
    }
    const prefixOwner = registry.find((p) => item.key.startsWith(`${p.name}_`));
    if (prefixOwner) {
      byProvider[prefixOwner.name].push(item);
      continue;
    }
    other.push(item);
  }
  return { byProvider, advanced, other };
}

function CollapsibleSection({
  title,
  subtitle,
  children,
  defaultOpen = false,
}: {
  title: string;
  subtitle?: string;
  children: React.ReactNode;
  defaultOpen?: boolean;
}) {
  const tc = useThemeColors();
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div style={{ marginTop: 16, border: `1px solid ${tc.border}`, borderRadius: 8, overflow: "hidden" }}>
      <button
        onClick={() => setOpen((v) => !v)}
        style={{
          width: "100%", display: "flex", alignItems: "center", gap: 8, padding: "10px 14px",
          border: "none", background: tc.bgCard, color: tc.text, cursor: "pointer",
          fontFamily: "inherit", textAlign: "left",
        }}
      >
        <span style={{ transition: "transform 0.15s", transform: open ? "rotate(90deg)" : "none" }}>&#9656;</span>
        <span style={{ fontWeight: 600, fontSize: 14 }}>{title}</span>
        {subtitle && <span style={{ fontSize: 12, color: tc.textMuted }}>{subtitle}</span>}
      </button>
      {open && <div style={{ padding: 14 }}>{children}</div>}
    </div>
  );
}

/**
 * Pannello provider a card omogenee per-provider: una card per provider che
 * aggrega API key, base_url, toggle, LED/Testa, modelli e budget; una sezione
 * collassata per le impostazioni avanzate globali (provider.* e providers.* ).
 * Fetch registry/catalog/budget UNA volta e passa gli override alle istanze
 * ProviderSettings (nessun fetch duplicato).
 */
export function ProvidersOverview(props: ProviderSettingsProps) {
  const tc = useThemeColors();
  const { items, gatewayProviders } = props;
  const [registry, setRegistry] = useState<ProviderRegistryEntry[]>([]);
  const [catalog, setCatalog] = useState<Array<{ provider: string; model: string }>>([]);
  const budgets = useProviderBudgets();

  useEffect(() => {
    let active = true;
    Promise.all([
      getProviderRegistry().catch(() => ({ providers: [] })),
      getModels().catch(() => ({ models: [] })),
    ]).then(([reg, cat]) => {
      if (!active) return;
      setRegistry((reg.providers ?? []).filter((p) => p.isActive));
      setCatalog((cat.models ?? []).map((m) => ({ provider: m.provider, model: m.model })));
    });
    return () => { active = false; };
  }, []);

  const { byProvider, advanced, other } = partitionProviderItems(items, registry);
  // Passthrough delle props a ogni istanza, con override per evitare fetch duplicati.
  const shared = { ...props, hideGatewayBanner: true, catalogOverride: catalog, registryOverride: registry };

  return (
    <>
      <GatewayStatusBanner providers={gatewayProviders} />

      <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
        {registry.map((p) => {
          const providerItems = byProvider[p.name] ?? [];
          const budget = budgets.byProvider[p.name];
          return (
            <div
              key={p.name}
              style={{
                border: `1px solid ${tc.border}`,
                borderRadius: 10,
                padding: 16,
                background: tc.bgCard,
              }}
            >
              <div style={{ fontSize: 16, fontWeight: 700, color: tc.text, marginBottom: 10 }}>
                {labelProvider(p.name)}
              </div>
              <ProviderSettings {...shared} items={providerItems} />
              {budget && (
                <ProviderBudgetRow
                  entry={budget}
                  editing={budgets.editing[p.name]}
                  busy={!!budgets.busy[p.name]}
                  setEditing={budgets.setEditing}
                  onSetBudget={budgets.setBudget}
                  onRecharge={budgets.recharge}
                  compact
                />
              )}
            </div>
          );
        })}
      </div>

      {budgets.error && (
        <div style={{ marginTop: 10, fontSize: 12, color: tc.error }}>Budget: {budgets.error}</div>
      )}

      <CollapsibleSection
        title="Impostazioni avanzate (globali)"
        subtitle={`${advanced.length} chiavi — cooldown, circuit breaker, timeout, cache`}
      >
        <ProviderSettings {...shared} items={advanced} />
      </CollapsibleSection>

      {other.length > 0 && (
        <CollapsibleSection title="Altri setting" subtitle={`${other.length} chiavi`}>
          <ProviderSettings {...shared} items={other} />
        </CollapsibleSection>
      )}
    </>
  );
}
