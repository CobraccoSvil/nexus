import type { Theme } from "./theme";

/**
 * Button style helper
 * Fornisce stili di base per button con varianti primary/secondary/ghost
 */
export const buttonStyles = (
  tc: Theme,
  variant: "primary" | "secondary" | "ghost" = "primary"
): React.CSSProperties => {
  const baseStyle: React.CSSProperties = {
    borderRadius: 6,
    padding: "8px 12px",
    fontSize: 12,
    fontWeight: 600,
    cursor: "pointer",
    fontFamily: "inherit",
    transition: "all 0.15s",
  };

  if (variant === "primary") {
    return {
      ...baseStyle,
      border: `1px solid ${tc.accent}`,
      background: `${tc.accent}22`,
      color: tc.accent,
    };
  }
  if (variant === "secondary") {
    return {
      ...baseStyle,
      border: `1px solid ${tc.border}`,
      background: tc.bgInput,
      color: tc.text,
    };
  }
  return {
    ...baseStyle,
    border: `1px solid ${tc.border}`,
    background: "transparent",
    color: tc.textMuted,
  };
};

/**
 * Input style helper
 * Fornisce stili di base per input, textarea, select
 */
export const inputStyle = (tc: Theme): React.CSSProperties => ({
  background: tc.bgInput,
  border: `1px solid ${tc.border}`,
  borderRadius: 6,
  color: tc.text,
  fontSize: 13,
  padding: "6px 10px",
  fontFamily: "inherit",
  width: "100%",
  boxSizing: "border-box",
});

/**
 * Badge style helper
 * Fornisce gli stili del badge inline ricorrente (pillola colorata).
 * variant -> color: success/error/warning -> omonimi tema,
 * info -> textSecondary, default -> textMuted.
 */
export type BadgeVariant = "success" | "error" | "warning" | "info" | "default";

export const badgeStyles = (
  tc: Theme,
  variant: BadgeVariant = "default"
): React.CSSProperties => {
  const colorByVariant: Record<BadgeVariant, string> = {
    success: tc.success,
    error: tc.error,
    warning: tc.warning,
    info: tc.textSecondary,
    default: tc.textMuted,
  };
  const color = colorByVariant[variant];
  return {
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
  };
};

/**
 * Card style helper
 * Fornisce stili di base per card container
 */
export const cardStyle = (
  tc: Theme,
  size: "sm" | "md" = "md"
): React.CSSProperties => ({
  border: `1px solid ${tc.border}`,
  borderRadius: size === "sm" ? 10 : 12,
  background: tc.bgCard,
  padding: size === "sm" ? 12 : 16,
  display: "flex",
  flexDirection: "column",
  gap: 10,
});
