"use client";
import { useThemeColors } from "../../../lib/theme";

export function ViewHeader({
  title,
  subtitle,
  actions,
}: {
  title: string;
  subtitle?: string;
  actions?: React.ReactNode;
}) {
  const tc = useThemeColors();
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        padding: "6px 10px",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgSidebar,
      }}
    >
      <div>
        <div
          style={{
            fontSize: 12,
            fontWeight: 700,
            color: tc.text,
            textTransform: "uppercase",
            letterSpacing: "0.06em",
          }}
        >
          {title}
        </div>
        {subtitle && (
          <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>
            {subtitle}
          </div>
        )}
      </div>
      {actions}
    </div>
  );
}
