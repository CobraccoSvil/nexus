"use client";

import { useState, useEffect } from "react";
import { useI18n } from "../../lib/i18n";
import { Band } from "../../components/landing/Band";
import { NavBar } from "../../components/landing/NavBar";
import { PricingTier } from "../../components/landing/PricingTier";
import { ProviderLogo } from "../../components/landing/ProviderLogo";

export default function PricingPage() {
  const { t } = useI18n();
  const [mobile, setMobile] = useState(false);

  useEffect(() => {
    const check = () => setMobile(window.innerWidth < 768);
    check();
    window.addEventListener("resize", check);
    return () => window.removeEventListener("resize", check);
  }, []);

  const tiers = [
    {
      name: t("landing.v2.pricing.selfHost.name"),
      price: t("landing.v2.pricing.selfHost.price"),
      description: t("landing.v2.pricing.selfHost.desc"),
      features: [
        t("landing.v2.pricing.selfHost.feat1"),
        t("landing.v2.pricing.selfHost.feat2"),
        t("landing.v2.pricing.selfHost.feat3"),
        t("landing.v2.pricing.selfHost.feat4"),
      ],
    },
    {
      name: t("landing.v2.pricing.pro.name"),
      price: t("landing.v2.pricing.pro.price"),
      description: t("landing.v2.pricing.pro.desc"),
      features: [
        t("landing.v2.pricing.pro.feat1"),
        t("landing.v2.pricing.pro.feat2"),
        t("landing.v2.pricing.pro.feat3"),
        t("landing.v2.pricing.pro.feat4"),
      ],
    },
    {
      name: t("landing.v2.pricing.team.name"),
      price: t("landing.v2.pricing.team.price"),
      description: t("landing.v2.pricing.team.desc"),
      features: [
        t("landing.v2.pricing.team.feat1"),
        t("landing.v2.pricing.team.feat2"),
        t("landing.v2.pricing.team.feat3"),
        t("landing.v2.pricing.team.feat4"),
      ],
      highlighted: true,
    },
    {
      name: t("landing.v2.pricing.enterprise.name"),
      price: t("landing.v2.pricing.enterprise.price"),
      description: t("landing.v2.pricing.enterprise.desc"),
      features: [
        t("landing.v2.pricing.enterprise.feat1"),
        t("landing.v2.pricing.enterprise.feat2"),
        t("landing.v2.pricing.enterprise.feat3"),
        t("landing.v2.pricing.enterprise.feat4"),
      ],
    },
  ];

  return (
    <div style={{ background: "#fafaf9", minHeight: "100vh" }}>
      <NavBar />

      {/* Header */}
      <Band tone="light" style={{ padding: mobile ? "60px 0" : "80px 0" }}>
        <div style={{ textAlign: "center" }}>
          <h1
            style={{
              fontSize: mobile ? 32 : 48,
              fontWeight: 800,
              color: "#171717",
              letterSpacing: "-0.03em",
            }}
          >
            {t("landing.v2.pricing.title")}
          </h1>
          <p
            style={{
              fontSize: 18,
              color: "#737373",
              marginTop: 12,
              maxWidth: 560,
              margin: "12px auto 0",
            }}
          >
            {t("landing.v2.pricing.subtitle")}
          </p>
        </div>
      </Band>

      {/* Tiers grid */}
      <Band tone="light" style={{ paddingTop: 0 }}>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: mobile
              ? "1fr"
              : "repeat(4, 1fr)",
            gap: 20,
            alignItems: "start",
          }}
        >
          {tiers.map((tier, i) => (
            <PricingTier key={i} {...tier} />
          ))}
        </div>
      </Band>

      {/* Supported providers */}
      <Band tone="light" style={{ paddingTop: 48 }}>
        <div style={{ textAlign: "center", marginBottom: 32 }}>
          <h3
            style={{
              fontSize: mobile ? 18 : 22,
              fontWeight: 700,
              color: "#171717",
            }}
          >
            {t("landing.v2.providers.title")}
          </h3>
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
      </Band>

      {/* Disclaimer */}
      <Band tone="light" style={{ paddingTop: 40, paddingBottom: 60 }}>
        <p
          style={{
            textAlign: "center",
            fontSize: 13,
            color: "#a3a3a3",
            fontStyle: "italic",
            maxWidth: 600,
            margin: "0 auto",
          }}
        >
          {t("landing.v2.pricing.disclaimer")}
        </p>
      </Band>

      {/* Footer */}
      <Band tone="dark" style={{ padding: "32px 0" }}>
        <div
          style={{
            display: "flex",
            flexDirection: mobile ? "column" : "row",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 16,
            fontSize: 13,
            color: "#8494a7",
          }}
        >
          <span>&copy; 2026 {t("landing.v2.footer.copyright")}</span>
          <div style={{ display: "flex", gap: 24 }}>
            <a href="/" style={{ color: "#8494a7", textDecoration: "none" }}>
              {t("landing.v2.nav.product")}
            </a>
            <a href="/pricing" style={{ color: "#8494a7", textDecoration: "none" }}>
              {t("landing.v2.nav.pricing")}
            </a>
          </div>
        </div>
      </Band>
    </div>
  );
}
