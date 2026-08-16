"use client";

import { useEffect, useState } from "react";
import { useTheme, useThemeColors } from "../../../lib/theme";
import { NexusMetricsPanel } from "../nexus-metrics-panel";
import { NexusWorkersPanel } from "../nexus-workers-panel";
import { listAdminPurposeModels, resolveInternalPurposeModel, updateAdminPurposeModel, getProviderRegistry, type PurposeModelEntry } from "../../../lib/api-client";
import {
  API_BASE,
  FALLBACK_PROVIDERS,
  MANAGED_ROUTING_KEYS,
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
import { useI18n } from "../../../lib/i18n";

export type { SettingEntry, BehaviorMode, ProviderName, RoutingConfigState };
export { ROUTING_INTENTS, MANAGED_ROUTING_KEYS };

interface RoutingConfigProps {
  settings: SettingEntry[];
  onSaveComplete: () => Promise<void>;
}

export function RoutingConfig({ settings, onSaveComplete }: RoutingConfigProps) {
  const { t } = useI18n();
  const tc = useThemeColors();
  const { resolved } = useTheme();
  // Provider dal registry (fonte unica, regola G): niente piu' lista
  // hardcoded a 5. Fallback ai noti finche' il fetch non completa.
  const [providers, setProviders] = useState<string[]>(FALLBACK_PROVIDERS);
  const [providersError, setProvidersError] = useState<string | null>(null);
  const [config, setConfig] = useState<RoutingConfigState>(() => buildRoutingState(settings, FALLBACK_PROVIDERS));
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
    setConfig(buildRoutingState(settings, providers));
    const parsedNexusPct =
      parseInt(settings.find((s) => s.key === "nexus_active_routing_pct")?.value ?? "0", 10) || 0;
    setNexusRoutingPct(Math.max(0, Math.min(100, parsedNexusPct)));
  }, [settings, providers]);

  // Carica la lista provider (registry, is_active per sortOrder, incluso vllm
  // che partecipa legittimamente alle catene). I modelli per provider non
  // servono piu' qui: il pannello purpose e' tier-only (mig 0723).
  useEffect(() => {
    let active = true;
    getProviderRegistry()
      .then((reg) => {
        if (!active) return;
        const names = (reg.providers ?? []).filter((p) => p.isActive).map((p) => p.name);
        if (names.length > 0) setProviders(names);
        setProvidersError(null);
      })
      .catch((e: unknown) => {
        // L'errore era inghiottito da due `.catch(() => vuoto)`: la pagina
        // restava sui cinque provider storici di FALLBACK_PROVIDERS e non
        // mostrava groq, openrouter o vertex, senza che nulla lo segnalasse.
        // Chi configurava il routing concludeva che quei provider non ci fossero.
        if (!active) return;
        setProvidersError(
          e instanceof Error ? e.message : "Registry provider non raggiungibile",
        );
      });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    let active = true;
    setPurposeLoading(true);
    setPurposeError(null);
    listAdminPurposeModels()
      .then((res) => {
        if (!active) return;
        const pm: RoutingConfigState["purposeModels"] = {};
        for (const it of (res.items ?? []) as PurposeModelEntry[]) {
          // Tier-only (mig 0723): niente piu' provider/model_id statici. Cio'
          // che risponde davvero lo dice `resolved`, chiesto al resolver.
          pm[it.purpose] = {
            notes: it.notes ?? null,
            tier: it.tier ?? null,
            required_capability: it.required_capability ?? null,
            requires_tool_use: it.requires_tool_use ?? false,
            resolved: it.resolved ?? null,
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
      // L'esito e' lo status HTTP (il backend risponde 500 se anche una sola
      // chiave e' stata rifiutata); il body serve solo a dire QUALE, per il
      // display. Prima il 200-sempre rendeva il controllo su payload.status
      // l'unico presidio, e il gemello saveRouting non ce l'aveva affatto.
      if (!res.ok) {
        throw new Error(payload?.error ?? `HTTP ${res.status}`);
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

    try {
      const res = await fetch(`${API_BASE}/api/admin/settings`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        credentials: "include",
        body: JSON.stringify({
          settings: [
            { key: "provider_hierarchy", value: config.providerHierarchy.join(",") },
            { key: "default_provider", value: primaryProvider },
            { key: "nexus_behavior_mode",       value: config.behaviorMode },
            { key: "token_budget", value: config.tokenBudget },
            { key: "max_token_budget", value: config.maxTokenBudget },
            ...ROUTING_INTENTS.map((intent) => ({
              key: `routing_${intent.key}_providers`,
              value: config.intentChains[intent.key].join(","),
            })),
          ],
        }),
      });

      // Difetto corretto: qui `res.ok` era l'unico controllo, e bulk_update
      // rispondeva 200 anche quando il DB rifiutava ogni chiave (l'esito viveva
      // solo in payload.status/errors, che questa funzione non leggeva): il
      // pannello mostrava "Salvato" con il DB invariato.
      if (!res.ok) {
        const payload = await res.json().catch(() => null);
        throw new Error(payload?.error ?? `HTTP ${res.status}`);
      }

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
    // Tier obbligatorio (mig 0723): senza pin statico un purpose senza tier
    // non risolve nulla, e il backend lo rifiuta con 400.
    if (!pm.tier) {
      setPurposeError(`Il purpose '${purpose}' richiede un tier: scegli la fascia prima di salvare.`);
      return;
    }
    setPurposeSaving((prev) => ({ ...prev, [purpose]: true }));
    setPurposeError(null);
    setPurposeSaved(false);
    try {
      await updateAdminPurposeModel(purpose, {
        tier: pm.tier,
        notes: pm.notes ?? null,
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
            <h2 className="text-xl font-bold" style={{ margin: 0 }}>{t("settings.routingAi")}</h2>
            <p className="text-base text-muted" style={{ margin: "6px 0 0" }}>
              Imposta la gerarchia globale dei provider, i modelli preferiti e gli override per intent. La chat usera questa chain in runtime.
            </p>
          </div>
          <div className="flex-row-gap-8">
            {saved && <span className="text-sm font-semibold" style={{ color: tc.success }}>{t("settings.salvato")}</span>}
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

        {providersError && (
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
            Elenco provider non aggiornato: {providersError}. La pagina mostra i
            provider storici e potrebbe ometterne di configurati — ricarica prima
            di salvare il routing.
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
          <div className="text-xl font-bold" style={{ marginBottom: 6 }}>{t("settings.routingAvanzato")}</div>
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
