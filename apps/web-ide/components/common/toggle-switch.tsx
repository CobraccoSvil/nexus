"use client";

import React, { useState } from "react";
import { useThemeColors } from "../../lib/theme";

interface ToggleSwitchProps {
  enabled: boolean;
  onToggle: () => void | Promise<void>;
  disabled?: boolean;
  size?: "sm" | "md";
  ariaLabel?: string;
  title?: string;
}

/**
 * Toggle switch riusabile.
 * Riproduce esattamente il toggle inline ricorrente:
 *  - md: width 38, height 20, borderRadius 10, pallino 16
 *  - sm: width 32, height 16, borderRadius 8, pallino 12
 * Background: enabled ? tc.success : tc.textMuted. Pallino bianco animato a sinistra.
 * Gestione busy interna: se onToggle ritorna una Promise, il toggle si disabilita
 * finche' non risolve.
 */
export function ToggleSwitch({
  enabled,
  onToggle,
  disabled = false,
  size = "md",
  ariaLabel,
  title,
}: ToggleSwitchProps) {
  const tc = useThemeColors();
  const [busy, setBusy] = useState(false);

  const dims =
    size === "sm"
      ? { width: 32, height: 16, radius: 8, inner: 12, offset: 2, on: 16 }
      : { width: 38, height: 20, radius: 10, inner: 16, offset: 2, on: 20 };

  const isLocked = disabled || busy;

  const handleClick = async () => {
    if (isLocked) return;
    const result = onToggle();
    if (result instanceof Promise) {
      setBusy(true);
      try {
        await result;
      } finally {
        setBusy(false);
      }
    }
  };

  return (
    <button
      type="button"
      role="switch"
      aria-checked={enabled}
      aria-label={ariaLabel}
      title={title}
      disabled={isLocked}
      onClick={handleClick}
      style={{
        width: dims.width,
        height: dims.height,
        borderRadius: dims.radius,
        border: "none",
        background: enabled ? tc.success : tc.textMuted,
        cursor: isLocked ? "not-allowed" : "pointer",
        position: "relative",
        transition: "background 0.2s",
        flexShrink: 0,
        opacity: isLocked ? 0.6 : 1,
        padding: 0,
      }}
    >
      <span
        style={{
          position: "absolute",
          top: dims.offset,
          left: enabled ? dims.on : dims.offset,
          width: dims.inner,
          height: dims.inner,
          borderRadius: "50%",
          background: "#fff",
          transition: "left 0.2s",
          boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
        }}
      />
    </button>
  );
}
