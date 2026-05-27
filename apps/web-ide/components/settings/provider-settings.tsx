"use client";

import { useState, useEffect } from "react";
import { useTheme, useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";

// eslint-disable-next-line @typescript-eslint/no-unused-vars
const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

export interface GatewayProvider {
  name: string;
  // null = gateway offline, stato dall'ultimo health probe o mai misurato
  healthy: boolean | null;
  configured?: boolean;
  last_check?: string;
  last_health_check_at?: string;
  error?: string;
}

export interface SettingEntry {
  key: string;
  value: string;
  category: string;
  description: string;
  is_secret: boolean;
  has_value: boolean;
  updated_at: string;
}

interface BrowseDirectoryNode {
  name: string;
  path: string;
  has_children: boolean;
}

export interface BrowseDirectoriesResponse {
  roots: string[];
  currentPath: string;
  parentPath?: string;
  directories: BrowseDirectoryNode[];
}

// Provider che supportano il toggle enable/disable
const PROVIDER_NAMES = ["anthropic", "openai", "google", "deepseek", "mistral"] as const;

interface ProviderSettingsProps {
  items: SettingEntry[];
  editValues: Record<string, string>;
  saving: Record<string, boolean>;
  saved: Record<string, boolean>;
  testResults: Record<string, string>;
  isBrowsingRoot: boolean;
  browseBusy: boolean;
  browseError: string | null;
  browseData: BrowseDirectoriesResponse | null;
  newDirectoryName: string;
  onEditChange: (key: string, value: string) => void;
  onSave: (key: string) => Promise<void>;
  /** Salva immediatamente un valore senza passare per editValues (usato per i toggle). */
  onSaveImmediate: (key: string, value: string) => Promise<void>;
  onTestProvider: (provider: string) => Promise<void>;
  onReloadProvider: (provider: string) => Promise<void>;
  onOpenBrowse: (currentValue: string) => void;
  onCloseBrowse: () => void;
  onLoadDirectories: (path?: string) => Promise<void>;
  onCreateDirectory: () => Promise<void>;
  onSetNewDirectoryName: (name: string) => void;
  onSelectDirectory: (path: string) => void;
  /** Provider LLM dal gateway: fonte primaria dello stato. */
  gatewayProviders?: GatewayProvider[];
}

export function ProviderSettings({
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
  onEditChange,
  onSave,
  onSaveImmediate,
  onTestProvider,
  onReloadProvider,
  onOpenBrowse,
  onCloseBrowse,
  onLoadDirectories,
  onCreateDirectory,
  onSetNewDirectoryName,
  onSelectDirectory,
  gatewayProviders,
}: ProviderSettingsProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();
  const { t } = useI18n();
  const [embeddingMsg, setEmbeddingMsg] = useState<string | null>(null);
  const [embeddingBusy, setEmbeddingBusy] = useState(false);

  // Catalogo modelli per i dropdown _model.
  // Stato esplicito (loading/error/ok) per evitare il fallback silenzioso al
  // textbox quando il fetch e' in flight o fallisce — il dropdown deve sempre
  // apparire per i setting *_model, con messaggio chiaro all'utente se il
  // catalogo non arriva.
  const [modelCatalog, setModelCatalog] = useState<Array<{ provider: string; model: string }>>([]);
  const [modelCatalogStatus, setModelCatalogStatus] = useState<"loading" | "ok" | "error">("loading");
  useEffect(() => {
    fetch("/api/models", { credentials: "include" })
      .then(async (r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then((d) => {
        const models = Array.isArray(d?.models) ? d.models as Array<{ provider: string; model: string }> : [];
        setModelCatalog(models);
        setModelCatalogStatus(models.length > 0 ? "ok" : "error");
      })
      .catch(() => {
        setModelCatalog([]);
        setModelCatalogStatus("error");
      });
  }, []);

  // Set di chiavi _enabled già incorporate nei card delle API key — non vanno mostrate come card separati
  const embeddedEnabledKeys = new Set(PROVIDER_NAMES.map((p) => `${p}_enabled`));

  return (
    <>
      {/* ── Banner stato gateway LLM ── */}
      {gatewayProviders && gatewayProviders.length > 0 && (
        <div style={{
          display: "flex", alignItems: "center", gap: 8,
          padding: "10px 16px", borderRadius: 8,
          border: "1px solid var(--color-border)",
          background: "var(--color-bgCard)",
          marginBottom: 8, flexWrap: "wrap",
        }}>
          <span style={{
            fontSize: 11, color: "var(--color-textMuted)", fontWeight: 600,
            letterSpacing: "0.07em", textTransform: "uppercase", marginRight: 4,
          }}>
            Gateway
          </span>
          {gatewayProviders.map((p) => (
            <div key={p.name} style={{
              display: "flex", alignItems: "center", gap: 6,
              padding: "4px 10px", background: "var(--color-bgInput)",
              borderRadius: 6, border: "1px solid var(--color-border)", fontSize: 12,
            }}>
              <span style={{
                width: 7, height: 7, borderRadius: "50%", flexShrink: 0,
                background: p.healthy ? "#4ade80" : "#f87171",
              }} />
              <span style={{ fontWeight: 600 }}>{p.name}</span>
              {!p.healthy && p.error && (
                <span style={{ color: "#f87171", fontSize: 11 }}>{p.error}</span>
              )}
            </div>
          ))}
        </div>
      )}

      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {items.map((setting) => {
          // Salta i setting _enabled dei provider — sono incorporati nel card della API key
          if (embeddedEnabledKeys.has(setting.key)) return null;

          const isEditing = editValues[setting.key] !== undefined;
          const currentValue = isEditing ? editValues[setting.key] : setting.value;
          const isSaving = saving[setting.key];
          const isSaved = saved[setting.key];
          // Per setting standard `<provider>_api_key`: deriva providerName.
          // Per `google_provider_backend` (Vertex selector): trattato come pseudo-api-key
          // per esporre LED + tasto Testa (usa lo stesso endpoint /providers/google/health
          // che internamente sceglie Gemini direct o Vertex in base al backend in DB).
          const isVertexBackendSwitch = setting.key === "google_provider_backend";
          const providerName = isVertexBackendSwitch
            ? "google"
            : setting.key.replace("_api_key", "");

          // Cerca il corrispondente setting _enabled (solo per le API key dei provider noti)
          const isProviderApiKey =
            (setting.key.endsWith("_api_key") || isVertexBackendSwitch) &&
            PROVIDER_NAMES.includes(providerName as (typeof PROVIDER_NAMES)[number]);

          const enabledItem = isProviderApiKey
            ? items.find((i) => i.key === `${providerName}_enabled`)
            : undefined;
          // default true se il setting non esiste ancora in DB
          const isProviderEnabled = enabledItem ? enabledItem.value !== "false" : true;
          const isTogglingEnabled = saving[`${providerName}_enabled`];

          return (
            <div
              key={setting.key}
              style={{
                padding: "12px 16px",
                borderRadius: 8,
                border: `1px solid ${isProviderApiKey && !isProviderEnabled ? tc.border : tc.border}`,
                background: isProviderApiKey && !isProviderEnabled
                  ? resolved === "dark" ? "#1a1a1a" : "#f8f8f8"
                  : tc.bgCard,
                opacity: isProviderApiKey && !isProviderEnabled ? 0.65 : 1,
                transition: "opacity 0.2s",
              }}
            >
              <div className="flex-row" style={{ justifyContent: "space-between", marginBottom: 4 }}>
                <div className="flex-row-gap-6">
                  <span className="text-base font-semibold" style={{ color: tc.text }}>{setting.key}</span>
                  {setting.is_secret && (
                    <span
                      style={{
                        padding: "2px 6px",
                        borderRadius: 4,
                        background: resolved === "dark" ? "#2d1f00" : "#fef3c7",
                        color: tc.warning,
                        fontSize: 10,
                        fontWeight: 600,
                      }}
                    >
                      {t("settings.secret")}
                    </span>
                  )}
                  {isProviderApiKey && !isProviderEnabled && (
                    <span
                      style={{
                        padding: "2px 7px",
                        borderRadius: 4,
                        background: resolved === "dark" ? "#2a2a2a" : "#e5e7eb",
                        color: "var(--color-textMuted)",
                        fontSize: 10,
                        fontWeight: 600,
                        letterSpacing: "0.03em",
                      }}
                    >
                      DISABILITATO
                    </span>
                  )}
                </div>
                <div className="flex-row-gap-6">
                  {/* ── Toggle enable/disable provider ── */}
                  {isProviderApiKey && (
                    <button
                      disabled={isTogglingEnabled}
                      onClick={async () => {
                        const newVal = isProviderEnabled ? "false" : "true";
                        await onSaveImmediate(`${providerName}_enabled`, newVal);
                      }}
                      title={isProviderEnabled ? "Disabilita provider" : "Abilita provider"}
                      style={{
                        width: 38,
                        height: 20,
                        borderRadius: 10,
                        border: "none",
                        background: isTogglingEnabled
                          ? tc.bgInput
                          : isProviderEnabled
                          ? tc.success
                          : tc.textMuted,
                        cursor: isTogglingEnabled ? "not-allowed" : "pointer",
                        position: "relative",
                        transition: "background 0.2s",
                        flexShrink: 0,
                        outline: `1px solid ${isProviderEnabled ? `${tc.success}60` : tc.border}`,
                        opacity: isTogglingEnabled ? 0.6 : 1,
                      }}
                    >
                      <span
                        style={{
                          position: "absolute",
                          top: 2,
                          left: isProviderEnabled ? 19 : 2,
                          width: 16,
                          height: 16,
                          borderRadius: "50%",
                          background: "#fff",
                          transition: "left 0.2s",
                          boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
                        }}
                      />
                    </button>
                  )}

                  {/* ── LED stato provider ──
                       Visibile per (a) le API key dei provider (campi `*_api_key`,
                       sensitive) e (b) `google_provider_backend` (Vertex selector,
                       not-secret): cosi' admin che configurano Vertex hanno il
                       tasto Testa direttamente accanto al campo backend, senza
                       dover scorrere fino a `google_api_key`.
                       L'endpoint /providers/google/health usato dal Testa sceglie
                       internamente Gemini direct o Vertex in base al backend in DB. */}
                  {((setting.is_secret && setting.key.endsWith("_api_key")) || isVertexBackendSwitch) && (() => {
                    const billingUrls: Record<string, string> = {
                      anthropic: "https://console.anthropic.com/settings/billing",
                      openai:    "https://platform.openai.com/account/billing",
                      google:    "https://console.cloud.google.com/billing",
                      deepseek:  "https://platform.deepseek.com/api-keys",
                      mistral:   "https://console.mistral.ai/api-keys",
                    };

                    // Se disabilitato: mostra sempre il LED grigio "Disabilitato"
                    if (isProviderApiKey && !isProviderEnabled) {
                      return (
                        <span
                          style={{
                            display: "inline-flex",
                            alignItems: "center",
                            gap: 4,
                            padding: "3px 8px",
                            borderRadius: 12,
                            background: `${tc.textMuted}18`,
                            border: `1px solid ${tc.textMuted}40`,
                            color: "var(--color-textMuted)",
                            fontSize: 11,
                            fontWeight: 600,
                          }}
                        >
                          <span style={{ width: 6, height: 6, borderRadius: "50%", background: tc.textMuted, flexShrink: 0 }} />
                          Disabilitato
                        </span>
                      );
                    }

                    // Fonte primaria: stato gateway. Fallback: test brain.
                    const gwProvider = gatewayProviders?.find((p) => p.name === providerName);
                    const result = testResults[providerName];
                    const isTesting = result === "testing...";

                    const useGwState = gwProvider != null;
                    const isReady = useGwState ? gwProvider.healthy : result?.startsWith("ready");
                    const isDisabledResult = !useGwState && result === "disabled";
                    const isError = useGwState
                      ? !gwProvider.healthy
                      : (result && !isTesting && !result.startsWith("ready") && result !== "disabled");
                    const badgeLabel = useGwState
                      ? (gwProvider.healthy ? "OK" : (gwProvider.error?.split(":")[0] ?? "Errore"))
                      : (isReady ? "OK" : isDisabledResult ? "Disabilitato" : result?.split(":")[0] ?? "");
                    const statusColor = isTesting
                      ? tc.textSecondary
                      : isDisabledResult
                      ? tc.textMuted
                      : isReady
                      ? tc.success
                      : isError
                      ? tc.error
                      : tc.textSecondary;
                    const billingUrl = billingUrls[providerName];
                    const showBadge = useGwState || (result && !isTesting);

                    return (
                      <>
                        {showBadge && (
                          <span
                            title={useGwState
                              ? (gwProvider.healthy ? "Gateway: OK" : (gwProvider.error ?? "Errore gateway"))
                              : result}
                            style={{
                              display: "inline-flex",
                              alignItems: "center",
                              gap: 4,
                              padding: "3px 8px",
                              borderRadius: 12,
                              background: isReady
                                ? `${tc.success}18`
                                : isDisabledResult
                                ? `${tc.textMuted}18`
                                : `${tc.error}18`,
                              border: `1px solid ${statusColor}40`,
                              color: statusColor,
                              fontSize: 11,
                              fontWeight: 600,
                              maxWidth: 260,
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                              whiteSpace: "nowrap",
                              cursor: "help",
                            }}
                          >
                            <span style={{ width: 6, height: 6, borderRadius: "50%", background: statusColor, flexShrink: 0 }} />
                            {badgeLabel}
                          </span>
                        )}
                        {isError && (
                          <button
                            onClick={() => onReloadProvider(providerName)}
                            title="Ricarica chiave dal DB, azzera il cooldown (utile dopo billing recharge) e ri-testa il provider"
                            style={{
                              padding: "3px 8px",
                              borderRadius: 6,
                              border: "1px solid var(--color-border)",
                              background: "var(--color-bgInput)",
                              color: tc.accent,
                              fontSize: 11,
                              cursor: "pointer",
                              fontFamily: "inherit",
                            }}
                          >
                            ↺ Ricarica
                          </button>
                        )}
                        {billingUrl && isError && (
                          <a
                            href={billingUrl}
                            target="_blank"
                            rel="noopener noreferrer"
                            title="Apri console billing"
                            style={{
                              padding: "3px 7px",
                              borderRadius: 6,
                              border: "1px solid var(--color-border)",
                              background: "transparent",
                              color: "var(--color-textMuted)",
                              fontSize: 11,
                              cursor: "pointer",
                              textDecoration: "none",
                              fontFamily: "inherit",
                            }}
                          >
                            🔗
                          </a>
                        )}
                        <button
                          onClick={() => onTestProvider(providerName)}
                          style={{
                            padding: "3px 10px",
                            borderRadius: 6,
                            border: "1px solid var(--color-border)",
                            background: "var(--color-bgInput)",
                            color: "var(--color-textSecondary)",
                            fontSize: 11,
                            cursor: "pointer",
                            fontFamily: "inherit",
                          }}
                        >
                          {isTesting ? "..." : "Testa"}
                        </button>
                      </>
                    );
                  })()}
                  {setting.key === "projects_base_root" && (
                    <button
                      onClick={() => onOpenBrowse(currentValue || setting.value)}
                      style={{
                        padding: "4px 10px",
                        borderRadius: 6,
                        border: "1px solid var(--color-border)",
                        background: "var(--color-bgInput)",
                        color: "var(--color-textSecondary)",
                        fontSize: 11,
                        cursor: "pointer",
                        fontFamily: "inherit",
                      }}
                    >
                      Sfoglia
                    </button>
                  )}
                  {isEditing && (
                    <button
                      onClick={() => onSave(setting.key)}
                      disabled={isSaving}
                      style={{
                        padding: "4px 14px",
                        borderRadius: 6,
                        border: "none",
                        background: isSaving ? tc.bgInput : tc.success,
                        color: "#fff",
                        fontSize: 12,
                        cursor: isSaving ? "not-allowed" : "pointer",
                        fontWeight: 600,
                        fontFamily: "inherit",
                      }}
                    >
                      {isSaving ? t("settings.saving") : t("settings.save")}
                    </button>
                  )}
                  {isSaved && <span style={{ color: "var(--color-success)", fontSize: 12, padding: "4px 8px" }}>{t("settings.saved")}</span>}
                </div>
              </div>

              <div style={{ color: "var(--color-textMuted)", fontSize: 12, marginBottom: 6 }}>{setting.description}</div>

              {(currentValue === "true" || currentValue === "false") && !setting.is_secret ? (
                /* Toggle generico: cattura automaticamente qualsiasi setting con valore true/false */
                <div style={{ display: "flex", alignItems: "center", gap: 10, marginTop: 4 }}>
                  <button
                    onClick={() => {
                      const newVal = currentValue === "true" ? "false" : "true";
                      void onSaveImmediate(setting.key, newVal);
                    }}
                    disabled={isSaving}
                    style={{
                      width: 44,
                      height: 24,
                      borderRadius: 12,
                      border: "none",
                      background: currentValue === "true" ? tc.success : tc.bgInput,
                      cursor: isSaving ? "not-allowed" : "pointer",
                      position: "relative",
                      transition: "background 0.2s",
                      flexShrink: 0,
                      outline: `1px solid ${tc.border}`,
                      opacity: isSaving ? 0.7 : 1,
                    }}
                    title={currentValue === "true" ? "Attivo — clicca per disabilitare" : "Non attivo — clicca per abilitare"}
                  >
                    <span
                      style={{
                        position: "absolute",
                        top: 3,
                        left: currentValue === "true" ? 23 : 3,
                        width: 18,
                        height: 18,
                        borderRadius: "50%",
                        background: "#fff",
                        transition: "left 0.2s",
                        boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
                      }}
                    />
                  </button>
                  <span style={{ fontSize: 12, color: currentValue === "true" ? tc.success : "var(--color-textMuted)" }}>
                    {currentValue === "true" ? "ON" : "OFF"}
                  </span>
                  {isSaving && <span style={{ fontSize: 10, color: "var(--color-textMuted)" }}>...</span>}
                  {isSaved && <span style={{ fontSize: 10, color: tc.success }}>✓</span>}
                </div>
              ) : setting.key === "quality_severity_threshold" ? (
                <select
                  value={currentValue || "medium"}
                  onChange={(event) => onEditChange(setting.key, event.target.value)}
                  style={{
                    width: "100%",
                    padding: "8px 12px",
                    borderRadius: 6,
                    border: `1px solid ${isEditing ? tc.accent : tc.border}`,
                    background: "var(--color-bgInput)",
                    color: tc.text,
                    fontSize: 13,
                    fontFamily: "inherit",
                    boxSizing: "border-box",
                    cursor: "pointer",
                  }}
                >
                  {["low", "medium", "high"].map((opt) => (
                    <option key={opt} value={opt}>
                      {opt}
                    </option>
                  ))}
                </select>
              ) : setting.key === "embedding_model" ? (
                <div style={{ display: "grid", gap: 8 }}>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                    <select
                      value={
                        (currentValue || "all-MiniLM-L6-v2") === "all-MiniLM-L6-v2"
                          ? "all-MiniLM-L6-v2"
                          : "__custom__"
                      }
                      onChange={(event) => {
                        setEmbeddingMsg(null);
                        const v = event.target.value;
                        if (v === "__custom__") {
                          // attiva custom
                          if (currentValue === "all-MiniLM-L6-v2") onEditChange(setting.key, "");
                          return;
                        }
                        onEditChange(setting.key, v);
                      }}
                      style={{
                        flex: "1 1 220px",
                        minWidth: 220,
                        padding: "8px 12px",
                        borderRadius: 6,
                        border: `1px solid ${isEditing ? tc.accent : tc.border}`,
                        background: "var(--color-bgInput)",
                        color: tc.text,
                        fontSize: 13,
                        fontFamily: "inherit",
                        boxSizing: "border-box",
                        cursor: "pointer",
                      }}
                    >
                      <option value="all-MiniLM-L6-v2">all-MiniLM-L6-v2 (curato)</option>
                      <option value="__custom__">Custom…</option>
                    </select>

                    <button
                      type="button"
                      disabled={embeddingBusy || !(currentValue ?? "").trim()}
                      onClick={async () => {
                        try {
                          setEmbeddingBusy(true);
                          setEmbeddingMsg(null);
                          const res = await fetch("/api/admin/embeddings/validate", {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            credentials: "include",
                            body: JSON.stringify({ model: (currentValue ?? "").trim() }),
                          });
                          const data = await res.json().catch(() => ({}));
                          if (!res.ok) throw new Error(data?.error || `HTTP ${res.status}`);
                          setEmbeddingMsg(`OK. dimensions=${data.dimensions}`);
                        } catch (e) {
                          setEmbeddingMsg(`Errore: ${e instanceof Error ? e.message : "validazione fallita"}`);
                        } finally {
                          setEmbeddingBusy(false);
                        }
                      }}
                      style={{
                        padding: "4px 10px",
                        borderRadius: 6,
                        border: "1px solid var(--color-border)",
                        background: "var(--color-bgInput)",
                        color: "var(--color-textSecondary)",
                        fontSize: 11,
                        cursor: embeddingBusy ? "not-allowed" : "pointer",
                        fontFamily: "inherit",
                        opacity: embeddingBusy ? 0.7 : 1,
                      }}
                      title="Verifica che il modello sia caricabile e legge la dimensione embeddings"
                    >
                      {embeddingBusy ? "..." : "Valida"}
                    </button>

                    <button
                      type="button"
                      disabled={embeddingBusy || !(currentValue ?? "").trim()}
                      onClick={async () => {
                        try {
                          setEmbeddingBusy(true);
                          setEmbeddingMsg(null);
                          const res = await fetch("/api/admin/embeddings/apply", {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            credentials: "include",
                            body: JSON.stringify({ model: (currentValue ?? "").trim(), reindex: true }),
                          });
                          const data = await res.json().catch(() => ({}));
                          if (!res.ok) throw new Error(data?.error || `HTTP ${res.status}`);
                          setEmbeddingMsg(
                            `Applicato. dim=${data.dimensions}, collection=${data.qdrantCollection}. Reindex completato.`,
                          );
                        } catch (e) {
                          setEmbeddingMsg(`Errore: ${e instanceof Error ? e.message : "apply fallito"}`);
                        } finally {
                          setEmbeddingBusy(false);
                        }
                      }}
                      style={{
                        padding: "4px 10px",
                        borderRadius: 6,
                        border: "1px solid var(--color-border)",
                        background: tc.accentBg,
                        color: tc.accent,
                        fontSize: 11,
                        cursor: embeddingBusy ? "not-allowed" : "pointer",
                        fontFamily: "inherit",
                        opacity: embeddingBusy ? 0.7 : 1,
                      }}
                      title="Applica il modello e fa reindex (reset della collection dedicata)"
                    >
                      {embeddingBusy ? "..." : "Applica + Reindex"}
                    </button>
                  </div>

                  {(currentValue || "") !== "all-MiniLM-L6-v2" && (
                    <input
                      type="text"
                      value={currentValue}
                      placeholder="es. sentence-transformers/all-MiniLM-L6-v2 oppure BAAI/bge-small-en-v1.5"
                      onChange={(event) => onEditChange(setting.key, event.target.value)}
                      style={{
                        width: "100%",
                        padding: "8px 12px",
                        borderRadius: 6,
                        border: `1px solid ${isEditing ? tc.accent : tc.border}`,
                        background: "var(--color-bgInput)",
                        color: tc.text,
                        fontSize: 13,
                        fontFamily: "inherit",
                        boxSizing: "border-box",
                      }}
                    />
                  )}

                  <div style={{ fontSize: 11, color: "var(--color-textMuted)" }}>
                    Custom richiede validazione: Nexus legge la dimensione dal Neural Core e crea una collection Qdrant dedicata.
                    Con “Applica + Reindex” la collection viene resettata (vector size coerente).
                  </div>
                  {embeddingMsg && (
                    <div style={{ fontSize: 12, color: embeddingMsg.startsWith("Errore") ? tc.error : tc.success }}>
                      {embeddingMsg}
                    </div>
                  )}
                </div>
              ) : setting.key.endsWith("_model") && setting.key !== "embedding_model" ? (
                /* Dropdown modelli per qualsiasi setting *_model (es. reflection_model, summarizer_model).
                   Sempre presente; gestisce loading/error/ok espliciti.
                   Il valore corrente viene preservato come opzione anche se non
                   compare nel catalogo (modelli legacy o con typo). */
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <select
                    value={currentValue}
                    disabled={modelCatalogStatus === "loading"}
                    onChange={(event) => {
                      onEditChange(setting.key, event.target.value);
                      setTimeout(() => void onSave(setting.key), 50);
                    }}
                    style={{
                      width: "100%",
                      padding: "8px 12px",
                      borderRadius: 6,
                      border: `1px solid ${isEditing ? tc.accent : tc.border}`,
                      background: "var(--color-bgInput)",
                      color: tc.text,
                      fontSize: 13,
                      fontFamily: "inherit",
                      boxSizing: "border-box",
                      cursor: modelCatalogStatus === "loading" ? "wait" : "pointer",
                    }}
                  >
                    {/* Valore corrente come prima opzione, anche se non e' nel catalogo */}
                    {currentValue && !modelCatalog.some((m) => m.model === currentValue) && (
                      <option value={currentValue}>
                        {currentValue} (corrente — non nel catalogo)
                      </option>
                    )}
                    {modelCatalogStatus === "loading" && modelCatalog.length === 0 && (
                      <option value="" disabled>Caricamento modelli...</option>
                    )}
                    {/* Raggruppati per provider */}
                    {Array.from(new Set(modelCatalog.map((m) => m.provider))).sort().map((provider) => (
                      <optgroup key={provider} label={provider}>
                        {modelCatalog
                          .filter((m) => m.provider === provider)
                          .map((m) => (
                            <option key={m.model} value={m.model}>
                              {m.model}
                            </option>
                          ))}
                      </optgroup>
                    ))}
                  </select>
                  {modelCatalogStatus === "error" && (
                    <div style={{ fontSize: 11, color: tc.error }}>
                      Errore: catalogo modelli non caricato. Verifica che mcp-core sia
                      raggiungibile e che ai_price_catalog contenga modelli con is_enabled=TRUE.
                    </div>
                  )}
                </div>
              ) : (
                <input
                  type={setting.is_secret ? "password" : "text"}
                  value={currentValue}
                  placeholder={setting.is_secret ? t("settings.enterSecret") : t("settings.enterValue")}
                  onChange={(event) => onEditChange(setting.key, event.target.value)}
                  disabled={isProviderApiKey && !isProviderEnabled}
                  style={{
                    width: "100%",
                    padding: "8px 12px",
                    borderRadius: 6,
                    border: `1px solid ${isEditing ? tc.accent : tc.border}`,
                    background: "var(--color-bgInput)",
                    color: isProviderApiKey && !isProviderEnabled ? tc.textMuted : tc.text,
                    fontSize: 13,
                    fontFamily: "inherit",
                    boxSizing: "border-box",
                    cursor: isProviderApiKey && !isProviderEnabled ? "not-allowed" : "text",
                  }}
                />
              )}
            </div>
          );
        })}
      </div>

      {isBrowsingRoot && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.35)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 80,
            padding: 16,
          }}
        >
          <div
            style={{
              width: 760,
              maxWidth: "96vw",
              maxHeight: "82vh",
              overflow: "auto",
              borderRadius: 12,
              border: "1px solid var(--color-border)",
              background: "var(--color-bgCard)",
              padding: 14,
            }}
          >
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                marginBottom: 10,
                gap: 8,
              }}
            >
              <strong style={{ fontSize: 14 }}>Seleziona root progetti</strong>
              <button
                onClick={onCloseBrowse}
                style={{
                  padding: "4px 10px",
                  borderRadius: 6,
                  border: "1px solid var(--color-border)",
                  background: "var(--color-bgInput)",
                  color: "var(--color-text)",
                  cursor: "pointer",
                  fontFamily: "inherit",
                }}
              >
                Chiudi
              </button>
            </div>

            <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginBottom: 10 }}>
              {browseData?.roots.map((root) => (
                <button
                  key={root}
                  onClick={() => void onLoadDirectories(root)}
                  style={{
                    padding: "4px 8px",
                    borderRadius: 6,
                    border: "1px solid var(--color-border)",
                    background: browseData.currentPath === root ? tc.accentBg : tc.bgInput,
                    color: "var(--color-text)",
                    fontSize: 12,
                    cursor: "pointer",
                    fontFamily: "inherit",
                  }}
                >
                  {root}
                </button>
              ))}
            </div>

            <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
              <button
                disabled={!browseData?.parentPath || browseBusy}
                onClick={() => {
                  if (browseData?.parentPath) {
                    void onLoadDirectories(browseData.parentPath);
                  }
                }}
                style={{
                  padding: "5px 10px",
                  borderRadius: 6,
                  border: "1px solid var(--color-border)",
                  background: "var(--color-bgInput)",
                  color: "var(--color-text)",
                  cursor: !browseData?.parentPath || browseBusy ? "not-allowed" : "pointer",
                  fontFamily: "inherit",
                }}
              >
                Su
              </button>
              <button
                disabled={!browseData || browseBusy}
                onClick={() => {
                  if (!browseData) return;
                  onSelectDirectory(browseData.currentPath);
                  onCloseBrowse();
                }}
                style={{
                  padding: "5px 10px",
                  borderRadius: 6,
                  border: "1px solid var(--color-border)",
                  background: tc.accentBg,
                  color: "var(--color-text)",
                  cursor: !browseData || browseBusy ? "not-allowed" : "pointer",
                  fontFamily: "inherit",
                }}
              >
                Usa questa cartella
              </button>
            </div>

            <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
              <input
                value={newDirectoryName}
                onChange={(event) => onSetNewDirectoryName(event.target.value)}
                placeholder="Nuova directory"
                style={{
                  flex: 1,
                  minWidth: 0,
                  padding: "6px 10px",
                  borderRadius: 6,
                  border: "1px solid var(--color-border)",
                  background: "var(--color-bgInput)",
                  color: "var(--color-text)",
                  fontFamily: "inherit",
                }}
              />
              <button
                disabled={!browseData || browseBusy || !newDirectoryName.trim()}
                onClick={() => void onCreateDirectory()}
                style={{
                  padding: "6px 10px",
                  borderRadius: 6,
                  border: "1px solid var(--color-border)",
                  background: "var(--color-bgInput)",
                  color: "var(--color-text)",
                  cursor:
                    !browseData || browseBusy || !newDirectoryName.trim()
                      ? "not-allowed"
                      : "pointer",
                  fontFamily: "inherit",
                }}
              >
                Crea cartella
              </button>
            </div>

            <div style={{ marginBottom: 8, fontSize: 12, color: "var(--color-textMuted)", wordBreak: "break-all" }}>
              {browseData?.currentPath ?? "Caricamento percorso..."}
            </div>

            <div
              style={{
                border: "1px solid var(--color-border)",
                borderRadius: 8,
                background: "var(--color-bgInput)",
                minHeight: 140,
              }}
            >
              {browseBusy ? (
                <div style={{ padding: 10, fontSize: 12, color: "var(--color-textMuted)" }}>Caricamento...</div>
              ) : browseError ? (
                <div style={{ padding: 10, fontSize: 12, color: "var(--color-error)" }}>{browseError}</div>
              ) : browseData && browseData.directories.length > 0 ? (
                browseData.directories.map((directory) => (
                  <button
                    key={directory.path}
                    onClick={() => void onLoadDirectories(directory.path)}
                    style={{
                      width: "100%",
                      textAlign: "left",
                      padding: "8px 10px",
                      border: "none",
                      borderBottom: `1px solid ${tc.border}`,
                      background: "transparent",
                      color: "var(--color-text)",
                      cursor: "pointer",
                      fontFamily: "inherit",
                    }}
                  >
                    {directory.name}
                    {directory.has_children ? " /" : ""}
                  </button>
                ))
              ) : (
                <div style={{ padding: 10, fontSize: 12, color: "var(--color-textMuted)" }}>
                  Nessuna sottodirectory disponibile.
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </>
  );
}
