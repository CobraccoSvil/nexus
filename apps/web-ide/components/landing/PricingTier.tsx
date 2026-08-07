"use client";
import { useI18n } from "../../lib/i18n";

interface PricingTierProps {
  name: string;
  price: string;
  description: string;
  features: string[];
  highlighted?: boolean;
  currency?: string;
}

export function PricingTier({
  name,
  price,
  description,
  features,
  highlighted = false,
  currency = "€",
}: PricingTierProps) {
  const { t } = useI18n();
  return (
    <div
      style={{
        background: highlighted
          ? "linear-gradient(135deg, rgba(91,163,230,0.08), rgba(139,92,246,0.05))"
          : "#ffffff",
        border: `${highlighted ? 2 : 1}px solid ${highlighted ? "#5ba3e6" : "#e5e5e5"}`,
        borderRadius: 16,
        padding: 32,
        display: "flex",
        flexDirection: "column",
        gap: 16,
        position: "relative",
      }}
    >
      {highlighted && (
        <div
          style={{
            position: "absolute",
            top: -12,
            left: "50%",
            transform: "translateX(-50%)",
            background: "linear-gradient(135deg, #5ba3e6, #8b5cf6)",
            color: "#fff",
            fontSize: 11,
            fontWeight: 700,
            padding: "4px 12px",
            borderRadius: 20,
            textTransform: "uppercase",
            letterSpacing: "0.05em",
          }}
        >
          {t("landing.popular")}
        </div>
      )}

      <h3
        style={{
          fontSize: 20,
          fontWeight: 700,
          color: "#171717",
          margin: 0,
        }}
      >
        {name}
      </h3>

      <div style={{ display: "flex", alignItems: "baseline", gap: 4 }}>
        {price === "Free" || price === "Gratis" || price === "Custom" || price === "Personalizzato" || price === "Personalizado" ? (
          <span style={{ fontSize: 36, fontWeight: 800, color: "#171717" }}>
            {price}
          </span>
        ) : (
          <>
            <span style={{ fontSize: 14, color: "#737373" }}>{currency}</span>
            <span style={{ fontSize: 36, fontWeight: 800, color: "#171717" }}>
              {price.replace(/[^\d]/g, "")}
            </span>
            <span style={{ fontSize: 14, color: "#737373" }}>
              /{price.replace(/^\d+/, "").replace(/^\//, "")}
            </span>
          </>
        )}
      </div>

      <p
        style={{
          fontSize: 14,
          lineHeight: 1.5,
          color: "#737373",
          margin: 0,
          minHeight: 42,
        }}
      >
        {description}
      </p>

      <ul
        style={{
          listStyle: "none",
          padding: 0,
          margin: 0,
          display: "flex",
          flexDirection: "column",
          gap: 10,
        }}
      >
        {features.map((feat, i) => (
          <li
            key={i}
            style={{
              fontSize: 14,
              color: "#525252",
              display: "flex",
              alignItems: "center",
              gap: 8,
            }}
          >
            <span style={{ color: "#5ba3e6", fontWeight: 700 }}>-</span>
            {feat}
          </li>
        ))}
      </ul>
    </div>
  );
}
