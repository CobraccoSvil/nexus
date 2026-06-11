/**
 * Stile condiviso dei pulsanti-icona (punto unico, regola L / ADR 0026).
 * Prima duplicato identico in components/editor/editor-area.tsx e
 * components/shell/shell-helpers.tsx.
 */

import type { Theme } from "./theme";

export function iconButton(tc: Theme, disabled = false, active = false) {
  return {
    width: 30,
    height: 30,
    border: `1px solid ${active ? tc.accent : tc.border}`,
    background: disabled ? tc.bgInput : active ? tc.accentBg : tc.bgCard,
    color: disabled ? tc.textMuted : active ? tc.accent : tc.textSecondary,
    borderRadius: 7,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 13,
    lineHeight: 1,
  } as const;
}
