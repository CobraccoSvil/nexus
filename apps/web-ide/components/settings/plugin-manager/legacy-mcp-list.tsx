"use client";

import type { McpServer } from "../../../lib/api-client";
import type { Theme } from "../../../lib/theme";
import { detectLegacyMigratableSlug, isNexusBrowserBridgeLocal } from "./plugin-helpers";
import { actionButtonStyle } from "./plugin-styles";
import { useI18n } from "../../../lib/i18n";

const BUILTIN_TOOL_HINTS = ["nexus_mcp_tool_search", "nexus_mcp_tool_call"];

/** Badge "pillola" verde con uppercase. Punto unico (regola L) per i 3 badge
 *  con stesso stile ripetuti nella card del server: "integrato", "integrato",
 *  "migrabile → slug". */
function GreenPill({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <span
      title={title}
      style={{
        fontSize: 10,
        border: "1px solid #22c55e66",
        borderRadius: 999,
        padding: "2px 6px",
        color: "#16a34a",
        textTransform: "uppercase",
      }}
    >
      {children}
    </span>
  );
}

interface LegacyMcpListProps {
  tc: Theme;
  legacyConnectors: McpServer[];
  busyKey: string | null;
  onMigrate: (server: McpServer) => void;
  onDelete: (server: McpServer) => void;
}

export function LegacyMcpList({
  tc,
  legacyConnectors,
  busyKey,
  onMigrate,
  onDelete,
}: LegacyMcpListProps) {
  const { t } = useI18n();
  return (
    <div style={{ display: "grid", gap: 10 }}>
      <div style={{ fontSize: 12, color: tc.textMuted }}>
        MCP legacy rilevati. Se mappabili, puoi migrarli in plugin con un click.
      </div>
      {legacyConnectors.length === 0 && (
        <div style={{ fontSize: 12, color: tc.textMuted }}>
          {t("settings.nessunConnettoreLegacyRilevato")}
        </div>
      )}
      {legacyConnectors.map((server) => {
        const slug = detectLegacyMigratableSlug(server);
        const isBuiltin = (server.transport as string) === "builtin";
        const isNexusBridge = isNexusBrowserBridgeLocal(server);
        return (
          <div
            key={server.id}
            style={{
              border: `1px solid ${tc.border}`,
              borderRadius: 10,
              background: tc.bgCard,
              padding: "10px 12px",
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <div style={{ fontWeight: 700, fontSize: 13, color: tc.text }}>{server.name}</div>
              <span
                style={{
                  fontSize: 10,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 999,
                  padding: "2px 6px",
                  color: tc.textMuted,
                  textTransform: "uppercase",
                }}
              >
                {server.transport}
              </span>
              <span style={{ fontSize: 11, color: tc.textMuted }}>
                {server.enabled ? "enabled" : "disabled"}
              </span>
              {isBuiltin && (
                <GreenPill title={t("settings.mcpIntegratoNelCore")}>
                  integrato
                </GreenPill>
              )}
              {isNexusBridge && !isBuiltin && (
                <GreenPill title={t("settings.connettoreLocaleNexusBrowser")}>
                  integrato
                </GreenPill>
              )}
              {slug && <GreenPill>migrabile → {slug}</GreenPill>}
            </div>
            <div style={{ marginTop: 4, fontSize: 11, color: tc.textMuted }}>
              {server.transport === "http"
                ? server.url
                : `${server.command ?? ""} ${(server.args ?? []).join(" ")}`.trim()}
            </div>
            {isBuiltin && (
              <div style={{ marginTop: 8, display: "grid", gap: 6 }}>
                <div
                  style={{
                    fontSize: 12,
                    color: tc.textMuted,
                    fontWeight: 600,
                  }}
                  title={t("settings.toolEspostiDalServer")}
                >
                  {t("settings.toolDisponibiliNexusBuiltin")}
                </div>
                <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                  {BUILTIN_TOOL_HINTS.map((name) => (
                    <span
                      key={name}
                      style={{
                        fontFamily: "var(--font-mono)",
                        fontSize: 11,
                        padding: "3px 6px",
                        borderRadius: 999,
                        border: `1px solid ${tc.border}`,
                        background: tc.bgInput,
                        color: tc.text,
                      }}
                    >
                      {name}
                    </span>
                  ))}
                </div>
                <div style={{ fontSize: 11, color: tc.textMuted }}>
                  Questi tool servono per scoprire/eseguire tool MCP esterni a runtime (senza inviare tutta la lista al provider).
                </div>
                <div style={{ fontSize: 11, color: tc.textMuted }}>
                  {t("settings.elencoCompleto")} <strong>{t("settings.adminTemplatePromptMcp")}</strong>.
                </div>
              </div>
            )}
            <div style={{ marginTop: 8 }}>
              <div style={{ display: "flex", flexWrap: "wrap", gap: 8, alignItems: "center" }}>
                <button
                  type="button"
                  disabled={isBuiltin || isNexusBridge || !slug || busyKey === `legacy-migrate:${server.id}`}
                  onClick={() => onMigrate(server)}
                  style={actionButtonStyle(
                    tc,
                    isBuiltin || isNexusBridge || !slug || busyKey === `legacy-migrate:${server.id}`,
                  )}
                >
                  {isBuiltin
                    ? "Già integrato"
                    : isNexusBridge
                      ? "Connettore Nexus"
                    : !slug
                      ? "Non migrabile automaticamente"
                    : busyKey === `legacy-migrate:${server.id}`
                      ? "Migrazione..."
                      : "Migra a plugin"}
                </button>
                <button
                  type="button"
                  disabled={isBuiltin || busyKey === `legacy-delete:${server.id}`}
                  onClick={() => onDelete(server)}
                  style={actionButtonStyle(tc, isBuiltin || busyKey === `legacy-delete:${server.id}`)}
                >
                  {isBuiltin ? "Integrato" : busyKey === `legacy-delete:${server.id}` ? "Elimino..." : "Elimina MCP"}
                </button>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
