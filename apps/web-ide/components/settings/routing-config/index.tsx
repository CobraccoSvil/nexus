"use client";

import { useEffect, useState } from "react";
import { useTheme, useThemeColors } from "../../../lib/theme";
import { NexusMetricsPanel } from "../nexus-metrics-panel";
import { NexusWorkersPanel } from "../nexus-workers-panel";
import { listAdminPurposeModels, resolveInternalPurposeModel, updateAdminPurposeModel, type PurposeModelEntry } from "../../../lib/api-client";
import {
  API_BASE,
  MANAGED_ROUTING_KEYS,
  PROVIDERS,
  ROUTING_INTENTS,
  buildRoutingState,
  type BehaviorMode,
  type ProviderName,
  type RoutingConfigState,
  type SettingEntry,
} from "./shared";
import { BehaviorModeSection } from "./behavior-mode-section";
import { PurposeModelsSection } from "./purpose-models-section";
import { ManualConfigSection } from "./manual-config-section";
import { NexusActiveRoutingSection } from "./nexus-active-routing-section";

export type { SettingEntry, BehaviorMode, ProviderName, RoutingConfigState };
export { ROUTING_INTENTS, MANAGED_ROUTING_KEYS };

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
          pm[it.purpose] = {
            provider: prov,
            model_id: it.model_id,
            notes: it.notes ?? null,
            tier: it.tier ?? null,
            required_capability: it.required_capability ?? null,
            requires_tool_use: it.requires_tool_use ?? false,
          };
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
        tier: pm.tier ?? null,
        required_capability: pm.required_capability ?? null,
        requires_tool_use: pm.requires_tool_use ?? false,
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
          <BehaviorModeSection config={config} setConfig={setConfig} behaviorSaved={behaviorSaved} />

          {/* ── Purpose models (DB-driven) ── */}
          <PurposeModelsSection
            config={config}
            setConfig={setConfig}
            purposeLoading={purposeLoading}
            purposeSaved={purposeSaved}
            purposeError={purposeError}
            purposeSaving={purposeSaving}
            purposeTestBusy={purposeTestBusy}
            purposeTestMsg={purposeTestMsg}
            savePurposeModel={savePurposeModel}
            testPurposeModel={testPurposeModel}
          />

          {/* ── Configurazione manuale (visibile solo in modalità Manuale) ── */}
          {config.behaviorMode === "manuale" && (
            <ManualConfigSection config={config} setConfig={setConfig} />
          )}
        </div>
      </div>

      {/* I controlli sub-agent/parallelismo vivono ora in un unico pannello:
          AI & Prompt -> Orchestrator (/admin/orchestrator). Qui rimossi per
          evitare doppia configurazione delle stesse chiavi orchestrator.*. */}

      {/* Sezione Nexus Active Routing (Q-Learning A/B) */}
      <NexusActiveRoutingSection
        nexusRoutingPct={nexusRoutingPct}
        setNexusRoutingPct={setNexusRoutingPct}
        nexusPctSaving={nexusPctSaving}
        nexusPctSaved={nexusPctSaved}
        nexusPctError={nexusPctError}
        saveNexusPct={saveNexusPct}
      />

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
