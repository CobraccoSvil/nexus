"use client";

import { useState, useEffect } from "react";
import { useI18n } from "../lib/i18n";
import { Band } from "../components/landing/Band";
import { NavBar } from "../components/landing/NavBar";
import { HeroSplit } from "../components/landing/HeroSplit";
import { FeatureCard } from "../components/landing/FeatureCard";
import { ProviderLogo } from "../components/landing/ProviderLogo";
import { CobraccoMark } from "../components/landing/CobraccoMark";

/* ─── CONFIG ─── */
const SHOW_LOGIN_CTA = false;

const C = {
  dark: {
    bg: "#06090f",
    surface: "rgba(12,18,30,0.85)",
    border: "#1a2336",
    text: "#e2e8f0",
    muted: "#8494a7",
  },
  light: {
    bg: "#fafaf9",
    surface: "#ffffff",
    border: "#e5e5e5",
    text: "#171717",
    muted: "#737373",
  },
  accent: "#5ba3e6",
  accentGradient: "linear-gradient(135deg, #5ba3e6, #8b5cf6)",
  accentGlow: "rgba(91,163,230,0.25)",
};

export default function LandingPage() {
  const { t } = useI18n();
  const [mobile, setMobile] = useState(false);

  useEffect(() => {
    const check = () => setMobile(window.innerWidth < 768);
    check();
    window.addEventListener("resize", check);
    return () => window.removeEventListener("resize", check);
  }, []);

  return (
    <div style={{ background: C.light.bg, minHeight: "100vh" }}>
      {/* ─── NAV (light, sticky) ─── */}
      <NavBar />

      {/* ─── HERO (dark) ─── */}
      <Band tone="dark" style={{ padding: mobile ? "60px 0" : "100px 0" }}>
        <HeroSplit />
      </Band>

      {/* ─── LIVE IDE PREVIEW (light) ─── */}
      <Band tone="light" id="preview">
        <div style={{ textAlign: "center", marginBottom: 32 }}>
          <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
            {t("landing.v2.preview.title")}
          </h2>
          <p style={{ fontSize: 16, color: C.light.muted, maxWidth: 560, margin: "8px auto 0" }}>
            {t("landing.v2.preview.subtitle")}
          </p>
        </div>
        <div
          style={{
            borderRadius: 12,
            overflow: "hidden",
            border: `1px solid ${C.light.border}`,
            aspectRatio: mobile ? "4/3" : "16/9",
            background: "#fff",
            position: "relative",
            boxShadow: "0 4px 24px rgba(0,0,0,0.08)",
          }}
        >
          <img
            src="/screenshots/hero-ide.jpg"
            alt="Nexus IDE"
            style={{ width: "100%", height: "100%", objectFit: "cover" }}
            loading="lazy"
            onError={(e) => { (e.target as HTMLImageElement).style.opacity = "0"; }}
          />
        </div>
      </Band>

      {/* ─── BUILT DIFFERENT (light) ─── */}
      <Band tone="light">
        <div style={{ maxWidth: 640, margin: "0 auto", textAlign: "center" }}>
          <h2 style={{ fontSize: mobile ? 26 : 40, fontWeight: 800, color: C.light.text }}>
            {t("landing.v2.builtDifferent.title")}
          </h2>
          <p style={{ fontSize: 16, lineHeight: 1.7, color: C.light.muted, marginTop: 16 }}>
            {t("landing.v2.builtDifferent.desc")}
          </p>
        </div>
      </Band>

      {/* ─── AGENTS (light) ─── */}
      <Band tone="light" id="features" style={{ background: "#f5f5f4" }}>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: mobile ? "1fr" : "1fr 1fr",
            gap: 48,
            alignItems: "center",
          }}
        >
          <div>
            <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
              {t("landing.v2.agents.title")}
            </h2>
            <p style={{ fontSize: 16, lineHeight: 1.7, color: C.light.muted, marginTop: 16 }}>
              {t("landing.v2.agents.desc")}
            </p>
          </div>
          <div
            style={{
              borderRadius: 12,
              overflow: "hidden",
              border: `1px solid ${C.light.border}`,
              aspectRatio: "16/10",
              background: "#fff",
              boxShadow: "0 4px 24px rgba(0,0,0,0.08)",
            }}
          >
            <img
              src="/screenshots/orchestrator.jpg"
              alt="Orchestrator"
              style={{ width: "100%", height: "100%", objectFit: "cover" }}
              loading="lazy"
              onError={(e) => { (e.target as HTMLImageElement).style.opacity = "0"; }}
            />
          </div>
        </div>
      </Band>

      {/* ─── MULTI-PROVIDER CASCADE (light) ─── */}
      <Band tone="light">
        <div style={{ textAlign: "center", marginBottom: 40 }}>
          <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
            {t("landing.v2.providers.title")}
          </h2>
          <p style={{ fontSize: 16, color: C.light.muted, maxWidth: 560, margin: "8px auto 0" }}>
            {t("landing.v2.providers.subtitle")}
          </p>
        </div>
        <div
          style={{
            display: "flex",
            justifyContent: "center",
            gap: mobile ? 24 : 48,
            flexWrap: "wrap",
          }}
        >
          {["OpenAI", "Anthropic", "Google", "DeepSeek", "Mistral"].map((p) => (
            <ProviderLogo key={p} name={p} tone="light" />
          ))}
        </div>
        <div
          style={{
            marginTop: 40,
            borderRadius: 12,
            overflow: "hidden",
            border: `1px solid ${C.light.border}`,
            background: "#fff",
          }}
        >
          <img
            src="/screenshots/providers.jpg"
            alt="Provider cascade"
            style={{ width: "100%", display: "block" }}
            loading="lazy"
            onError={(e) => { (e.target as HTMLImageElement).parentElement!.style.display = "none"; }}
          />
        </div>
      </Band>

      {/* ─── INTEGRATIONS & TOOLS (light) ─── */}
      <Band tone="light">
        <div style={{ textAlign: "center", marginBottom: 40 }}>
          <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
            {t("landing.v2.integrations.title")}
          </h2>
          <p style={{ fontSize: 16, color: C.light.muted }}>
            {t("landing.v2.integrations.subtitle")}
          </p>
        </div>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: mobile ? "1fr" : "repeat(2, 1fr)",
            gap: 20,
          }}
        >
          <FeatureCard
            icon={<span>▶</span>}
            title={t("landing.v2.integrations.playwright.title")}
            description={t("landing.v2.integrations.playwright.desc")}
            screenshot="/screenshots/playwright-live.jpg"
            tone="light"
          />
          <FeatureCard
            icon={<span>⎇</span>}
            title={t("landing.v2.integrations.github.title")}
            description={t("landing.v2.integrations.github.desc")}
            tone="light"
          />
          <FeatureCard
            icon={<span>$</span>}
            title={t("landing.v2.integrations.terminal.title")}
            description={t("landing.v2.integrations.terminal.desc")}
            tone="light"
          />
          <FeatureCard
            icon={<span>D</span>}
            title={t("landing.v2.integrations.docs.title")}
            description={t("landing.v2.integrations.docs.desc")}
            tone="light"
          />
        </div>
      </Band>

      {/* ─── PERSISTENT MEMORY (light) ─── */}
      <Band tone="light">
        <div
          style={{
            display: "grid",
            gridTemplateColumns: mobile ? "1fr" : "1fr 1fr",
            gap: 48,
            alignItems: "center",
          }}
        >
          <div>
            <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
              {t("landing.v2.memory.title")}
            </h2>
            <p style={{ fontSize: 16, lineHeight: 1.7, color: C.light.muted, marginTop: 16 }}>
              {t("landing.v2.memory.desc")}
            </p>
          </div>
          <div
            style={{
              borderRadius: 12,
              overflow: "hidden",
              border: `1px solid ${C.light.border}`,
              background: "#fff",
              aspectRatio: "16/10",
            }}
          >
            <img
              src="/screenshots/cost-breakdown.jpg"
              alt="Cost breakdown"
              style={{ width: "100%", height: "100%", objectFit: "cover" }}
              loading="lazy"
              onError={(e) => { (e.target as HTMLImageElement).style.opacity = "0"; }}
            />
          </div>
        </div>
      </Band>

      {/* ─── KNOWLEDGE BASE (light, alternato) ─── */}
      <Band tone="light" id="knowledge" style={{ background: "#f5f5f4" }}>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: mobile ? "1fr" : "1fr 1fr",
            gap: 48,
            alignItems: "center",
          }}
        >
          <div>
            <div
              style={{
                display: "inline-block",
                padding: "4px 12px",
                borderRadius: 20,
                background: "rgba(139,92,246,0.1)",
                color: "#8b5cf6",
                fontSize: 12,
                fontWeight: 600,
                marginBottom: 16,
                letterSpacing: 0.5,
              }}
            >
              NEW
            </div>
            <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
              {t("landing.v2.knowledge.title")}
            </h2>
            <p style={{ fontSize: 16, lineHeight: 1.7, color: C.light.muted, marginTop: 16 }}>
              {t("landing.v2.knowledge.desc")}
            </p>
            <ul style={{ marginTop: 20, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 10 }}>
              {(["feat1", "feat2", "feat3", "feat4", "feat5"] as const).map((k) => (
                <li
                  key={k}
                  style={{
                    display: "flex",
                    alignItems: "flex-start",
                    gap: 8,
                    fontSize: 14,
                    color: C.light.muted,
                    lineHeight: 1.5,
                  }}
                >
                  <span style={{ color: "#8b5cf6", fontWeight: 700, flexShrink: 0 }}>-</span>
                  {t(`landing.v2.knowledge.${k}`)}
                </li>
              ))}
            </ul>
          </div>
          <div
            style={{
              borderRadius: 12,
              overflow: "hidden",
              border: `1px solid ${C.light.border}`,
              background: "#fff",
              aspectRatio: "16/10",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              boxShadow: "0 4px 24px rgba(0,0,0,0.08)",
            }}
          >
            <img
              src="/screenshots/knowledge-graph.jpg"
              alt="Knowledge Base — grafo interattivo"
              style={{ width: "100%", height: "100%", objectFit: "cover" }}
              loading="lazy"
              onError={(e) => {
                const parent = (e.target as HTMLImageElement).parentElement!;
                parent.innerHTML = `<div style="display:flex;flex-direction:column;align-items:center;gap:12px;padding:40px;color:${C.light.muted}">
                  <div style="font-size:48px">K</div>
                  <div style="font-size:14px;text-align:center">Knowledge Base<br/>Interactive graph + Obsidian vault</div>
                </div>`;
              }}
            />
          </div>
        </div>
      </Band>

      {/* ─── MULTI-TENANT ON-PREM (light, alternato) ─── */}
      <Band tone="light" style={{ background: "#f5f5f4" }}>
        <div style={{ maxWidth: 640, margin: "0 auto", textAlign: "center" }}>
          <h2 style={{ fontSize: mobile ? 24 : 36, fontWeight: 800, color: C.light.text }}>
            {t("landing.v2.onprem.title")}
          </h2>
          <p style={{ fontSize: 16, lineHeight: 1.7, color: C.light.muted, marginTop: 16 }}>
            {t("landing.v2.onprem.desc")}
          </p>
        </div>
      </Band>

      {/* ─── COMPARISON TABLE (light) ─── */}
      <Band tone="light" id="comparison">
        <div style={{ textAlign: "center", marginBottom: mobile ? 36 : 64 }}>
          <h2 style={{ fontSize: mobile ? 28 : 40, fontWeight: 800, color: C.light.text }}>
            {t("landing.compTitle")}
          </h2>
          <p style={{ fontSize: 16, color: C.light.muted }}>
            {t("landing.compDesc")}
          </p>
        </div>

        <div style={{ overflowX: "auto", WebkitOverflowScrolling: "touch" }}>
          <table
            style={{
              width: "100%",
              minWidth: mobile ? 800 : 1000,
              borderCollapse: "separate",
              borderSpacing: 0,
              background: "#fff",
              border: `1px solid ${C.light.border}`,
              borderRadius: 12,
              overflow: "hidden",
              fontSize: mobile ? 12 : 14,
              boxShadow: "0 2px 12px rgba(0,0,0,0.06)",
            }}
          >
            <thead>
              <tr>
                {[
                  t("landing.comp.feature"),
                  "Nexus",
                  "Cursor",
                  "Copilot",
                  "Windsurf",
                  "Devin",
                  "Claude Code",
                  "Aider",
                  "Continue",
                ].map((h, i) => (
                  <th
                    key={i}
                    style={{
                      padding: mobile ? "10px 8px" : "14px 16px",
                      textAlign: i === 0 ? "left" : "center",
                      borderBottom: `1px solid ${C.light.border}`,
                      color: i === 1 ? C.accent : C.light.text,
                      fontWeight: i === 1 ? 800 : 600,
                      fontSize: i === 1 ? (mobile ? 13 : 15) : undefined,
                      whiteSpace: "nowrap",
                      background: i === 1 ? "rgba(91,163,230,0.08)" : i === 0 ? "#f9fafb" : "transparent",
                    }}
                  >
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {COMPARISON_ROWS.map((row, ri) => (
                <tr key={ri}>
                  {row.map((cell, ci) => (
                    <td
                      key={ci}
                      style={{
                        padding: mobile ? "10px 8px" : "12px 16px",
                        textAlign: ci === 0 ? "left" : "center",
                        borderBottom:
                          ri < COMPARISON_ROWS.length - 1
                            ? `1px solid #f0f0f0`
                            : "none",
                        color: ci === 0 ? C.light.text : C.light.muted,
                        fontWeight: ci === 0 ? 500 : 400,
                        background: ci === 1 ? "rgba(91,163,230,0.05)" : "transparent",
                        whiteSpace: ci === 0 ? "nowrap" : undefined,
                      }}
                    >
                      {ci === 0
                        ? t(cell as Parameters<typeof t>[0])
                        : cell === true
                          ? "✅"
                          : cell === "~"
                            ? "⚠️"
                            : "—"}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        {/* Detail cards */}
        <div style={{ marginTop: mobile ? 40 : 64 }}>
          <div style={{ textAlign: "center", marginBottom: mobile ? 24 : 40 }}>
            <h3 style={{ fontSize: mobile ? 22 : 28, fontWeight: 700, color: C.light.text }}>
              {t("landing.compDetailsTitle")}
            </h3>
            <p style={{ fontSize: 15, color: C.light.muted, maxWidth: 560, margin: "8px auto 0" }}>
              {t("landing.compDetailsDesc")}
            </p>
          </div>
          <div
            style={{
              display: "grid",
              gridTemplateColumns: mobile ? "1fr" : "repeat(4, 1fr)",
              gap: 20,
            }}
          >
            {([
              { key: "routing", color: C.accent, icon: "R" },
              { key: "privacy", color: "#8b5cf6", icon: "P" },
              { key: "cost", color: "#22d3ee", icon: "C" },
              { key: "knowledge", color: "#f59e0b", icon: "K" },
            ] as const).map(({ key, color, icon }) => (
              <div
                key={key}
                style={{
                  background: "#fff",
                  border: `1px solid ${C.light.border}`,
                  borderRadius: 16,
                  padding: mobile ? "24px 20px" : "32px 28px",
                  display: "flex",
                  flexDirection: "column",
                  gap: 12,
                  boxShadow: "0 2px 8px rgba(0,0,0,0.04)",
                }}
              >
                <div
                  style={{
                    width: 36,
                    height: 36,
                    borderRadius: 8,
                    background: `${color}15`,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: 16,
                    fontWeight: 700,
                    color,
                  }}
                >
                  {icon}
                </div>
                <h4 style={{ fontSize: mobile ? 16 : 18, fontWeight: 700, margin: 0, color }}>
                  {t(`landing.compDetail.${key}`)}
                </h4>
                <p style={{ fontSize: 14, lineHeight: 1.6, color: C.light.muted, margin: 0 }}>
                  {t(`landing.compDetail.${key}Desc`)}
                </p>
              </div>
            ))}
          </div>
        </div>
      </Band>

      {/* ─── FINAL CTA (light) ─── */}
      <Band tone="light" style={{ padding: mobile ? "60px 0" : "100px 0" }}>
        <div style={{ textAlign: "center" }}>
          <h2 style={{ fontSize: mobile ? 28 : 44, fontWeight: 800, color: C.light.text }}>
            {t("landing.v2.cta.title")}
          </h2>
          <p style={{ fontSize: 16, color: C.light.muted, marginTop: 12 }}>
            {t("landing.v2.cta.subtitle")}
          </p>
          <a
            href="/pricing"
            style={{
              display: "inline-flex",
              alignItems: "center",
              marginTop: 28,
              padding: "14px 32px",
              borderRadius: 8,
              background: C.accentGradient,
              color: "#fff",
              fontWeight: 600,
              fontSize: 16,
              textDecoration: "none",
            }}
          >
            {t("landing.v2.cta.button")}
          </a>
        </div>
      </Band>

      {/* ─── FOOTER (dark) ─── */}
      <Band tone="dark" style={{ padding: "32px 0" }}>
        <div
          style={{
            display: "flex",
            flexDirection: mobile ? "column" : "row",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 16,
            fontSize: 13,
            color: C.dark.muted,
          }}
        >
          <span>
            &copy; 2026 {t("landing.v2.footer.copyright")}{" "}
            <a
              href="https://cobracco.it"
              target="_blank"
              rel="noopener noreferrer"
              aria-label="Cobracco"
              style={{ color: "inherit", textDecoration: "underline" }}
            >
              <CobraccoMark />
            </a>
          </span>
          <div style={{ display: "flex", gap: 24 }}>
            <a href="#preview" style={{ color: C.dark.muted, textDecoration: "none" }}>
              {t("landing.v2.nav.product")}
            </a>
            <a href="/pricing" style={{ color: C.dark.muted, textDecoration: "none" }}>
              {t("landing.v2.nav.pricing")}
            </a>
            <a href="#comparison" style={{ color: C.dark.muted, textDecoration: "none" }}>
              {t("landing.comparison")}
            </a>
            {SHOW_LOGIN_CTA && (
              <a href="/login" style={{ color: C.dark.muted, textDecoration: "none" }}>
                {t("landing.login")}
              </a>
            )}
          </div>
        </div>
      </Band>
    </div>
  );
}

/* ─── COMPARISON DATA (13 rows, 8 competitors + Nexus) ─── */
// Columns: [feature_key, Nexus, Cursor, Copilot, Windsurf, Devin, ClaudeCode, Aider, Continue]
const COMPARISON_ROWS: [string, ...(boolean | string)[]][] = [
  ["landing.comp.multiProvider", true, "~", false, "~", false, false, "~", "~"],
  ["landing.comp.mlRouting", true, false, false, false, false, false, false, false],
  ["landing.comp.planActVerify", true, "~", false, "~", true, true, false, false],
  ["landing.comp.agents", true, "~", false, false, "~", true, false, false],
  ["landing.comp.knowledgeBase", true, false, false, false, false, false, false, false],
  ["landing.comp.learning", true, false, false, false, false, false, false, false],
  ["landing.comp.liveMonitoring", true, false, false, false, false, false, false, false],
  ["landing.comp.costBreakdown", true, false, false, false, "~", false, "~", false],
  ["landing.comp.multiTenant", true, false, false, false, false, false, false, false],
  ["landing.comp.onprem", true, false, false, false, "~", false, true, true],
  ["landing.comp.dlp", true, false, false, false, false, false, false, false],
  ["landing.comp.promptCache", true, false, false, false, false, true, false, false],
  ["landing.comp.openSource", "~", false, false, false, false, false, true, true],
];
