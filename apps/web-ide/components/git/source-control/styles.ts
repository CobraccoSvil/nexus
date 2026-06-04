import type { useThemeColors } from "../../../lib/theme";

type ThemeColors = ReturnType<typeof useThemeColors>;

export function buttonStyle(tc: ThemeColors, disabled: boolean) {
  return {
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: disabled ? tc.bgCard : tc.accentBg,
    color: tc.text,
    cursor: disabled ? "not-allowed" : "pointer",
    flexShrink: 0,
    whiteSpace: "nowrap" as const,
  };
}

export function inputStyle(tc: ThemeColors) {
  return {
    flex: 1,
    minWidth: 0,
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    boxSizing: "border-box" as const,
    width: "100%",
  };
}

export function sectionTitleStyle(tc: ThemeColors) {
  return {
    color: tc.text,
    fontSize: 12,
    fontWeight: 700,
    textTransform: "uppercase" as const,
    letterSpacing: "0.04em",
  };
}

export function cardStyle(tc: ThemeColors) {
  return {
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    background: tc.bgCard,
    padding: "8px 10px",
    display: "flex",
    flexDirection: "column",
    gap: 6,
    minWidth: 0,
    width: "100%",
    overflow: "hidden",
    boxSizing: "border-box",
  } as const;
}

export function smallButtonStyle(tc: ThemeColors, disabled: boolean) {
  return {
    ...buttonStyle(tc, disabled),
    padding: "6px 10px",
    fontSize: 12,
    fontWeight: 600,
  } as const;
}

export function statusBadgeStyle(
  tc: ThemeColors,
  tone: "neutral" | "success" | "warning" | "error",
) {
  const colors = {
    neutral: { color: tc.textSecondary, background: tc.bgInput },
    success: { color: tc.success, background: tc.accentBg },
    warning: { color: tc.warning, background: tc.bgInput },
    error: { color: tc.error, background: tc.bgInput },
  }[tone];

  return {
    display: "inline-flex",
    alignItems: "center",
    gap: 6,
    borderRadius: 999,
    padding: "4px 8px",
    fontSize: 11,
    fontWeight: 700,
    background: colors.background,
    color: colors.color,
    border: `1px solid ${tc.border}`,
  } as const;
}

export function linkButtonStyle(tc: ThemeColors) {
  return {
    ...buttonStyle(tc, false),
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    textDecoration: "none",
    fontSize: 12,
    fontWeight: 600,
  } as const;
}
