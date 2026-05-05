"use client";

import { useEffect, useState } from "react";
import dynamic from "next/dynamic";
import { useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";

// Dynamic imports per componenti admin
const AdminSidebar = dynamic(() => import("../../components/admin-sidebar").then(mod => ({ default: mod.AdminSidebar })), {
  loading: () => <div style={{ width: 250, background: "#252526", display: "flex", alignItems: "center", justifyContent: "center" }}>Loading...</div>,
  ssr: false,
});

const UserHeader = dynamic(() => import("../../components/user-header").then(mod => ({ default: mod.UserHeader })), {
  loading: () => <div style={{ minWidth: 120, display: "flex", alignItems: "center", justifyContent: "center" }}>Loading...</div>,
  ssr: false,
});

export default function AdminLayout({ children }: { children: React.ReactNode }) {
  const tc = useThemeColors();
  const { t } = useI18n();
  const [viewportWidth, setViewportWidth] = useState(1280);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const onResize = () => setViewportWidth(window.innerWidth);
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const compactSidebar = viewportWidth < 1100;

  return (
    <div
      style={{
        height: "100vh",
        display: "flex",
        flexDirection: "column",
        background: tc.bgGradient,
        color: tc.text,
        fontFamily: "'JetBrains Mono', 'Fira Code', monospace",
      }}
    >
      <header
        style={{
          flexShrink: 0,
          padding: compactSidebar ? "10px 14px" : "12px 24px",
          borderBottom: `1px solid ${tc.border}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          background: tc.bgHeader,
          zIndex: 10,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
          <strong style={{ letterSpacing: "0.08em", fontSize: 14 }}>NEXUS</strong>
          <span
            style={{
              padding: "4px 12px",
              borderRadius: 6,
              background: tc.accentBg,
              color: tc.accent,
              fontSize: 11,
              fontWeight: 600,
            }}
          >
            {t("admin.badge")}
          </span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
          <UserHeader />
        </div>
      </header>
      <div style={{ display: "flex", flexDirection: compactSidebar ? "column" : "row", flex: 1, overflow: "hidden" }}>
        <AdminSidebar compact={compactSidebar} />
        <main
          className="no-scrollbar"
          style={{
            flex: 1,
            padding: compactSidebar ? "16px 14px 24px" : "32px 40px",
            maxWidth: compactSidebar ? "none" : 900,
            overflowY: "auto",
          }}
        >
          {children}
        </main>
      </div>
    </div>
  );
}
