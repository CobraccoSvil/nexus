"use client";

import { useI18n } from "../../lib/i18n";

export function HeroSplit() {
  const { t } = useI18n();

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "1fr 1fr",
        gap: 48,
        alignItems: "center",
        minHeight: 480,
      }}
    >
      {/* Left: text */}
      <div>
        <h1
          style={{
            fontSize: "clamp(32px, 4vw, 56px)",
            fontWeight: 800,
            lineHeight: 1.1,
            letterSpacing: "-0.03em",
            marginBottom: 20,
            color: "#e2e8f0",
          }}
        >
          {t("landing.v2.hero.title" as any)}
        </h1>
        <p
          style={{
            fontSize: 18,
            lineHeight: 1.6,
            color: "#8494a7",
            marginBottom: 32,
            maxWidth: 480,
          }}
        >
          {t("landing.v2.hero.subtitle" as any)}
        </p>
        <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
          <a href="/pricing" style={ctaPrimary}>
            {t("landing.v2.hero.cta" as any)}
          </a>
          <a href="#preview" style={ctaSecondary}>
            {t("landing.v2.hero.ctaSecondary" as any)}
          </a>
        </div>
      </div>

      {/* Right: screenshot placeholder */}
      <div
        style={{
          position: "relative",
          borderRadius: 12,
          overflow: "hidden",
          border: "1px solid rgba(255,255,255,0.08)",
          background: "rgba(12,18,30,0.6)",
          aspectRatio: "16/10",
        }}
      >
        <img
          src="/screenshots/hero-ide.jpg"
          alt="Nexus IDE"
          style={{
            width: "100%",
            height: "100%",
            objectFit: "cover",
            display: "block",
          }}
          loading="eager"
          onError={(e) => {
            (e.target as HTMLImageElement).style.display = "none";
          }}
        />
        {/* Fallback gradient if no screenshot */}
        <div
          style={{
            position: "absolute",
            inset: 0,
            background:
              "linear-gradient(135deg, rgba(91,163,230,0.15), rgba(139,92,246,0.1))",
            zIndex: -1,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            color: "#5ba3e6",
            fontSize: 14,
            fontWeight: 500,
          }}
        >
          Nexus IDE Preview
        </div>
      </div>
    </div>
  );
}

const ctaPrimary: React.CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  padding: "12px 24px",
  borderRadius: 8,
  background: "linear-gradient(135deg, #5ba3e6, #8b5cf6)",
  color: "#fff",
  fontWeight: 600,
  fontSize: 15,
  textDecoration: "none",
  transition: "opacity 0.15s",
};

const ctaSecondary: React.CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  padding: "12px 24px",
  borderRadius: 8,
  border: "1px solid rgba(255,255,255,0.15)",
  color: "#e2e8f0",
  fontWeight: 500,
  fontSize: 15,
  textDecoration: "none",
  transition: "border-color 0.15s",
};
