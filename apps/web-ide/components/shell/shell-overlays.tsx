"use client";

import type { useThemeColors } from "../../lib/theme";

type LiveHealth = {
  // Stato di mcp-core (porta 4000). Dopo l'eliminazione del brain Python la chat
  // e gli agent run girano qui: e' l'unico indicatore del Core.
  neural_core: boolean;
};

export function ShellOverlays({
  tc,
  projectBusy,
  projectError,
  liveHealth,
}: {
  tc: ReturnType<typeof useThemeColors>;
  projectBusy: boolean;
  projectError: string | null;
  liveHealth: LiveHealth;
}) {
  return (
    <>
      {projectBusy && (
        <div
          style={{
            position: "fixed",
            top: 12,
            right: 12,
            padding: "8px 12px",
            borderRadius: 8,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            color: tc.text,
            fontSize: 12,
          }}
        >
          Caricamento progetto...
        </div>
      )}

      {projectError && (
        <div
          style={{
            position: "fixed",
            bottom: 36,
            right: 12,
            maxWidth: 520,
            padding: "8px 12px",
            borderRadius: 8,
            background: `${tc.error}18`,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            fontSize: 12,
            zIndex: 10,
          }}
        >
          {projectError}
        </div>
      )}

      {/* Banner Core offline — visibile e prominente */}
      {!liveHealth.neural_core && (
        <div
          style={{
            position: "fixed",
            top: 38,
            left: "50%",
            transform: "translateX(-50%)",
            padding: "8px 20px",
            borderRadius: 8,
            background: "#dc2626",
            color: "#fff",
            fontSize: 13,
            fontWeight: 600,
            zIndex: 9999,
            display: "flex",
            alignItems: "center",
            gap: 8,
            boxShadow: "0 4px 12px rgba(220,38,38,0.4)",
          }}
        >
          <span style={{ fontSize: 16 }}>!</span>
          Core (mcp-core) offline — la chat e gli agent run non funzioneranno
        </div>
      )}
    </>
  );
}
