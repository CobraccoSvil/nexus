"use client";

import React, { useCallback, useEffect, useState } from "react";
import { useTheme, useThemeColors } from "../../lib/theme";
import type { SettingEntry } from "./provider-settings";
import type { EnvironmentCheck } from "../../lib/api-client";
import { getEnvironmentStatus, fixEnvironment } from "../../lib/api-client";

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";
// Proxy via Next.js /neural/* → mcp-core :4000 /api/neural/* (il brain Python
// e' stato eliminato; gli endpoint neural vivono ora in mcp-core).
const NEURAL_BASE = "/neural";

// ── Microservice definitions ──────────────────────────────────────────────────

interface ServiceDef {
  id: string;
  name: string;
  description: string;
  icon: string;
  healthUrl: string;
  /** Settings keys associated with this service */
  settingKeys: string[];
  /** How to parse health response */
  parseHealth: (data: Record<string, unknown>) => ServiceHealth;
}

interface ServiceHealth {
  status: "ok" | "degraded" | "down" | "checking";
  version?: string;
  details?: Record<string, string | boolean | number>;
}

const SERVICES: ServiceDef[] = [
  {
    id: "mcp-core",
    name: "MCP Core",
    description: "Orchestratore centrale — gestisce chat, agent loop, tool, progetti e sessioni.",
    icon: "\u2699\uFE0F",
    healthUrl: `${API_BASE}/api/health`,
    settingKeys: [],
    parseHealth: (d) => ({
      status: d.status === "ok" ? "ok" : "down",
      version: String(d.version || ""),
      details: {
        database: Boolean((d.components as Record<string, boolean>)?.database),
        redis: Boolean((d.components as Record<string, boolean>)?.redis),
        neural_core: Boolean((d.components as Record<string, boolean>)?.neural_core),
      },
    }),
  },
  {
    id: "neural-core",
    name: "Neural Core (mcp-core)",
    description: "Endpoint AI in mcp-core (Rust): routing LLM, classificazione intent, health provider. Esposti sotto /api/neural.",
    icon: "\uD83E\uDDE0",
    healthUrl: `${NEURAL_BASE}/health`,
    settingKeys: [],
    parseHealth: (d) => ({
      status: d.status === "ok" ? "ok" : "down",
      version: String(d.version || ""),
    }),
  },
  {
    id: "postgresql",
    name: "PostgreSQL",
    description: "Database relazionale — utenti, progetti, sessioni, settings, documenti.",
    icon: "\uD83D\uDDC4\uFE0F",
    // Checked via mcp-core health (components.database)
    healthUrl: `${API_BASE}/api/health`,
    settingKeys: [],
    parseHealth: (d) => {
      const db = Boolean((d.components as Record<string, boolean>)?.database);
      return { status: db ? "ok" : "down", details: { connected: db } };
    },
  },
  {
    id: "redis",
    name: "Redis",
    description: "Cache e sessioni — gestione token, caching risposte, pub/sub eventi.",
    icon: "\u26A1",
    healthUrl: `${API_BASE}/api/health`,
    settingKeys: ["redis_url"],
    parseHealth: (d) => {
      const redis = Boolean((d.components as Record<string, boolean>)?.redis);
      return { status: redis ? "ok" : "down", details: { connected: redis } };
    },
  },
  {
    id: "qdrant",
    name: "Qdrant",
    description: "Database vettoriale — embedding del codice, contesto progetto, ricerca semantica.",
    icon: "\uD83D\uDD0D",
    healthUrl: `${API_BASE}/api/health`,
    settingKeys: ["qdrant_url", "qdrant_collection", "qdrant_project_context_collection"],
    parseHealth: () => ({ status: "checking" }), // Special: checked separately
  },
  {
    id: "watchdog",
    name: "Task Watchdog",
    description: "Monitoraggio centralizzato — controlla dipendenze (Qdrant, embedder) e rileva task bloccati.",
    icon: "👁️",
    healthUrl: `${API_BASE}/api/admin/watchdog-status`,
    settingKeys: [],
    parseHealth: (d) => {
      const deps = d.dependencies as Record<string, Record<string, unknown>> | undefined;
      const qdrantOk = deps?.qdrant?.healthy === true;
      const embedderOk = deps?.embedder?.healthy === true;
      const allOk = qdrantOk && embedderOk;
      return {
        status: allOk ? "ok" : "degraded",
        details: {
          qdrant: qdrantOk,
          embedder: embedderOk,
          qdrant_latency_ms: Number(deps?.qdrant?.latency_ms ?? 0),
          embedder_latency_ms: Number(deps?.embedder?.latency_ms ?? 0),
        },
      };
    },
  },
];

// ── Component ─────────────────────────────────────────────────────────────────

interface InfrastructureSettingsProps {
  items: SettingEntry[];
  editValues: Record<string, string>;
  saving: Record<string, boolean>;
  saved: Record<string, boolean>;
  onEditChange: (key: string, value: string) => void;
  onSave: (key: string) => void;
  /** For browsing directories (projects_base_root) */
  onOpenBrowse?: (currentValue: string) => void;
  /** Salvataggio immediato senza passare per editValues (usato dai toggle) */
  onSaveImmediate?: (key: string, value: string) => void;
}

export function InfrastructureSettings({
  items,
  editValues,
  saving,
  saved,
  onEditChange,
  onSave,
  onOpenBrowse,
  onSaveImmediate,
}: InfrastructureSettingsProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();
  const [healthMap, setHealthMap] = useState<Record<string, ServiceHealth>>({});
  const [qdrantCollections, setQdrantCollections] = useState<string[]>([]);
  const [refreshing, setRefreshing] = useState(false);

  const checkAllHealth = useCallback(async () => {
    setRefreshing(true);
    const newHealth: Record<string, ServiceHealth> = {};

    // Check main services
    try {
      const res = await fetch(`${API_BASE}/api/health`, { credentials: "include" });
      const data = await res.json();

      // MCP Core
      newHealth["mcp-core"] = SERVICES[0].parseHealth(data);
      // PostgreSQL (from mcp-core components)
      newHealth["postgresql"] = SERVICES[2].parseHealth(data);
      // Redis (from mcp-core components)
      newHealth["redis"] = SERVICES[3].parseHealth(data);
    } catch {
      newHealth["mcp-core"] = { status: "down" };
      newHealth["postgresql"] = { status: "down" };
      newHealth["redis"] = { status: "down" };
    }

    // Neural Core
    try {
      const res = await fetch(`${NEURAL_BASE}/health`);
      const data = await res.json();
      newHealth["neural-core"] = SERVICES[1].parseHealth(data);
    } catch {
      newHealth["neural-core"] = { status: "down" };
    }

    // Qdrant — check via proxy or direct
    try {
      const _qdrantUrl = items.find((s) => s.key === "qdrant_url")?.value || "http://localhost:6333";
      // Try via admin API proxy first
      const res = await fetch(`${API_BASE}/api/admin/qdrant-health`, { credentials: "include" });
      if (res.ok) {
        const data = await res.json();
        newHealth["qdrant"] = { status: data.healthy ? "ok" : "down", details: data };
        if (data.collections !== undefined) setQdrantCollections(data.collections);
      } else {
        // Endpoint HTTP error (es. 500) — mostra stato reale
        newHealth["qdrant"] = { status: "down", details: { error: `HTTP ${res.status}` } };
      }
    } catch {
      // Errore di rete — mcp-core non raggiungibile
      newHealth["qdrant"] = { status: "down", details: { error: "mcp-core non raggiungibile" } };
    }

    // Task Watchdog
    try {
      const res = await fetch(`${API_BASE}/api/admin/watchdog-status`, { credentials: "include" });
      if (res.ok) {
        const data = await res.json();
        const wdService = SERVICES.find(s => s.id === "watchdog");
        if (wdService) newHealth["watchdog"] = wdService.parseHealth(data);
      } else {
        newHealth["watchdog"] = { status: "down" };
      }
    } catch {
      newHealth["watchdog"] = { status: "down" };
    }

    setHealthMap(newHealth);
    setRefreshing(false);
  }, [items]);

  useEffect(() => {
    void checkAllHealth();
    const timer = setInterval(() => void checkAllHealth(), 30_000);
    return () => clearInterval(timer);
  }, [checkAllHealth]);

  const getStatusDot = (status: ServiceHealth["status"]) => {
    const colors = {
      ok: "#22c55e",
      degraded: "#f59e0b",
      down: "#ef4444",
      checking: tc.textMuted,
    };
    return (
      <span
        style={{
          display: "inline-block",
          width: 10,
          height: 10,
          borderRadius: "50%",
          background: colors[status],
          boxShadow: status === "ok" ? "0 0 6px #22c55e60" : undefined,
          flexShrink: 0,
        }}
      />
    );
  };

  const getStatusLabel = (status: ServiceHealth["status"]) => {
    switch (status) {
      case "ok": return "Operativo";
      case "degraded": return "Degradato";
      case "down": return "Non raggiungibile";
      case "checking": return "Verifica...";
    }
  };

  // Settings not associated with any service
  const orphanKeys = new Set(items.map((s) => s.key));
  for (const svc of SERVICES) {
    for (const k of svc.settingKeys) orphanKeys.delete(k);
  }
  // Always show projects_base_root separately
  orphanKeys.delete("projects_base_root");

  const projectsBaseRoot = items.find((s) => s.key === "projects_base_root");

  const cardBg = resolved === "dark" ? "#1e1e2e" : "#f8f9fb";
  const cardBorder = resolved === "dark" ? "#2e2e3e" : "#e5e7eb";

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      {/* ── Refresh button ──────────────────────────────────────────────── */}
      <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
        <button
          type="button"
          onClick={() => void checkAllHealth()}
          disabled={refreshing}
          style={{
            border: "1px solid var(--color-border)",
            background: "var(--color-bgCard)",
            color: "var(--color-text)",
            borderRadius: 8,
            padding: "6px 14px",
            fontSize: 12,
            cursor: refreshing ? "wait" : "pointer",
            opacity: refreshing ? 0.6 : 1,
          }}
        >
          {refreshing ? "\u21BB Verifica in corso..." : "\u21BB Verifica tutti i servizi"}
        </button>
        <span style={{ fontSize: 11, color: "var(--color-textMuted)" }}>
          Aggiornamento automatico ogni 30s
        </span>
      </div>

      {/* ── Projects base root (standalone) ────────────────────────────── */}
      {projectsBaseRoot && (
        <div
          style={{
            background: cardBg,
            border: `1px solid ${cardBorder}`,
            borderRadius: 12,
            padding: "16px 20px",
          }}
        >
          <div className="flex-row" style={{ justifyContent: "space-between", marginBottom: 8 }}>
            <div>
              <div className="text-base font-semibold" style={{ color: tc.text }}>
                \uD83D\uDCC1 {projectsBaseRoot.key}
              </div>
              <div className="text-xs" style={{ color: tc.textMuted, marginTop: 2 }}>
                {projectsBaseRoot.description}
              </div>
            </div>
            {onOpenBrowse && (
              <button
                type="button"
                onClick={() => onOpenBrowse(editValues[projectsBaseRoot.key] ?? projectsBaseRoot.value)}
                style={{
                  border: `1px solid ${tc.border}`,
                  background: tc.bgCard,
                  color: tc.textSecondary,
                  borderRadius: 6,
                  padding: "4px 10px",
                  fontSize: 11,
                  cursor: "pointer",
                }}
              >
                Sfoglia
              </button>
            )}
          </div>
          {renderSettingInput(projectsBaseRoot)}
        </div>
      )}

      {/* ── Service cards ───────────────────────────────────────────────── */}
      {SERVICES.map((svc) => {
        const health = healthMap[svc.id] || { status: "checking" as const };
        const svcSettings = svc.settingKeys
          .map((k) => items.find((s) => s.key === k))
          .filter(Boolean) as SettingEntry[];

        return (
          <div
            key={svc.id}
            style={{
              background: cardBg,
              border: `1px solid ${health.status === "down" ? `${tc.error}60` : cardBorder}`,
              borderRadius: 12,
              padding: "16px 20px",
              transition: "border-color 0.2s",
            }}
          >
            {/* Header */}
            <div className="flex-row-gap-10" style={{ marginBottom: svcSettings.length > 0 ? 14 : 0 }}>
              <span className="text-2xl">{svc.icon}</span>
              <div className="flex-1" style={{ minWidth: 0 }}>
                <div className="text-lg font-bold" style={{ color: tc.text }}>{svc.name}</div>
                <div className="text-xs" style={{ color: tc.textMuted, marginTop: 1 }}>{svc.description}</div>
              </div>
              <div
                className="flex-row-gap-6"
                style={{
                  padding: "4px 10px",
                  borderRadius: 999,
                  background: health.status === "ok"
                    ? (resolved === "dark" ? "#22c55e18" : "#22c55e12")
                    : health.status === "down"
                    ? (resolved === "dark" ? "#ef444418" : "#ef444412")
                    : "transparent",
                  border: `1px solid ${health.status === "ok" ? "#22c55e40" : health.status === "down" ? "#ef444440" : tc.border}`,
                  flexShrink: 0,
                }}
              >
                {getStatusDot(health.status)}
                <span style={{ fontSize: 11, fontWeight: 600, color: health.status === "ok" ? "#22c55e" : health.status === "down" ? tc.error : tc.textMuted }}>
                  {getStatusLabel(health.status)}
                </span>
              </div>
            </div>

            {/* Version & details */}
            {(health.version || (health.details && Object.keys(health.details).length > 0)) && (
              <div
                style={{
                  display: "flex",
                  flexWrap: "wrap",
                  gap: 8,
                  marginBottom: svcSettings.length > 0 ? 12 : 0,
                  marginTop: 6,
                }}
              >
                {health.version && (
                  <span style={tagStyle(tc, resolved)}>v{health.version}</span>
                )}
                {health.details && Object.entries(health.details).map(([k, v]) => (
                  <span key={k} style={tagStyle(tc, resolved, typeof v === "boolean" ? (v ? "#22c55e" : "#ef4444") : undefined)}>
                    {k}: {String(v)}
                  </span>
                ))}
                {svc.id === "qdrant" && qdrantCollections.length > 0 && (
                  <span style={tagStyle(tc, resolved)}>
                    {qdrantCollections.length} collections
                  </span>
                )}
              </div>
            )}

            {/* Settings for this service */}
            {svcSettings.length > 0 && (
              <div className="flex-col-gap-10" style={{ marginTop: 6 }}>
                {svcSettings.map((setting) => (
                  <div key={setting.key}>
                    <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 3, fontWeight: 500 }}>
                      {setting.key}
                      {setting.description && (
                        <span style={{ fontWeight: 400, marginLeft: 6 }}>— {setting.description}</span>
                      )}
                    </div>
                    {renderSettingInput(setting)}
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}

      {/* ── Environment checks ─────────────────────────────────────────── */}
      <EnvironmentSection cardBg={cardBg} cardBorder={cardBorder} />

      {/* ── Other infrastructure settings ──────────────────────────────── */}
      {Array.from(orphanKeys).length > 0 && (
        <ServiceUrlSettings
          orphanKeys={Array.from(orphanKeys)}
          items={items}
          cardBg={cardBg}
          cardBorder={cardBorder}
          renderSettingInput={renderSettingInput}
        />
      )}
    </div>
  );

  function renderSettingInput(setting: SettingEntry) {
    const currentValue = editValues[setting.key] ?? setting.value;
    const isEdited = editValues[setting.key] !== undefined && editValues[setting.key] !== setting.value;
    const isSaving = saving[setting.key] ?? false;
    const isSaved = saved[setting.key] ?? false;

    // Toggle per valori booleani (true/false)
    if ((currentValue === "true" || currentValue === "false") && !setting.is_secret) {
      return (
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginTop: 4 }}>
          <button
            onClick={() => {
              const newVal = currentValue === "true" ? "false" : "true";
              if (onSaveImmediate) {
                void onSaveImmediate(setting.key, newVal);
              } else {
                onEditChange(setting.key, newVal);
                setTimeout(() => onSave(setting.key), 50);
              }
            }}
            disabled={isSaving}
            style={{
              width: 44, height: 24, borderRadius: 12, border: "none",
              background: currentValue === "true" ? tc.success : tc.bgInput,
              cursor: isSaving ? "not-allowed" : "pointer",
              position: "relative", transition: "background 0.2s",
              flexShrink: 0, outline: `1px solid ${tc.border}`,
              opacity: isSaving ? 0.7 : 1,
            }}
            title={currentValue === "true" ? "Attivo — clicca per disabilitare" : "Non attivo — clicca per abilitare"}
          >
            <span style={{
              position: "absolute", top: 3,
              left: currentValue === "true" ? 23 : 3,
              width: 18, height: 18, borderRadius: "50%",
              background: "#fff", transition: "left 0.2s",
              boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
            }} />
          </button>
          <span style={{ fontSize: 12, color: currentValue === "true" ? tc.success : "var(--color-textMuted)" }}>
            {currentValue === "true" ? "ON" : "OFF"}
          </span>
          {isSaving && <span style={{ fontSize: 10, color: "var(--color-textMuted)" }}>...</span>}
          {isSaved && <span style={{ fontSize: 10, color: tc.success }}>✓</span>}
        </div>
      );
    }

    return (
      <div className="flex-row" style={{ gap: 8, alignItems: "center" }}>
        <input
          type={setting.is_secret ? "password" : "text"}
          value={currentValue}
          onChange={(e) => onEditChange(setting.key, e.target.value)}
          style={{
            flex: 1,
            minWidth: 0,
            background: resolved === "dark" ? "#14141e" : "#fff",
            border: `1px solid ${isEdited ? tc.accent : tc.border}`,
            borderRadius: 6,
            padding: "7px 10px",
            color: tc.text,
            fontSize: 13,
            fontFamily: "var(--font-mono)",
            outline: "none",
          }}
        />
        {isEdited && (
          <button
            type="button"
            onClick={() => onSave(setting.key)}
            disabled={saving[setting.key]}
            style={{
              border: `1px solid ${tc.accent}`,
              background: `${tc.accent}18`,
              color: tc.accent,
              borderRadius: 6,
              padding: "6px 14px",
              fontSize: 12,
              cursor: "pointer",
              fontWeight: 600,
              opacity: saving[setting.key] ? 0.5 : 1,
              whiteSpace: "nowrap",
            }}
          >
            {saving[setting.key] ? "..." : "Salva"}
          </button>
        )}
        {saved[setting.key] && (
          <span style={{ color: "#22c55e", fontSize: 12, fontWeight: 600 }}>✓</span>
        )}
      </div>
    );
  }
}

// ── Service URL Settings (con status dot) ─────────────────────────────────────

// Mappa: setting key → check ID restituito da /api/admin/environment/status
const SERVICE_URL_CHECK_MAP: Record<string, string> = {
  mcp_core_url:      "svc_mcp_core",
  admin_service_url: "svc_admin",
  doc_service_url:   "svc_doc",
  billing_service_url: "svc_billing",
  plugin_service_url:  "svc_plugin",
};

function ServiceUrlSettings({
  orphanKeys, items, cardBg, cardBorder, renderSettingInput,
}: {
  orphanKeys: string[];
  items: SettingEntry[];
  cardBg: string;
  cardBorder: string;
  renderSettingInput: (s: SettingEntry) => React.ReactNode;
}) {
  const tc = useThemeColors();
  const [statusMap, setStatusMap] = useState<Record<string, EnvironmentCheck>>({});

  useEffect(() => {
    getEnvironmentStatus().then((res) => {
      const map: Record<string, EnvironmentCheck> = {};
      for (const c of res.checks) map[c.id] = c;
      setStatusMap(map);
    }).catch(() => {/* ignore */});
  }, []);

  const dotColor = (status: EnvironmentCheck["status"]) => {
    switch (status) {
      case "ok": return "#22c55e";
      case "warn": return "#f59e0b";
      case "error": return "#ef4444";
      default: return tc.textMuted;
    }
  };

  return (
    <div style={{ background: cardBg, border: `1px solid ${cardBorder}`, borderRadius: 12, padding: "16px 20px" }}>
      <div style={{ fontWeight: 700, fontSize: 14, color: tc.text, marginBottom: 14 }}>
        ⚙️ Altre impostazioni
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {orphanKeys.map((key) => {
          const setting = items.find((s) => s.key === key);
          if (!setting) return null;
          const checkId = SERVICE_URL_CHECK_MAP[key];
          const check = checkId ? statusMap[checkId] : undefined;
          return (
            <div key={key}>
              <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 3, fontWeight: 500, display: "flex", alignItems: "center", gap: 6 }}>
                {check && (
                  <span
                    title={check.detail}
                    style={{
                      display: "inline-block", width: 8, height: 8,
                      borderRadius: "50%", background: dotColor(check.status),
                      flexShrink: 0,
                      boxShadow: check.status === "ok" ? "0 0 4px #22c55e60" : undefined,
                    }}
                  />
                )}
                <span>{setting.key}</span>
                {setting.description && (
                  <span style={{ fontWeight: 400 }}>— {setting.description}</span>
                )}
                {check && check.status !== "ok" && (
                  <span style={{ color: dotColor(check.status), fontSize: 10, fontWeight: 600 }}>
                    ({check.detail})
                  </span>
                )}
              </div>
              {renderSettingInput(setting)}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ── Environment Section ───────────────────────────────────────────────────────

type EnvTc = ReturnType<typeof useThemeColors>;

function envStatusIcon(status: EnvironmentCheck["status"]): string {
  switch (status) {
    case "ok": return "✅";
    case "warn": return "⚠️";
    case "error": return "❌";
    case "loading": return "⏳";
    default: return "❓";
  }
}

function envStatusColor(status: EnvironmentCheck["status"], tc: EnvTc): string {
  switch (status) {
    case "ok": return "#22c55e";
    case "warn": return "#f59e0b";
    case "error": return tc.error;
    default: return tc.textMuted;
  }
}

function EnvCheckRow({
  check, fixLoading, fixOutputs, onFix, onSudoInstall, tc,
}: {
  check: EnvironmentCheck;
  fixLoading: Record<string, boolean>;
  fixOutputs: Record<string, string>;
  onFix: (action: string, checkId: string) => Promise<void>;
  onSudoInstall: (action: string, checkId: string) => void;
  tc: EnvTc;
}) {
  const [cmdExpanded, setCmdExpanded] = useState(false);
  const showFix = check.status === "error" || check.status === "warn";

  return (
    <div style={{ padding: "12px 16px", borderBottom: `1px solid ${tc.border}` }}>
      <div style={{ display: "flex", alignItems: "flex-start", gap: 10 }}>
        <span style={{ fontSize: 16, lineHeight: "20px", flexShrink: 0 }}>{envStatusIcon(check.status)}</span>
        <div style={{ flex: 1 }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <span style={{ fontWeight: 600, fontSize: 13, color: tc.text }}>{check.label}</span>
            <span style={{ fontSize: 12, color: envStatusColor(check.status, tc), fontWeight: check.status !== "ok" ? 500 : 400 }}>
              {check.detail}
            </span>
          </div>

          {showFix && check.id === "playwright_libs" && (
            <div style={{ marginTop: 8, display: "flex", gap: 8, flexWrap: "wrap" }}>
              <button
                onClick={() => onSudoInstall("install_system_deps", check.id)}
                disabled={fixLoading[check.id] ?? false}
                style={{ padding: "4px 12px", fontSize: 12, borderRadius: 6, border: `1px solid ${tc.border}`, background: tc.bgSidebar, color: tc.text, cursor: "pointer" }}
              >
                🔧 Installa auto
              </button>
              <button
                onClick={async () => {
                  if (!fixOutputs[`${check.id}_cmd`]) {
                    await onFix("get_system_deps_command", `${check.id}_cmd`);
                  }
                  setCmdExpanded(v => !v);
                }}
                style={{ padding: "4px 12px", fontSize: 12, borderRadius: 6, border: `1px solid ${tc.border}`, background: tc.bgSidebar, color: tc.text, cursor: "pointer" }}
              >
                {fixLoading[`${check.id}_cmd`] ? "⏳" : "📋 Mostra comando"}
              </button>
              {cmdExpanded && fixOutputs[`${check.id}_cmd`] && (
                <div style={{ width: "100%", marginTop: 4 }}>
                  <pre style={{ padding: "8px 12px", background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 6, fontSize: 11, whiteSpace: "pre-wrap", wordBreak: "break-all", color: tc.textSecondary }}>
                    {fixOutputs[`${check.id}_cmd`]}
                  </pre>
                  <button
                    onClick={() => void navigator.clipboard.writeText(fixOutputs[`${check.id}_cmd`])}
                    style={{ marginTop: 4, padding: "2px 10px", fontSize: 11, borderRadius: 4, border: `1px solid ${tc.border}`, background: tc.bgSidebar, color: tc.textMuted, cursor: "pointer" }}
                  >
                    Copia
                  </button>
                </div>
              )}
              {fixOutputs[check.id] && (
                <pre style={{ width: "100%", marginTop: 8, padding: "8px 12px", background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 6, fontSize: 11, whiteSpace: "pre-wrap", wordBreak: "break-all", maxHeight: 200, overflowY: "auto", color: tc.textSecondary }}>
                  {fixOutputs[check.id]}
                </pre>
              )}
            </div>
          )}

          {showFix && check.id === "playwright_browser" && (
            <div style={{ marginTop: 8 }}>
              <button
                onClick={() => void onFix("install_playwright_browsers", check.id)}
                disabled={fixLoading[check.id] ?? false}
                style={{ padding: "4px 12px", fontSize: 12, borderRadius: 6, border: `1px solid ${tc.border}`, background: tc.bgSidebar, color: tc.text, cursor: fixLoading[check.id] ? "not-allowed" : "pointer", opacity: fixLoading[check.id] ? 0.7 : 1 }}
              >
                {fixLoading[check.id] ? "⏳ In corso..." : "⬇️ Installa Chromium"}
              </button>
              {fixOutputs[check.id] && (
                <pre style={{ marginTop: 8, padding: "8px 12px", background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 6, fontSize: 11, whiteSpace: "pre-wrap", wordBreak: "break-all", maxHeight: 200, overflowY: "auto", color: tc.textSecondary }}>
                  {fixOutputs[check.id]}
                </pre>
              )}
            </div>
          )}

          {showFix && check.id === "migrations" && (
            <div style={{ marginTop: 8 }}>
              <button
                onClick={() => void onFix("run_migrations", check.id)}
                disabled={fixLoading[check.id] ?? false}
                style={{ padding: "4px 12px", fontSize: 12, borderRadius: 6, border: `1px solid ${tc.border}`, background: tc.bgSidebar, color: tc.text, cursor: fixLoading[check.id] ? "not-allowed" : "pointer", opacity: fixLoading[check.id] ? 0.7 : 1 }}
              >
                {fixLoading[check.id] ? "⏳ In corso..." : "🔄 Esegui migrazioni"}
              </button>
              {fixOutputs[check.id] && (
                <pre style={{ marginTop: 8, padding: "8px 12px", background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 6, fontSize: 11, whiteSpace: "pre-wrap", wordBreak: "break-all", maxHeight: 200, overflowY: "auto", color: tc.textSecondary }}>
                  {fixOutputs[check.id]}
                </pre>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function EnvironmentSection({ cardBg, cardBorder }: { cardBg: string; cardBorder: string }) {
  const tc = useThemeColors();
  const [checks, setChecks] = useState<EnvironmentCheck[]>([]);
  const [loading, setLoading] = useState(false);
  const [fixOutputs, setFixOutputs] = useState<Record<string, string>>({});
  const [fixLoading, setFixLoading] = useState<Record<string, boolean>>({});
  const [lastCheck, setLastCheck] = useState<Date | null>(null);
  const [envError, setEnvError] = useState<string | null>(null);
  const [sudoModal, setSudoModal] = useState<{ action: string; checkId: string } | null>(null);
  const [sudoPassword, setSudoPassword] = useState("");
  const [sudoError, setSudoError] = useState("");

  const refresh = async () => {
    setLoading(true);
    setChecks(prev => prev.length > 0 ? prev.map(c => ({ ...c, status: "loading" as const })) : []);
    setEnvError(null);
    try {
      const res = await getEnvironmentStatus();
      setChecks(res.checks);
      setLastCheck(new Date());
    } catch (e) {
      setEnvError(e instanceof Error ? e.message : "Errore nel recupero dello stato ambiente");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { void refresh(); }, []);

  const runFix = async (action: string, checkId: string) => {
    setFixLoading(prev => ({ ...prev, [checkId]: true }));
    try {
      const res = await fixEnvironment(action);
      setFixOutputs(prev => ({ ...prev, [checkId]: res.output }));
      await refresh();
    } catch (e) {
      setFixOutputs(prev => ({ ...prev, [checkId]: e instanceof Error ? e.message : "Errore sconosciuto" }));
    } finally {
      setFixLoading(prev => ({ ...prev, [checkId]: false }));
    }
  };

  const handleSudoConfirm = async () => {
    if (!sudoModal || !sudoPassword) return;
    setSudoError("");
    setFixLoading(prev => ({ ...prev, [sudoModal.checkId]: true }));
    try {
      const res = await fixEnvironment(sudoModal.action, sudoPassword);
      if (!res.ok && /incorrect password|authentication failure|Sorry, try again/.test(res.output)) {
        setSudoError("Password errata. Riprova.");
        return;
      }
      setFixOutputs(prev => ({ ...prev, [sudoModal.checkId]: res.output }));
      setSudoModal(null);
      setSudoPassword("");
      await refresh();
    } catch {
      setSudoError("Errore durante l'installazione.");
    } finally {
      setFixLoading(prev => ({ ...prev, [sudoModal!.checkId]: false }));
    }
  };

  const okCount = checks.filter(c => c.status === "ok").length;
  const errorCount = checks.filter(c => c.status === "error").length;
  const warnCount = checks.filter(c => c.status === "warn").length;
  const hasIssues = errorCount > 0 || warnCount > 0;

  return (
    <>
      <div style={{ background: cardBg, border: `1px solid ${hasIssues ? (errorCount > 0 ? "#ef444460" : "#f59e0b60") : cardBorder}`, borderRadius: 12, padding: "16px 20px" }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: checks.length > 0 ? 14 : 0 }}>
          <div>
            <div style={{ fontWeight: 700, fontSize: 14, color: tc.text }}>🛡️ Stato ambiente Nexus</div>
            <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>
              Dipendenze di sistema, migrazioni DB, browser Playwright.
              {lastCheck && (
                <span style={{ marginLeft: 8 }}>
                  Aggiornato: {lastCheck.toLocaleTimeString()}
                  {checks.length > 0 && (
                    <>
                      <span style={{ color: "#22c55e", marginLeft: 8 }}>{okCount} ok</span>
                      {warnCount > 0 && <span style={{ color: "#f59e0b", marginLeft: 6 }}>{warnCount} avvisi</span>}
                      {errorCount > 0 && <span style={{ color: tc.error, marginLeft: 6 }}>{errorCount} errori</span>}
                    </>
                  )}
                </span>
              )}
            </div>
          </div>
          <button
            type="button"
            onClick={() => void refresh()}
            disabled={loading}
            style={{ border: `1px solid ${tc.border}`, background: tc.bgCard, color: tc.text, borderRadius: 8, padding: "5px 12px", fontSize: 12, cursor: loading ? "wait" : "pointer", opacity: loading ? 0.6 : 1, whiteSpace: "nowrap" }}
          >
            {loading ? "⏳ Aggiornamento..." : "🔄 Aggiorna"}
          </button>
        </div>

        {envError && (
          <div style={{ padding: "8px 14px", background: "#2d1215", border: `1px solid ${tc.error}`, borderRadius: 8, color: tc.error, fontSize: 12, marginBottom: 12 }}>
            {envError}
          </div>
        )}

        {checks.length > 0 && (
          <div style={{ border: `1px solid ${tc.border}`, borderRadius: 8, overflow: "hidden" }}>
            {checks.map(check => (
              <EnvCheckRow
                key={check.id}
                check={check}
                fixLoading={fixLoading}
                fixOutputs={fixOutputs}
                onFix={runFix}
                onSudoInstall={(action, checkId) => setSudoModal({ action, checkId })}
                tc={tc}
              />
            ))}
          </div>
        )}

        {checks.length === 0 && !loading && !envError && (
          <div style={{ padding: "20px 0", textAlign: "center", color: tc.textMuted, fontSize: 12 }}>
            Clicca Aggiorna per verificare l&apos;ambiente.
          </div>
        )}
      </div>

      {sudoModal && (
        <div style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.6)", display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1000 }}>
          <div style={{ background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 12, padding: 24, width: 380, maxWidth: "90vw" }}>
            <h3 style={{ margin: "0 0 8px", color: tc.text }}>Password sudo richiesta</h3>
            <p style={{ color: tc.textMuted, fontSize: 13, margin: "0 0 16px" }}>
              Per installare le dipendenze di sistema è necessaria la password sudo del server. La password non viene salvata.
            </p>
            <input
              type="password"
              value={sudoPassword}
              onChange={e => { setSudoPassword(e.target.value); setSudoError(""); }}
              onKeyDown={e => { if (e.key === "Enter") void handleSudoConfirm(); }}
              placeholder="Password sudo..."
              autoFocus
              style={{ width: "100%", boxSizing: "border-box", padding: "8px 12px", borderRadius: 6, border: `1px solid ${sudoError ? tc.error : tc.border}`, background: tc.bgSidebar, color: tc.text, fontSize: 13, marginBottom: sudoError ? 6 : 16 }}
            />
            {sudoError && <div style={{ color: tc.error, fontSize: 12, marginBottom: 12 }}>{sudoError}</div>}
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button onClick={() => { setSudoModal(null); setSudoPassword(""); setSudoError(""); }} style={{ padding: "6px 14px", borderRadius: 6, border: `1px solid ${tc.border}`, background: "none", color: tc.textMuted, cursor: "pointer" }}>
                Annulla
              </button>
              <button
                onClick={() => void handleSudoConfirm()}
                disabled={!sudoPassword || (fixLoading[sudoModal.checkId] ?? false)}
                style={{ padding: "6px 14px", borderRadius: 6, border: "none", background: tc.accent, color: "#fff", cursor: "pointer" }}
              >
                {(fixLoading[sudoModal.checkId] ?? false) ? "Installando..." : "Installa"}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function tagStyle(
  tc: ReturnType<typeof useThemeColors>,
  resolved: string,
  accentColor?: string,
): React.CSSProperties {
  return {
    display: "inline-flex",
    alignItems: "center",
    gap: 4,
    padding: "2px 8px",
    borderRadius: 999,
    fontSize: 10,
    fontWeight: 500,
    fontFamily: "var(--font-mono)",
    background: accentColor
      ? `${accentColor}14`
      : resolved === "dark" ? "#ffffff0a" : "#0000000a",
    color: accentColor || tc.textMuted,
    border: `1px solid ${accentColor ? `${accentColor}30` : tc.border}`,
  };
}
