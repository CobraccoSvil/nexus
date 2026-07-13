"use client";

import { useCallback, useEffect, useState } from "react";
import { useTheme, useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";
import {
  useProjectStore,
  selectProviderHealthChangedAt,
  selectSettingsChangedAt,
} from "../../lib/project-dispatcher/store";
import { ProviderSettings, type BrowseDirectoriesResponse, type GatewayProvider, type SettingEntry } from "./provider-settings";
import { RoutingConfig } from "./routing-config";
import { PluginManager } from "./plugin-manager";
import { InfrastructureSettings } from "./infrastructure-settings";
import { SecuritySettings } from "./security-settings";
import { GatewayConfig } from "./gateway-config";
import { CatalogMaintenance } from "./catalog-maintenance";
import { ProvidersOverview } from "./providers-overview";
import { getGatewayProviders } from "../../lib/api-client";
import { labelForCategory } from "../../lib/settings-categories";

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

// Le categorie di navigazione derivano dai dati: vedi lib/settings-categories.ts
// (punto unico, regola L). La vecchia CATEGORY_ORDER hardcoded e' stata rimossa.

interface SettingsPanelProps {
  category?: string;
}

export function SettingsPanel({ category }: SettingsPanelProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();
  const { t } = useI18n();
  const [settings, setSettings] = useState<SettingEntry[]>([]);
  const [editValues, setEditValues] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState<Record<string, boolean>>({});
  const [saved, setSaved] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [testResults, setTestResults] = useState<Record<string, string>>({});
  const [gatewayProviders, setGatewayProviders] = useState<GatewayProvider[]>([]);
  const [isBrowsingRoot, setIsBrowsingRoot] = useState(false);
  const [browseBusy, setBrowseBusy] = useState(false);
  const [browseError, setBrowseError] = useState<string | null>(null);
  const [browseData, setBrowseData] = useState<BrowseDirectoriesResponse | null>(null);
  const [newDirectoryName, setNewDirectoryName] = useState("");

  // Proxy via Next.js /neural/* → mcp-core :4000 /api/neural/* (server-side, no CORS).
  // Il brain Python e' stato eliminato: gli endpoint neural vivono ora in mcp-core.
  const NEURAL_BASE = "/neural";

  // "Testa" provider: smart-test che ricarica il provider in modo completo.
  //   1. Reset cooldown lato mcp-core (rimuove sia in-memory sia Redis).
  //   2. Probe provider via /neural/providers/:name/health (mcp-core).
  //   3. Se status=ok → il provider torna attivo immediatamente, l'UI vede
  //      LED verde tramite dispatcher SSE ProviderHealthChanged.
  //   4. Se status!=ok → il probe lo rimette automaticamente in cooldown.
  //
  // L'admin tipicamente clicca "Testa" dopo aver ricaricato il credito o
  // risolto la quota presso il provider, e si aspetta che il provider torni
  // attivo se davvero ricaricato. Con la sola probe (vecchia logica) il
  // cooldown residuo restava per 6h impedendo la riattivazione.
  const handleTestProvider = useCallback(async (provider: string) => {
    setTestResults((current) => ({ ...current, [provider]: "testing..." }));
    // Step 1: reset cooldown (idempotente, no-op se non in cooldown).
    try {
      const { resetProviderCooldown } = await import("../../lib/api-client");
      await resetProviderCooldown(provider);
    } catch {
      // ignora — il provider potrebbe non essere in cooldown
    }
    // Step 2: probe provider via mcp-core (/neural -> /api/neural).
    try {
      const res = await fetch(`${NEURAL_BASE}/providers/${provider}/health`);
      const data = await res.json();
      const status = data.status || "unknown";
      const reason = data.reason || data.message || data.error || "";
      const detail = reason ? `${status}: ${reason}` : status;
      setTestResults((current) => ({ ...current, [provider]: detail }));
    } catch (e) {
      const msg = e instanceof Error ? e.message : "unreachable";
      setTestResults((current) => ({ ...current, [provider]: `unreachable: ${msg}` }));
    }
  }, []);

  // AZZERA il cooldown lato mcp-core (utile dopo billing recharge/rate_limit
  // risolto), poi ri-testa il provider. La chiamata a /reload-settings ora e'
  // un no-op lato mcp-core (il brain Python che ricaricava le chiavi non esiste
  // piu'): resta innocua e non rompe il flusso.
  const handleReloadProvider = useCallback(async (provider: string) => {
    try {
      await fetch(`${NEURAL_BASE}/reload-settings`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      });
    } catch {
      // ignora errori di reload, prova comunque il reset cooldown e il test
    }
    // Best-effort: il reset-cooldown e' no-op se non c'e' cooldown attivo
    // (idempotente lato Rust). Cosi' dopo billing recharge l'utente puo'
    // riattivare il provider senza passare dall'API.
    try {
      const { resetProviderCooldown } = await import("../../lib/api-client");
      await resetProviderCooldown(provider);
    } catch {
      // ignora: il provider potrebbe non essere registrato nei cooldown
    }
    await handleTestProvider(provider);
  }, [handleTestProvider]);

  const loadGatewayProviders = useCallback(async () => {
    try {
      const data = await getGatewayProviders();
      const d = data as { providers?: GatewayProvider[] };
      setGatewayProviders(Array.isArray(d?.providers) ? d.providers! : []);
    } catch {
      setGatewayProviders([]);
    }
  }, []);

  useEffect(() => {
    if (category !== "providers") return;
    void loadGatewayProviders();
  }, [category, loadGatewayProviders]);

  // Event-driven: ricarica gateway providers quando il health probe emette ProviderHealthChanged
  const providerHealthAt = useProjectStore(selectProviderHealthChangedAt);
  useEffect(() => {
    if (providerHealthAt > 0) void loadGatewayProviders();
  }, [providerHealthAt, loadGatewayProviders]);

  const fetchSettings = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/api/admin/settings`, { credentials: "include" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      setSettings(data.settings || []);
      setError(null);
    } catch (fetchError) {
      setError(fetchError instanceof Error ? fetchError.message : "Failed to load settings");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchSettings();
  }, [fetchSettings]);

  // Event-driven: ricarica settings quando SettingChanged arriva via SSE
  const settingsChangedAt = useProjectStore(selectSettingsChangedAt);
  useEffect(() => {
    if (settingsChangedAt > 0) void fetchSettings();
  }, [settingsChangedAt, fetchSettings]);


  const handleSave = async (key: string) => {
    const value = editValues[key];
    if (value === undefined) return;

    setSaving((current) => ({ ...current, [key]: true }));
    try {
      const res = await fetch(`${API_BASE}/api/admin/setting/${key}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        credentials: "include",
        body: JSON.stringify({ value }),
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      setSaved((current) => ({ ...current, [key]: true }));
      setTimeout(() => setSaved((current) => ({ ...current, [key]: false })), 2000);
      setEditValues((current) => {
        const next = { ...current };
        delete next[key];
        return next;
      });
      await fetchSettings();
      // Se si salva una API key, ricarica il brain e ri-testa
      if (key.endsWith("_api_key")) {
        const providerName = key.replace("_api_key", "");
        void handleReloadProvider(providerName);
      }
      // Se si salva la configurazione DNS, invoca /reload-settings su mcp-core
      // (no-op dopo l'eliminazione del brain, ma innocuo).
      if (key === "network_dns_servers") {
        try {
          await fetch(`${NEURAL_BASE}/reload-settings`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: "{}",
          });
        } catch { /* ignore */ }
      }
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "Save failed");
    } finally {
      setSaving((current) => ({ ...current, [key]: false }));
    }
  };

  const handleSaveImmediate = async (key: string, value: string) => {
    setSaving((current) => ({ ...current, [key]: true }));
    try {
      const res = await fetch(`${API_BASE}/api/admin/setting/${key}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        credentials: "include",
        body: JSON.stringify({ value }),
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      await fetchSettings();
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "Save failed");
    } finally {
      setSaving((current) => ({ ...current, [key]: false }));
    }
  };

  const loadAdminDirectories = useCallback(async (path?: string) => {
    setBrowseBusy(true);
    setBrowseError(null);
    try {
      const url = new URL(`${API_BASE}/api/admin/fs/directories`);
      if (path?.trim()) {
        url.searchParams.set("path", path.trim());
      }
      const res = await fetch(url.toString(), { credentials: "include" });
      if (!res.ok) {
        let details = "";
        try {
          const payload = await res.json();
          if (payload?.error && typeof payload.error === "string") {
            details = ` - ${payload.error}`;
          }
        } catch {
          // ignore parse errors
        }
        throw new Error(`API error ${res.status}: ${res.statusText}${details}`);
      }
      const data = (await res.json()) as BrowseDirectoriesResponse;
      setBrowseData(data);
    } catch (browseLoadError) {
      setBrowseError(
        browseLoadError instanceof Error
          ? browseLoadError.message
          : "Impossibile navigare il filesystem.",
      );
    } finally {
      setBrowseBusy(false);
    }
  }, []);

  const createAdminDirectory = useCallback(async () => {
    if (!browseData || !newDirectoryName.trim()) return;
    setBrowseBusy(true);
    setBrowseError(null);
    try {
      const res = await fetch(`${API_BASE}/api/admin/fs/directories/create`, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          parent_path: browseData.currentPath,
          name: newDirectoryName.trim(),
        }),
      });
      if (!res.ok) {
        let details = "";
        try {
          const payload = await res.json();
          if (payload?.error && typeof payload.error === "string") {
            details = ` - ${payload.error}`;
          }
        } catch {
          // ignore parse errors
        }
        throw new Error(`API error ${res.status}: ${res.statusText}${details}`);
      }
      setNewDirectoryName("");
      await loadAdminDirectories(browseData.currentPath);
    } catch (createError) {
      setBrowseError(
        createError instanceof Error
          ? createError.message
          : "Impossibile creare la directory.",
      );
      setBrowseBusy(false);
    }
  }, [browseData, loadAdminDirectories, newDirectoryName]);

  const items = category
    ? settings.filter((setting) => setting.category === category)
    : settings;
  // Items della categoria gateway (per la sezione integrata nella pagina providers)
  const gatewayItems = settings.filter((s) => s.category === "gateway");
  const routingItems = category === "routing"
    ? settings.filter(
        (setting) =>
          setting.category === "routing" ||
          setting.key === "nexus_active_routing_pct",
      )
    : items;

  const catKey = category ? (`cat.${category}` as Parameters<typeof t>[0]) : null;
  // Se la chiave i18n non esiste, t() ritorna la chiave grezza ("cat.agent"):
  // in quel caso usa la label nota (labelForCategory) invece di mostrarla cruda.
  const translated = catKey ? t(catKey) : "";
  const catLabel = category
    ? (translated && translated !== catKey ? translated : labelForCategory(category))
    : "";
  const title = category ? catLabel : t("admin.settings");
  const subtitle = category
    ? t("settings.configure", { category: catLabel.toLowerCase() })
    : t("settings.stored");

  if (loading) {
    return <div style={{ padding: 40, textAlign: "center", color: tc.textMuted }}>{t("settings.loading")}</div>;
  }

  if (error && settings.length === 0) {
    return (
      <div style={{ padding: 40, textAlign: "center" }}>
        <div className="text-lg" style={{ color: tc.error, marginBottom: 12 }}>{t("settings.errorConnect")}</div>
        <div className="text-base" style={{ color: tc.textMuted }}>{error}</div>
        <div style={{ color: tc.textMuted, fontSize: 13, marginTop: 8 }}>
          {t("settings.errorServer", { url: API_BASE })}
        </div>
      </div>
    );
  }

  // Props comuni di <ProviderSettings>: punto unico (regola L, S69) per i
  // 2 rami if/else che prima ripetevano 18 prop identici (cluster top
  // intra-file 24L). Differiscono solo per `gatewayProviders` nel ramo
  // "category === providers".
  const providerSettingsCommonProps = {
    items,
    editValues,
    saving,
    saved,
    testResults,
    isBrowsingRoot,
    browseBusy,
    browseError,
    browseData,
    newDirectoryName,
    onEditChange: (key: string, value: string) =>
      setEditValues((current) => ({ ...current, [key]: value })),
    onSave: handleSave,
    onSaveImmediate: handleSaveImmediate,
    onTestProvider: handleTestProvider,
    onReloadProvider: handleReloadProvider,
    onOpenBrowse: (currentValue: string) => {
      setIsBrowsingRoot(true);
      if (!browseData) {
        void loadAdminDirectories(currentValue);
      }
    },
    onCloseBrowse: () => setIsBrowsingRoot(false),
    onLoadDirectories: loadAdminDirectories,
    onCreateDirectory: createAdminDirectory,
    onSetNewDirectoryName: setNewDirectoryName,
    onSelectDirectory: (path: string) =>
      setEditValues((current) => ({ ...current, projects_base_root: path })),
  };

  return (
    <div>
      <h1 className="text-2xl font-semibold" style={{ marginBottom: 6 }}>{title}</h1>
      <p className="text-base" style={{ color: tc.textMuted, marginBottom: 28 }}>{subtitle}</p>

      {error && (
        <div
          style={{
            padding: "10px 16px",
            background: resolved === "dark" ? "#2d1215" : "#fef2f2",
            border: `1px solid ${tc.error}`,
            borderRadius: 8,
            color: tc.error,
            fontSize: 13,
            marginBottom: 24,
          }}
        >
          {error}
        </div>
      )}

      {items.length === 0 && category !== "connectors" && (
        <div style={{ padding: 40, textAlign: "center", color: tc.textMuted, fontSize: 13 }}>
          {t("settings.noItems")}
        </div>
      )}

      {category === "routing" ? (
        <RoutingConfig settings={routingItems} onSaveComplete={fetchSettings} />
      ) : category === "connectors" ? (
        <PluginManager />
      ) : category === "security" ? (
        <SecuritySettings
          items={items}
          editValues={editValues}
          saving={saving}
          saved={saved}
          onEditChange={(key, value) => setEditValues((current) => ({ ...current, [key]: value }))}
          onSave={handleSave}
        />
      ) : category === "infrastructure" ? (
        <InfrastructureSettings
          items={items}
          editValues={editValues}
          saving={saving}
          saved={saved}
          onEditChange={(key, value) => setEditValues((current) => ({ ...current, [key]: value }))}
          onSave={handleSave}
          onSaveImmediate={handleSaveImmediate}
          onOpenBrowse={(currentValue) => {
            setIsBrowsingRoot(true);
            if (!browseData) {
              void loadAdminDirectories(currentValue);
            }
          }}
        />
      ) : category === "providers" ? (
        <>
          {/* Card omogenee per-provider (API key, modelli, budget aggregati) */}
          <ProvidersOverview
            {...providerSettingsCommonProps}
            gatewayProviders={gatewayProviders}
          />
          {/* ── Sezione Catalogo modelli ── */}
          <CatalogMaintenance />
          {/* ── Sezione Gateway LLM integrata ── */}
          <div style={{ marginTop: 40, borderTop: "1px solid var(--color-border)", paddingTop: 24 }}>
            <h2 style={{ fontSize: 18, fontWeight: 600, marginBottom: 6 }}>Gateway LLM</h2>
            <p style={{ fontSize: 13, color: "var(--color-textMuted)", marginBottom: 20 }}>
              Hot-reload e parametri del gateway LLM.
            </p>
            <GatewayConfig
              items={gatewayItems}
              onSaveComplete={fetchSettings}
              onRefreshProviders={loadGatewayProviders}
            />
          </div>
        </>
      ) : (
        <ProviderSettings {...providerSettingsCommonProps} />
      )}
    </div>
  );
}
