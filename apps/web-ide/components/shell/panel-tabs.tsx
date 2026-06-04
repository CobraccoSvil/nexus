"use client";

import type { useThemeColors } from "../../lib/theme";
import type { PanelTab } from "../panels/bottom-panel-manager";
import { usePanelHighlight } from "../dispatcher-status";

/**
 * Singolo tab del PanelDock con highlight effect quando dispatcher emette
 * HighlightPanel per la sua key.
 */
export function PanelTabButton({
  tab,
  active,
  tc,
  isMobileViewport,
  onSelect,
}: {
  tab: { key: PanelTab; label: string };
  active: boolean;
  tc: ReturnType<typeof useThemeColors>;
  isMobileViewport: boolean;
  onSelect: () => void;
}) {
  const highlighted = usePanelHighlight(tab.key);
  return (
    <button
      onClick={onSelect}
      style={{
        border: "none",
        borderRight: `1px solid ${tc.border}`,
        background: highlighted
          ? "rgba(245,158,11,0.25)"
          : active
            ? tc.bg
            : "transparent",
        color: active ? tc.text : tc.textMuted,
        padding: isMobileViewport ? "0 8px" : "0 14px",
        height: "100%",
        cursor: "pointer",
        fontSize: isMobileViewport ? 11 : 12,
        whiteSpace: "nowrap",
        flexShrink: 0,
        transition: "background-color 200ms ease-out",
        boxShadow: highlighted ? "inset 0 -2px 0 #f59e0b" : "none",
      }}
    >
      {tab.label}
    </button>
  );
}

// ── Tab bar pannello destro: switcha tra Editor (file Monaco) e SQL (pannello
// gestore query). Vedi listener `nexus:sql:open` in ide-shell che imposta
// rightView="sql" su richiesta dalla chat.
export function RightViewTabs({
  rightView,
  setRightView,
  tc,
}: {
  rightView: "editor" | "sql";
  setRightView: (v: "editor" | "sql") => void;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const Tab = ({
    label,
    active,
    onClick,
  }: {
    label: string;
    active: boolean;
    onClick: () => void;
  }) => (
    <button
      type="button"
      onClick={onClick}
      style={{
        padding: "0 12px",
        height: "100%",
        background: active ? tc.bgActive : "transparent",
        color: active ? tc.text : tc.textMuted,
        border: "none",
        borderRight: `1px solid ${tc.border}`,
        cursor: "pointer",
        fontSize: 12,
        fontWeight: active ? 600 : 400,
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </button>
  );
  return (
    <div
      style={{
        display: "flex",
        alignItems: "stretch",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgSidebar,
        fontSize: 12,
      }}
    >
      <Tab label="Editor" active={rightView === "editor"} onClick={() => setRightView("editor")} />
      <Tab label="SQL" active={rightView === "sql"} onClick={() => setRightView("sql")} />
    </div>
  );
}
