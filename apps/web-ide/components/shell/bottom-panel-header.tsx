"use client";

import type { useThemeColors } from "../../lib/theme";
import type { UserProjectDetails } from "../../lib/api-client";
import type { PanelTab } from "../panels/bottom-panel-manager";
import { QuotaBadge } from "../panels/quota-badge";
import { iconButton, panelTabs } from "./shell-helpers";
import { PanelTabButton } from "./panel-tabs";

export function BottomPanelHeader({
  tc,
  isMobileViewport,
  activePanelTab,
  activeProject,
  onSelectTab,
  onHide,
}: {
  tc: ReturnType<typeof useThemeColors>;
  isMobileViewport: boolean;
  activePanelTab: PanelTab;
  activeProject: UserProjectDetails | null;
  onSelectTab: (tab: PanelTab) => void;
  onHide: () => void;
}) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgHeader,
        overflowX: "auto",
      }}
    >
      {panelTabs.map((tab) => (
        <PanelTabButton
          key={tab.key}
          tab={tab}
          active={activePanelTab === tab.key}
          tc={tc}
          isMobileViewport={isMobileViewport}
          onSelect={() => onSelectTab(tab.key)}
        />
      ))}
      <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: 8, paddingRight: 12 }}>
        {activeProject && <QuotaBadge projectId={activeProject.id} />}
        <button
          type="button"
          onClick={onHide}
          title="Nascondi panel"
          aria-label="Nascondi panel"
          style={iconButton(tc)}
        >
          ✕
        </button>
      </div>
    </div>
  );
}
