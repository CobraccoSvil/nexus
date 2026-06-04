"use client";

import type { useThemeColors } from "../../lib/theme";
import type { WorkbenchLayoutMode } from "../../lib/api-client";
import { StatusDot } from "./shell-helpers";

type LiveHealth = {
  database: boolean;
  redis: boolean;
  neural_core: boolean;
  tools_grpc?: boolean;
  brain_rest?: boolean;
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
  return (
    <footer
      style={{
        gridColumn: "1 / 5",
        gridRow: "4",
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        padding: "0 10px",
        borderTop: `1px solid ${tc.border}`,
        background: tc.bgHeader,
        color: tc.textMuted,
        fontSize: 11,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
        <span>{currentBranch}</span>
        <span>{projectName}</span>
        <span>{layoutMode}</span>
        <span>{problemCount} problemi</span>
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
        <span>UTF-8</span>
        <span>LF</span>
        <span title={liveHealth.database ? "Database online" : "Database offline"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={liveHealth.database} />
          DB
        </span>
        <span title={liveHealth.redis ? "Redis online" : "Redis offline"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={liveHealth.redis} />
          Redis
        </span>
        <span title={
          liveHealth.neural_core && liveHealth.brain_rest
            ? "Brain (Python LangGraph) online — gRPC + REST ok"
            : !liveHealth.neural_core && !liveHealth.brain_rest
              ? "Brain offline — gRPC e REST irraggiungibili"
              : !liveHealth.brain_rest
                ? "Brain REST (:8001) offline — gli agent run non funzioneranno"
                : "Brain gRPC (:50051) offline — la chat potrebbe non rispondere"
        } style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={liveHealth.neural_core && !!liveHealth.brain_rest} />
          Brain
        </span>
        <span title={liveHealth.tools_grpc ? "MCP Tools (gRPC :50071) online" : "MCP Tools offline — l'AI non potrà eseguire tool (read_file, str_replace, ecc.)"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={!!liveHealth.tools_grpc} />
          Tools
        </span>
      </div>
    </footer>
  );
}
