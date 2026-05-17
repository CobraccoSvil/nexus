"use client";

import type { ReactNode } from "react";

interface FeatureCardProps {
  icon: ReactNode;
  title: string;
  description: string;
  screenshot?: string;
  tone?: "dark" | "light";
}

export function FeatureCard({
  icon,
  title,
  description,
  screenshot,
  tone = "dark",
}: FeatureCardProps) {
  const isDark = tone === "dark";
  return (
    <div
      style={{
        background: isDark ? "rgba(12,18,30,0.85)" : "#ffffff",
        border: `1px solid ${isDark ? "#1a2336" : "#e5e5e5"}`,
        borderRadius: 12,
        padding: 24,
        display: "flex",
        flexDirection: "column",
        gap: 12,
        transition: "border-color 0.2s",
      }}
    >
      <div style={{ fontSize: 28 }}>{icon}</div>
      <h3
        style={{
          fontSize: 16,
          fontWeight: 700,
          color: isDark ? "#e2e8f0" : "#171717",
          margin: 0,
        }}
      >
        {title}
      </h3>
      <p
        style={{
          fontSize: 14,
          lineHeight: 1.6,
          color: isDark ? "#8494a7" : "#737373",
          margin: 0,
        }}
      >
        {description}
      </p>
      {screenshot && (
        <div
          style={{
            marginTop: 8,
            borderRadius: 8,
            overflow: "hidden",
            border: `1px solid ${isDark ? "#1a2336" : "#e5e5e5"}`,
          }}
        >
          <img
            src={screenshot}
            alt={title}
            style={{ width: "100%", display: "block" }}
            loading="lazy"
            onError={(e) => {
              (e.target as HTMLImageElement).parentElement!.style.display =
                "none";
            }}
          />
        </div>
      )}
    </div>
  );
}
