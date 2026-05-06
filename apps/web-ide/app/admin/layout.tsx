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
  const [mobileNavOpen, setMobileNavOpen] = useState(false);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const onResize = () => setViewportWidth(window.innerWidth);
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const compactSidebar = viewportWidth < 1100;
  useEffect(() => {
    if (!compactSidebar) setMobileNavOpen(false);
  }, [compactSidebar]);

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
          {compactSidebar && (
            <button
              type="button"
              aria-label="Apri menu"
              onClick={() => setMobileNavOpen(true)}
              style={{
                width: 36,
                height: 36,
                borderRadius: 10,
                border: `1px solid ${tc.border}`,
                background: tc.bgCard,
                color: tc.text,
                display: "grid",
                placeItems: "center",
                cursor: "pointer",
              }}
            >
              <span style={{ fontSize: 18, lineHeight: 1 }}>☰</span>
            </button>
          )}
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
      <div style={{ display: "flex", flexDirection: "row", flex: 1, overflow: "hidden" }}>
        {!compactSidebar && <AdminSidebar compact={false} />}
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

      {compactSidebar && mobileNavOpen && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label="Menu admin"
          onClick={() => setMobileNavOpen(false)}
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.45)",
            zIndex: 50,
            display: "flex",
          }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            style={{
              width: 280,
              maxWidth: "85vw",
              height: "100%",
              borderRight: `1px solid ${tc.border}`,
              background: tc.bgSidebar,
              boxShadow: "0 12px 40px rgba(0,0,0,0.35)",
              display: "flex",
              flexDirection: "column",
            }}
          >
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                padding: "10px 12px",
                borderBottom: `1px solid ${tc.border}`,
                background: tc.bgHeader,
              }}
            >
              <strong style={{ letterSpacing: "0.08em", fontSize: 12 }}>MENU</strong>
              <button
                type="button"
                aria-label="Chiudi menu"
                onClick={() => setMobileNavOpen(false)}
                style={{
                  width: 34,
                  height: 34,
                  borderRadius: 10,
                  border: `1px solid ${tc.border}`,
                  background: tc.bgCard,
                  color: tc.text,
                  display: "grid",
                  placeItems: "center",
                  cursor: "pointer",
                }}
              >
                ✕
              </button>
            </div>
            <AdminSidebar
              compact
              onNavigate={() => setMobileNavOpen(false)}
            />
          </div>
        </div>
      )}
    </div>
  );
}
