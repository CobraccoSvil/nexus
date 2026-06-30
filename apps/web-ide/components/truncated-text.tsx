"use client";

import { useState } from "react";
import type { useThemeColors } from "../lib/theme";

/**
 * Componente per testi troncati con tooltip.
 * Mostra il testo con ellipsis e un tooltip al hover.
 */
export function TruncatedText({
  text,
  className = "",
  maxWidth = 300,
  tc,
  style = {},
}: {
  text: string;
  className?: string;
  maxWidth?: number | string;
  tc?: ReturnType<typeof useThemeColors>;
  style?: React.CSSProperties;
}) {
  const [showTooltip, setShowTooltip] = useState(false);

  // Se non c'è theme colors, fallback a title nativo
  if (!tc) {
    return (
      <span
        className={className}
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          maxWidth,
          display: "inline-block",
          ...style,
        }}
        title={text}
      >
        {text}
      </span>
    );
  }

  return (
    <div
      style={{
        position: "relative",
        display: "inline-block",
        width: typeof maxWidth === "number" ? maxWidth : "auto",
      }}
      onMouseEnter={() => setShowTooltip(true)}
      onMouseLeave={() => setShowTooltip(false)}
    >
      <span
        className={className}
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          maxWidth,
          display: "block",
          ...style,
        }}
      >
        {text}
      </span>

      {showTooltip && text && (
        <div
          style={{
            position: "absolute",
            bottom: "100%",
            left: "50%",
            transform: "translateX(-50%)",
            marginBottom: 8,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 6,
            padding: "6px 10px",
            fontSize: 12,
            color: tc.text,
            zIndex: 1000,
            boxShadow: "0 2px 8px rgba(0,0,0,0.2)",
            maxWidth: 400,
            wordBreak: "break-word",
            whiteSpace: "normal",
            pointerEvents: "none",
          }}
        >
          {text}
        </div>
      )}
    </div>
  );
}
