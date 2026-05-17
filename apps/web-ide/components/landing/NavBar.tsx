"use client";

import { useI18n, LOCALE_LABELS, type Locale } from "../../lib/i18n";

export function NavBar() {
  const { t, locale, setLocale } = useI18n();

  return (
    <nav
      style={{
        position: "sticky",
        top: 0,
        zIndex: 100,
        background: "rgba(250,250,249,0.92)",
        backdropFilter: "blur(12px)",
        borderBottom: "1px solid #e5e5e5",
        padding: "0 24px",
      }}
    >
      <div
        style={{
          maxWidth: 1200,
          margin: "0 auto",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          height: 56,
        }}
      >
        {/* Logo */}
        <a
          href="/"
          style={{
            fontWeight: 700,
            fontSize: 20,
            color: "#171717",
            textDecoration: "none",
            letterSpacing: "-0.02em",
          }}
        >
          Nexus
        </a>

        {/* Links */}
        <div style={{ display: "flex", alignItems: "center", gap: 28 }}>
          <a href="#preview" style={linkStyle}>
            {t("landing.v2.nav.product" as any)}
          </a>
          <a href="/pricing" style={linkStyle}>
            {t("landing.v2.nav.pricing" as any)}
          </a>

          {/* Language selector */}
          <select
            value={locale}
            onChange={(e) => setLocale(e.target.value as Locale)}
            style={{
              background: "transparent",
              border: "1px solid #e5e5e5",
              borderRadius: 6,
              padding: "4px 8px",
              fontSize: 13,
              color: "#171717",
              cursor: "pointer",
            }}
          >
            {(Object.keys(LOCALE_LABELS) as Locale[]).map((l) => (
              <option key={l} value={l}>
                {LOCALE_LABELS[l]}
              </option>
            ))}
          </select>
        </div>
      </div>
    </nav>
  );
}

const linkStyle: React.CSSProperties = {
  color: "#525252",
  textDecoration: "none",
  fontSize: 14,
  fontWeight: 500,
  transition: "color 0.15s",
};
