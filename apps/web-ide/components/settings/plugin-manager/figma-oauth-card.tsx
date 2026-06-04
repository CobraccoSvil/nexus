"use client";

import type { FigmaOAuthStatus } from "../../../lib/api-client";
import type { Theme } from "../../../lib/theme";
import { actionButtonStyle } from "./plugin-styles";

interface FigmaOAuthCardProps {
  tc: Theme;
  figmaOAuthStatus: FigmaOAuthStatus | null;
  figmaPreferStdio: boolean;
  busyKey: string | null;
  onStartOAuth: () => void;
  onToggleStdioFallback: (enabled: boolean) => void;
}

export function FigmaOAuthCard({
  tc,
  figmaOAuthStatus,
  figmaPreferStdio,
  busyKey,
  onStartOAuth,
  onToggleStdioFallback,
}: FigmaOAuthCardProps) {
  return (
    <div className="card-sm flex-col-gap-8" style={{ marginBottom: 14 }}>
      <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
        <div className="text-base font-bold">
          Figma MCP OAuth
        </div>
        <span
          style={{
            fontSize: 10,
            padding: "2px 6px",
            borderRadius: 999,
            border: `1px solid ${figmaOAuthStatus?.configured ? "#22c55e66" : tc.warning}`,
            color: figmaOAuthStatus?.configured ? "#16a34a" : tc.warning,
            textTransform: "uppercase",
            fontWeight: 700,
          }}
        >
          {figmaOAuthStatus?.configured ? "client ok" : "client mancante"}
        </span>
        <span
          style={{
            fontSize: 10,
            padding: "2px 6px",
            borderRadius: 999,
            border: `1px solid ${figmaOAuthStatus?.hasAccessToken ? "#22c55e66" : tc.border}`,
            color: figmaOAuthStatus?.hasAccessToken ? "#16a34a" : tc.textMuted,
            textTransform: "uppercase",
            fontWeight: 700,
          }}
        >
          {figmaOAuthStatus?.hasAccessToken
            ? `token ${figmaOAuthStatus.tokenType === "pat" ? "PAT" : "OAuth"}`
            : "token assente"}
        </span>
        {figmaOAuthStatus?.tokenScope && (
          <span style={{ fontSize: 11, color: tc.textMuted }}>
            scope: {figmaOAuthStatus.tokenScope}
          </span>
        )}
      </div>
      <div className="text-sm text-muted">
        Se usi Figma MCP remoto serve OAuth con scope <code>mcp:connect</code>. In alternativa puoi usare il fallback
        locale <code>figma-developer-mcp</code>.
      </div>
      {figmaOAuthStatus?.lastError && (
        <div className="text-sm" style={{ color: tc.error }}>
          Ultimo errore OAuth: {figmaOAuthStatus.lastError}
        </div>
      )}
      <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
        <button
          type="button"
          onClick={() => onStartOAuth()}
          disabled={busyKey === "figma-oauth-connect" || !(figmaOAuthStatus?.configured ?? false)}
          className="btn btn-primary"
          style={actionButtonStyle(
            tc,
            busyKey === "figma-oauth-connect" || !(figmaOAuthStatus?.configured ?? false),
          )}
          title={
            figmaOAuthStatus?.configured
              ? "Connetti Figma con OAuth (mcp:connect)"
              : "Configura prima figma_client_id e figma_client_secret"
          }
        >
          {busyKey === "figma-oauth-connect" ? "Avvio OAuth..." : "Connetti OAuth"}
        </button>
        <button
          type="button"
          onClick={() => onToggleStdioFallback(!figmaPreferStdio)}
          disabled={busyKey === "figma-stdio-toggle"}
          style={actionButtonStyle(tc, busyKey === "figma-stdio-toggle")}
          title="Abilita/disabilita fallback stdio Figma"
        >
          {busyKey === "figma-stdio-toggle"
            ? "Aggiorno..."
            : figmaPreferStdio
              ? "Fallback stdio: attivo"
              : "Fallback stdio: disattivo"}
        </button>
        {figmaOAuthStatus?.redirectUri && (
          <span style={{ fontSize: 11, color: tc.textMuted }}>
            callback: {figmaOAuthStatus.redirectUri}
          </span>
        )}
      </div>
    </div>
  );
}
