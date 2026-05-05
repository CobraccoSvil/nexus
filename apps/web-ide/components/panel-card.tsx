"use client";

import type { ReactNode } from "react";
import { useThemeColors } from "../lib/theme";

export function PanelCard({
  title,
  subtitle,
  fullHeight = false,
  children,
}: {
  title: string;
  subtitle?: string;
  fullHeight?: boolean;
  children: ReactNode;
}) {
  const t = useThemeColors();
  return (
    <section
      style={{
        border: `1px solid ${t.border}`,
        borderRadius: 12,
        background: t.bgCard,
        color: t.text,
        padding: 16,
        height: fullHeight ? "100%" : undefined,
        display: fullHeight ? "flex" : undefined,
        flexDirection: fullHeight ? "column" : undefined,
        minHeight: fullHeight ? 0 : undefined,
      }}
    >
      <header style={{ marginBottom: 12 }}>
        <div style={{ fontSize: 16, fontWeight: 700 }}>{title}</div>
        {subtitle ? (
          <div style={{ fontSize: 13, color: t.textMuted, marginTop: 4 }}>{subtitle}</div>
        ) : null}
      </header>
      <div style={{ minHeight: 0, flex: fullHeight ? 1 : undefined, display: fullHeight ? "flex" : undefined, flexDirection: fullHeight ? "column" : undefined }}>
        {children}
      </div>
    </section>
  );
}
