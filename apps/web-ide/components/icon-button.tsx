"use client";

import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from "react";
import { useThemeColors } from "../lib/theme";

type IconButtonVariant = "default" | "primary";

interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  label: string;
  children: ReactNode;
  size?: number;
  variant?: IconButtonVariant;
  active?: boolean;
  borderless?: boolean;
  style?: CSSProperties;
}

export function IconButton({
  label,
  children,
  size = 30,
  variant = "default",
  active = false,
  borderless = false,
  disabled,
  style,
  type = "button",
  ...rest
}: IconButtonProps) {
  const tc = useThemeColors();

  return (
    <button
      type={type}
      title={label}
      aria-label={label}
      disabled={disabled}
      style={{
        width: size,
        height: size,
        borderRadius: 8,
        border: borderless ? "none" : `1px solid ${active ? tc.accent : tc.border}`,
        background:
          variant === "primary"
            ? disabled
              ? tc.bgInput
              : tc.accentBg
            : disabled
              ? tc.bgInput
              : active
                ? tc.accentBg
                : tc.bgCard,
        color: disabled ? tc.textMuted : active ? tc.accent : tc.textSecondary,
        cursor: disabled ? "not-allowed" : "pointer",
        display: "inline-grid",
        placeItems: "center",
        padding: 0,
        fontSize: 13,
        lineHeight: 1,
        flexShrink: 0,
        ...style,
      }}
      {...rest}
    >
      {children}
    </button>
  );
}
