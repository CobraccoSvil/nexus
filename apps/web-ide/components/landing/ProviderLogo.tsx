"use client";

interface ProviderLogoProps {
  name: string;
  tone?: "dark" | "light";
}

const PROVIDER_COLORS: Record<string, string> = {
  OpenAI: "#10a37f",
  Anthropic: "#d4a574",
  Google: "#4285f4",
  DeepSeek: "#5b7bd5",
  Mistral: "#f7931e",
};

export function ProviderLogo({ name, tone = "dark" }: ProviderLogoProps) {
  const color = PROVIDER_COLORS[name] || "#888";
  const isDark = tone === "dark";

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        gap: 8,
      }}
    >
      {/* Simple colored circle with initial */}
      <div
        style={{
          width: 48,
          height: 48,
          borderRadius: "50%",
          background: `${color}20`,
          border: `2px solid ${color}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontSize: 18,
          fontWeight: 700,
          color,
        }}
      >
        {name[0]}
      </div>
      <span
        style={{
          fontSize: 12,
          fontWeight: 500,
          color: isDark ? "#8494a7" : "#737373",
        }}
      >
        {name}
      </span>
    </div>
  );
}
