"use client";

import React from "react";

interface StatusBadgeProps {
  /** Colore base del badge (es. tc.success, tc.error, tc.textMuted). */
  color: string;
  /** Testo mostrato (ignorato per variant "dot"). */
  label?: React.ReactNode;
  /** dot = solo pallino; label = solo testo; both = pallino + testo. */
  variant?: "dot" | "label" | "both";
  title?: string;
  /** Stili aggiuntivi mergiati sul contenitore (es. troncamento, cursor). */
  style?: React.CSSProperties;
}

/**
 * Badge "pallino colorato + testo" riusabile.
 * Riproduce la pillola inline ricorrente:
 *  display inline-flex, alignItems center, gap 4, padding "3px 8px",
 *  borderRadius 12, background `${color}18`, border `1px solid ${color}40`,
 *  color, fontSize 11, fontWeight 600.
 * Il dot e' un cerchio 6x6 dello stesso colore.
 */
export function StatusBadge({
  color,
  label,
  variant = "both",
  title,
  style,
}: StatusBadgeProps) {
  const showDot = variant === "dot" || variant === "both";
  const showLabel = variant === "label" || variant === "both";

  return (
    <span
      title={title}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 4,
        padding: "3px 8px",
        borderRadius: 12,
        background: `${color}18`,
        border: `1px solid ${color}40`,
        color,
        fontSize: 11,
        fontWeight: 600,
        ...style,
      }}
    >
      {showDot && (
        <span
          style={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            background: color,
            flexShrink: 0,
          }}
        />
      )}
      {showLabel && label}
    </span>
  );
}
