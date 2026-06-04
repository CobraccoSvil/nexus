"use client";

import { CATEGORIES } from "./types";
import type { Tc } from "./types";

interface CategorySidebarProps {
  tc: Tc;
  activeCategory: string;
  allActiveFindingsCount: number;
  catCounts: Record<string, number>;
  handleCategoryChange: (cat: string) => void;
}

export function CategorySidebar({
  tc,
  activeCategory,
  allActiveFindingsCount,
  catCounts,
  handleCategoryChange,
}: CategorySidebarProps) {
  return (
    <div style={{
      width: 140, flexShrink: 0, borderRight: `1px solid ${tc.border}`,
      overflowY: "auto", background: tc.bgSidebar,
    }}>
      {CATEGORIES.map(cat => {
        // Usa i conteggi live (findings attivi, FP esclusi) per tutti i badge.
        // Math.max con scanCatCount era sbagliato: gonfiava i conteggi con i false positive
        // (es. 1221 FP complexity di AdminConsole apparivano come 1240 invece di 19).
        const count = cat.id === "all"
          ? allActiveFindingsCount
          : (catCounts[cat.id] ?? 0);
        if (cat.id !== "all" && count === 0) return null;
        return (
          <button
            key={cat.id}
            onClick={() => handleCategoryChange(cat.id)}
            style={{
              display: "flex", justifyContent: "space-between", alignItems: "center",
              width: "100%", padding: "6px 10px", fontSize: 11, textAlign: "left",
              background: activeCategory === cat.id ? tc.bgCard : "transparent",
              color: activeCategory === cat.id ? tc.accent : tc.textSecondary,
              border: "none", borderBottom: `1px solid ${tc.border}`, cursor: "pointer",
            }}
          >
            <span>{cat.label}</span>
            {count > 0 && (
              <span style={{
                background: tc.bgInput, borderRadius: 999, padding: "1px 6px",
                fontSize: 10, color: tc.textMuted,
              }}>{count}</span>
            )}
          </button>
        );
      })}
    </div>
  );
}
