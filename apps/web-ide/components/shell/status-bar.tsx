"use client";

import type { useThemeColors } from "../../lib/theme";
import type { WorkbenchLayoutMode } from "../../lib/api-client";
import { StatusDot } from "./shell-helpers";
import { FooterToastCenter } from "./footer-toast-center";
import { useI18n } from "../../lib/i18n";

type LiveHealth = {
  database: boolean;
  redis: boolean;
  // neural_core: stato di mcp-core (porta 4000). Dopo l'eliminazione del brain
  // Python gli agent run e gli endpoint AI (/api/neural) girano qui: questo LED
  // rappresenta il Core. Il vecchio LED "Brain" + campo brain_rest sono rimossi.
  neural_core: boolean;
  tools_grpc?: boolean;
};

export function StatusBar({
  tc,
  currentBranch,
  projectName,
  layoutMode,
  problemCount,
  liveHealth,
}: {
  tc: ReturnType<typeof useThemeColors>;
  currentBranch: string;
  projectName: string;
  layoutMode: WorkbenchLayoutMode;
  problemCount: number;
  liveHealth: LiveHealth;
}) {
  const { t } = useI18n();
  return (
    <footer
      style={{
        gridColumn: "1 / 5",
        gridRow: "4",
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "0 10px",
        borderTop: `1px solid ${tc.border}`,
        background: tc.bgHeader,
        color: tc.textMuted,
        fontSize: 11,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 12, flexShrink: 0 }}>
        <span>{currentBranch}</span>
        <span>{projectName}</span>
        <span>{layoutMode}</span>
        <span>{problemCount} problemi</span>
      </div>
      {/* Centro: messaggio di stato non invasivo (toast + esiti azioni + pending). */}
      <div style={{ flex: 1, minWidth: 0, display: "flex", justifyContent: "center", overflow: "hidden" }}>
        <FooterToastCenter tc={tc} />
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 12, flexShrink: 0 }}>
        <span>UTF-8</span>
        <span>LF</span>
        <span title={liveHealth.database ? "Database online" : "Database offline"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={liveHealth.database} />
          DB
        </span>
        <span title={liveHealth.redis ? "Redis online" : "Redis offline"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={liveHealth.redis} />
          {t("shell.redis")}
        </span>
        <span title={
          liveHealth.neural_core
            ? "Core (mcp-core :4000) online — orchestratore, agent run ed endpoint AI (/api/neural) attivi"
            : "Core (mcp-core :4000) offline — chat e agent run non funzioneranno"
        } style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={liveHealth.neural_core} />
          {t("shell.core")}
        </span>
        <span title={liveHealth.tools_grpc ? "MCP Tools (gRPC :50071) online" : "MCP Tools offline — l'AI non potrà eseguire tool (read_file, str_replace, ecc.)"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={!!liveHealth.tools_grpc} />
          {t("shell.tools")}
        </span>
      </div>
    </footer>
  );
}
