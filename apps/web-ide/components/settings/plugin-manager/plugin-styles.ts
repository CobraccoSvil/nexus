import type { Theme } from "../../../lib/theme";

export function actionButtonStyle(tc: Theme, disabled = false): React.CSSProperties {
  return {
    border: `1px solid ${tc.accent}`,
    background: `${tc.accent}15`,
    color: tc.accent,
    borderRadius: 7,
    padding: "5px 10px",
    fontSize: 12,
    cursor: disabled ? "not-allowed" : "pointer",
    fontWeight: 600,
    opacity: disabled ? 0.55 : 1,
  };
}

export function inputStyle(tc: Theme): React.CSSProperties {
  return {
    width: "100%",
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    borderRadius: 8,
    padding: "7px 10px",
    fontSize: 12,
    boxSizing: "border-box",
  };
}

export function selectStyle(tc: Theme, width = 150): React.CSSProperties {
  return {
    minWidth: width,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    borderRadius: 8,
    padding: "6px 8px",
    fontSize: 12,
  };
}
