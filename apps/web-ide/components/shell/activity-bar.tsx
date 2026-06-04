"use client";

import type { useThemeColors } from "../../lib/theme";
import type { SidebarView } from "../sidebar/sidebar-manager";
import { UserSidebarMenu } from "../user-header";
import { sidebarItems } from "./shell-helpers";

export function ActivityBar({
  tc,
  activityButtonSize,
  activeSidebarView,
  onSelectView,
}: {
  tc: ReturnType<typeof useThemeColors>;
  activityButtonSize: number;
  activeSidebarView: SidebarView;
  onSelectView: (view: SidebarView) => void;
}) {
  return (
    <aside
      style={{
        gridRow: "2 / 4",
        gridColumn: "1",
        borderRight: `1px solid ${tc.border}`,
        background: tc.bgSidebar,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        gap: 8,
        padding: "10px 6px",
      }}
    >
      {sidebarItems.map((item) => (
        <button
          key={item.key}
          onClick={() => onSelectView(item.key)}
          title={item.label}
          aria-label={item.label}
          style={{
            width: activityButtonSize,
            height: activityButtonSize,
            borderRadius: 8,
            border: `1px solid ${activeSidebarView === item.key ? tc.accent : tc.border}`,
            background: activeSidebarView === item.key ? tc.accentBg : "transparent",
            color: activeSidebarView === item.key ? tc.accent : tc.textSecondary,
            cursor: "pointer",
            fontWeight: 700,
            fontSize: 14,
          }}
        >
          {item.icon}
        </button>
      ))}

      {/* Spacer per spingere il menu utente in fondo */}
      <div style={{ flex: 1 }} />

      {/* Menu utente in fondo alla activity bar */}
      <UserSidebarMenu buttonSize={activityButtonSize} tc={tc} />
    </aside>
  );
}
