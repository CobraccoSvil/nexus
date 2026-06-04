"use client";

import type { useThemeColors } from "../../lib/theme";
import type { UserProjectDetails, UserProjectSummary, WorkbenchLayoutMode } from "../../lib/api-client";
import { ProjectSwitcher } from "../project-switcher";
import { TruncatedText } from "../truncated-text";
import { ConnectionStatusBadge } from "../dispatcher-status";
import {
  StatusDot,
  iconButton,
  providerTitle,
  type ProviderHealthState,
  type ProviderKey,
} from "./shell-helpers";

type ProviderStatusMap = Record<ProviderKey, ProviderHealthState>;

export function TopBar({
  tc,
  isMobileViewport,
  isNarrowViewport,
  activeProject,
  projects,
  layoutMode,
  primarySidebarVisible,
  bottomPanelVisible,
  isFullscreen,
  providerStatus,
  onTogglePrimarySidebar,
  onToggleBottomPanel,
  onCycleLayoutMode,
  onToggleFullscreen,
  onSelectProject,
  onRegisterProject,
  onRefreshProjects,
}: {
  tc: ReturnType<typeof useThemeColors>;
  isMobileViewport: boolean;
  isNarrowViewport: boolean;
  activeProject: UserProjectDetails | null;
  projects: UserProjectSummary[];
  layoutMode: WorkbenchLayoutMode;
  primarySidebarVisible: boolean;
  bottomPanelVisible: boolean;
  isFullscreen: boolean;
  providerStatus: ProviderStatusMap;
  onTogglePrimarySidebar: () => void;
  onToggleBottomPanel: () => void;
  onCycleLayoutMode: () => void;
  onToggleFullscreen: () => void;
  onSelectProject: (projectId: string) => Promise<void>;
  onRegisterProject: (absolutePath: string, name?: string) => Promise<void>;
  onRefreshProjects: () => Promise<void>;
}) {
  return (
    <header
      style={{
        gridColumn: "1 / 5",
        display: "flex",
        alignItems: "center",
        columnGap: 10,
        padding: "0 12px",
        background: tc.bgHeader,
        borderBottom: `1px solid ${tc.border}`,
        flexWrap: isMobileViewport ? "wrap" : "nowrap",
        rowGap: isMobileViewport ? 6 : 0,
      }}
    >
      <a
        href="/?site"
        title="Vedi sito"
        style={{
          fontSize: 13,
          letterSpacing: "0.08em",
          color: tc.text,
          fontWeight: 700,
          textDecoration: "none",
          cursor: "pointer",
        }}
      >
        NEXUS
      </a>
      <TruncatedText
        text={activeProject?.name ?? "Nessun progetto"}
        maxWidth={220}
        tc={tc}
        style={{ color: tc.textMuted, fontSize: 12 }}
      />
      <div style={{ width: 1, height: 20, background: tc.border }} />
      <button
        type="button"
        onClick={onTogglePrimarySidebar}
        title={primarySidebarVisible ? "Nascondi primary sidebar" : "Mostra primary sidebar"}
        aria-label={primarySidebarVisible ? "Nascondi primary sidebar" : "Mostra primary sidebar"}
        style={iconButton(tc, false, primarySidebarVisible)}
      >
        ◧
      </button>
      <button
        type="button"
        onClick={onToggleBottomPanel}
        title={bottomPanelVisible ? "Nascondi panel" : "Mostra panel"}
        aria-label={bottomPanelVisible ? "Nascondi panel" : "Mostra panel"}
        style={iconButton(tc, false, bottomPanelVisible)}
      >
        <span style={{ display: "inline-block", transform: "rotate(90deg)" }}>◧</span>
      </button>
      <button
        type="button"
        onClick={onCycleLayoutMode}
        title={`Cambia layout (${layoutMode})`}
        aria-label={`Cambia layout (${layoutMode})`}
        style={iconButton(tc)}
      >
        ⧉
      </button>
      <button
        type="button"
        onClick={onToggleFullscreen}
        title={isFullscreen ? "Esci da pieno schermo" : "Vai a pieno schermo"}
        aria-label={isFullscreen ? "Esci da pieno schermo" : "Vai a pieno schermo"}
        style={iconButton(tc, false, isFullscreen)}
      >
        {isFullscreen ? "🗗" : "🗖"}
      </button>
      <div style={{ flex: 1, minWidth: 0, order: isMobileViewport ? 10 : 0 }}>
        <ProjectSwitcher
          projects={projects}
          activeProjectId={activeProject?.id}
          compact={isMobileViewport}
          onSelect={onSelectProject}
          onRegister={onRegisterProject}
          onRefreshProjects={onRefreshProjects}
        />
      </div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          columnGap: isNarrowViewport ? 8 : 10,
          marginLeft: 8,
          flexShrink: 0,
          maxWidth: isNarrowViewport ? 320 : undefined,
          overflowX: isNarrowViewport ? "auto" : "visible",
          paddingBottom: isNarrowViewport ? 2 : 0,
          order: isMobileViewport ? 11 : 0,
          flexWrap: isMobileViewport ? "wrap" : "nowrap",
          rowGap: isMobileViewport ? 6 : 0,
          whiteSpace: "nowrap",
        }}
        aria-label="Stato provider AI"
      >
        <ConnectionStatusBadge />
        <span
          title={providerTitle("OpenAI", providerStatus.openai)}
          style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
        >
          <StatusDot ok={providerStatus.openai.ok} billing={providerStatus.openai.billing} />
          {!isNarrowViewport && "OpenAI"}
        </span>
        <span
          title={providerTitle("Anthropic", providerStatus.anthropic)}
          style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
        >
          <StatusDot ok={providerStatus.anthropic.ok} billing={providerStatus.anthropic.billing} />
          {!isNarrowViewport && "Anthropic"}
        </span>
        <span
          title={providerTitle("Google", providerStatus.google)}
          style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
        >
          <StatusDot ok={providerStatus.google.ok} billing={providerStatus.google.billing} />
          {!isNarrowViewport && "Google"}
        </span>
        <span
          title={providerTitle("DeepSeek", providerStatus.deepseek)}
          style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
        >
          <StatusDot ok={providerStatus.deepseek.ok} billing={providerStatus.deepseek.billing} />
          {!isNarrowViewport && "DeepSeek"}
        </span>
        <span
          title={providerTitle("Mistral", providerStatus.mistral)}
          style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
        >
          <StatusDot ok={providerStatus.mistral.ok} billing={providerStatus.mistral.billing} />
          {!isNarrowViewport && "Mistral"}
        </span>
      </div>
    </header>
  );
}
