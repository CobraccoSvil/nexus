"use client";

import { useState } from "react";
import type { Route } from "next";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useThemeColors } from "../lib/theme";
import { useI18n } from "../lib/i18n";

const settingsSubKeys = [
  { key: "providers", href: "/admin/settings/providers" as Route },
  { key: "routing", href: "/admin/settings/routing" as Route },
  { key: "connectors", href: "/admin/settings/connectors" as Route, label: "Plugin MCP" },
  { key: "security", href: "/admin/settings/security" as Route, label: "Sicurezza & DLP" },
  { key: "infrastructure", href: "/admin/settings/infrastructure" as Route },
  { key: "embeddings", href: "/admin/settings/embeddings" as Route },
  { key: "quality", href: "/admin/settings/quality" as Route },
  { key: "learning", href: "/admin/settings/learning" as Route },
  { key: "agent", href: "/admin/settings/agent" as Route, label: "Agenti AI" },
  { key: "optimizer", href: "/admin/settings/optimizer" as Route, label: "Ottimizzatore" },
  { key: "reflection", href: "/admin/settings/reflection" as Route, label: "Self-Reflection" },
  { key: "auth", href: "/admin/settings/auth" as Route },
];

export function AdminSidebar({
  compact = false,
  onNavigate,
}: {
  compact?: boolean;
  onNavigate?: () => void;
}) {
  const pathname = usePathname();
  const tc = useThemeColors();
  const { t } = useI18n();

  const menuGroups = [
    {
      key: "ai-prompt",
      label: "AI & Prompt",
      icon: "AI",
      items: [
        { label: "Template Prompt", href: "/admin/prompts" as Route, icon: "PT" },
        { label: "Dashboard Prompt", href: "/admin/prompts/dashboard" as Route, icon: "DP" },
        { label: "Feedback AI", href: "/admin/ai-feedback" as Route, icon: "F" },
        { label: "Apprendimento Progetto", href: "/admin/project-learning" as Route, icon: "AP" },
      ],
    },
    {
      key: "dati-vettori",
      label: "Dati & Vettori",
      icon: "DV",
      items: [
        { label: "Database Nexus", href: "/admin/nexus-database" as Route, icon: "DN" },
        { label: "Manutenzione Vettori", href: "/admin/vector-maintenance" as Route, icon: "MV" },
        { label: "Porting Progetto", href: "/admin/project-porting" as Route, icon: "PP" },
      ],
    },
    {
      key: "utenti-accesso",
      label: "Utenti & Accesso",
      icon: "UA",
      items: [
        { label: "Profili", href: "/admin/profiles" as Route, icon: "PR" },
        { label: t("admin.users"), href: "/admin/users" as Route, icon: "U" },
        { label: "Fatturazione", href: "/admin/billing" as Route, icon: "FA" },
      ],
    },
    {
      key: "sistema",
      label: "Sistema",
      icon: "SI",
      items: [
        { label: t("admin.appearance"), href: "/admin/appearance" as Route, icon: "A" },
        { label: t("admin.language"), href: "/admin/language" as Route, icon: "L" },
        { label: "Processi Lunghi", href: "/admin/long-running" as Route, icon: "PL" },
        { label: "Browser Bridge", href: "/admin/browser-bridge" as Route, icon: "BB" },
      ],
    },
  ];

  // Inizializza aperto il gruppo che contiene la pagina attiva
  const [openGroups, setOpenGroups] = useState<Record<string, boolean>>(() => {
    const initial: Record<string, boolean> = {};
    for (const group of menuGroups) {
      initial[group.key] = group.items.some((item) => pathname.startsWith(item.href));
    }
    return initial;
  });

  const toggleGroup = (key: string) => {
    setOpenGroups((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const isSettingsActive = pathname === "/admin" || pathname.startsWith("/admin/settings");

  return (
    <nav
      className="no-scrollbar flex-col overflow-y-auto"
      style={{
        width: compact ? "100%" : 220,
        minHeight: compact ? "auto" : "calc(100vh - 57px)",
        maxHeight: compact ? "42vh" : "none",
        borderRight: compact ? "none" : `1px solid ${tc.border}`,
        borderBottom: compact ? `1px solid ${tc.border}` : "none",
        background: tc.bgSidebar,
        padding: compact ? "10px 0" : "16px 0",
        gap: 2,
      }}
    >
      <div
        className="text-xs font-bold text-muted"
        style={{
          padding: compact ? "0 12px 10px" : "0 16px 12px",
          textTransform: "uppercase",
          letterSpacing: "0.1em",
        }}
      >
        {t("admin.configuration")}
      </div>

      {/* ── Sezione Impostazioni (con sotto-voci) ── */}
      <Link
        href={"/admin" as Route}
        className="flex-row-gap-10 text-base transition-all"
        onClick={onNavigate}
        style={{
          padding: compact ? "9px 12px" : "10px 16px",
          margin: compact ? "0 6px" : "0 8px",
          borderRadius: 8,
          textDecoration: "none",
          fontWeight: isSettingsActive ? 600 : 400,
          color: isSettingsActive ? tc.accent : tc.textSecondary,
          background: pathname === "/admin" ? tc.bgActive : "transparent",
        }}
      >
        <span
          className="flex-row font-bold text-sm"
          style={{
            width: 28,
            height: 28,
            borderRadius: 6,
            background: isSettingsActive ? tc.accent : tc.border,
            color: isSettingsActive ? "#fff" : tc.textMuted,
            justifyContent: "center",
            alignItems: "center",
          }}
        >
          S
        </span>
        {t("admin.settings")}
      </Link>

      <div className="flex-col" style={{ marginLeft: compact ? 20 : 32, gap: 1 }}>
        {settingsSubKeys.map((item) => {
          const active = pathname === item.href;
          const catKey = `cat.${item.key}` as Parameters<typeof t>[0];
          const displayLabel = "label" in item && item.label ? item.label : t(catKey);
          return (
            <Link
              key={item.key}
              href={item.href}
              className="text-sm transition-all"
              onClick={onNavigate}
              style={{
                display: "block",
                padding: compact ? "6px 12px" : "6px 16px",
                margin: compact ? "0 6px" : "0 8px",
                borderRadius: 6,
                textDecoration: "none",
                color: active ? tc.accent : tc.textMuted,
                fontWeight: active ? 600 : 400,
                background: active ? tc.bgActive : "transparent",
                borderLeft: "2px solid transparent",
              }}
            >
              {displayLabel}
            </Link>
          );
        })}
      </div>

      <div style={{ height: 8 }} />

      {/* ── Gruppi collassabili ── */}
      {menuGroups.map((group) => {
        const isOpen = !!openGroups[group.key];
        const groupActive = group.items.some((item) => pathname.startsWith(item.href));

        return (
          <div key={group.key}>
            {/* Intestazione gruppo */}
            <button
              type="button"
              onClick={() => toggleGroup(group.key)}
              className="flex-row-gap-10 text-base transition-all"
              style={{
                padding: compact ? "9px 12px" : "10px 16px",
                margin: compact ? "0 6px" : "0 8px",
                width: `calc(100% - ${compact ? 12 : 16}px)`,
                borderRadius: 8,
                background: "transparent",
                border: "none",
                cursor: "pointer",
                textAlign: "left",
                fontWeight: groupActive ? 600 : 400,
                color: groupActive ? tc.accent : tc.textSecondary,
                display: "flex",
                alignItems: "center",
                gap: 10,
                fontSize: 14,
              }}
            >
              <span
                className="flex-row font-bold text-sm"
                style={{
                  width: 28,
                  height: 28,
                  flexShrink: 0,
                  borderRadius: 6,
                  background: groupActive ? tc.accent : tc.border,
                  color: groupActive ? "#fff" : tc.textMuted,
                  justifyContent: "center",
                  alignItems: "center",
                  display: "flex",
                  fontSize: 10,
                }}
              >
                {group.icon}
              </span>
              <span style={{ flex: 1 }}>{group.label}</span>
              <span style={{ fontSize: 10, color: tc.textMuted, marginRight: 6 }}>
                {isOpen ? "▾" : "▸"}
              </span>
            </button>

            {/* Sotto-voci del gruppo */}
            {isOpen && (
              <div className="flex-col" style={{ marginLeft: compact ? 20 : 32, gap: 1 }}>
                {group.items.map((item) => {
                  const active = pathname === item.href;
                  return (
                    <Link
                      key={item.href}
                      href={item.href}
                      className="text-sm transition-all"
                      onClick={onNavigate}
                      style={{
                        display: "block",
                        padding: compact ? "6px 12px" : "6px 16px",
                        margin: compact ? "0 6px" : "0 8px",
                        borderRadius: 6,
                        textDecoration: "none",
                        color: active ? tc.accent : tc.textMuted,
                        fontWeight: active ? 600 : 400,
                        background: active ? tc.bgActive : "transparent",
                        borderLeft: "2px solid transparent",
                      }}
                    >
                      {item.label}
                    </Link>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </nav>
  );
}
