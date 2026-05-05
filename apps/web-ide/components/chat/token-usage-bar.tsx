"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";

export interface TokenUsageBarProps {
  sessionId?: string;
  totalTokens: number;
  totalCostUsd: number;
  budgetUsd?: number;
}

export function TokenUsageBar({
  totalTokens,
  totalCostUsd,
  budgetUsd,
}: TokenUsageBarProps) {
  const tc = useThemeColors();
  const [expanded, setExpanded] = useState(false);

  if (totalTokens === 0 && totalCostUsd === 0) return null;

  const hasBudget = budgetUsd != null && budgetUsd > 0;
  const ratio = hasBudget ? totalCostUsd / budgetUsd! : null;

  let barColor = tc.textMuted;
  if (hasBudget && ratio != null) {
    if (ratio < 0.5) barColor = tc.success;
    else if (ratio < 0.8) barColor = tc.warning;
    else barColor = tc.error;
  }

  const fillPct = hasBudget && ratio != null ? Math.min(ratio * 100, 100) : null;

  const label =
    totalTokens >= 1_000_000
      ? `${(totalTokens / 1_000_000).toFixed(1)}M token`
      : totalTokens >= 1_000
      ? `${(totalTokens / 1_000).toFixed(1)}K token`
      : `${totalTokens} token`;

  const costLabel =
    totalCostUsd === 0
      ? "$0.00"
      : totalCostUsd < 0.01
      ? `$${totalCostUsd.toFixed(4)}`
      : `$${totalCostUsd.toFixed(2)}`;

  return (
    <div style={{ position: "relative" }}>
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        title={hasBudget ? `Budget: $${budgetUsd!.toFixed(2)}` : "Token consumati nella sessione"}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          height: 24,
          padding: "0 8px",
          borderRadius: 6,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          cursor: "pointer",
          color: hasBudget ? barColor : tc.textMuted,
          fontSize: 11,
          fontFamily: "inherit",
          whiteSpace: "nowrap",
          width: "100%",
          overflow: "hidden",
          position: "relative",
        }}
      >
        {/* progress fill */}
        {fillPct != null && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              width: `${fillPct}%`,
              background: barColor,
              opacity: 0.12,
              borderRadius: 6,
              pointerEvents: "none",
            }}
          />
        )}
        <span style={{ position: "relative", zIndex: 1 }}>
          {label} &bull; {costLabel}
          {hasBudget && ratio != null && (
            <span style={{ marginLeft: 4, opacity: 0.8 }}>
              ({Math.round(ratio * 100)}%)
            </span>
          )}
        </span>
        <span
          style={{
            marginLeft: "auto",
            fontSize: 9,
            opacity: 0.5,
            position: "relative",
            zIndex: 1,
          }}
        >
          {expanded ? "▲" : "▼"}
        </span>
      </button>

      {expanded && (
        <div
          style={{
            position: "absolute",
            bottom: "calc(100% + 4px)",
            left: 0,
            right: 0,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            padding: "8px 10px",
            fontSize: 11,
            color: tc.textMuted,
            zIndex: 20,
            boxShadow: "0 4px 16px rgba(0,0,0,0.25)",
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: 6, color: tc.textMuted }}>
            Dettaglio sessione
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 2 }}>
            <span>Token totali</span>
            <span style={{ color: barColor }}>{totalTokens.toLocaleString()}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 2 }}>
            <span>Costo totale</span>
            <span style={{ color: barColor }}>{costLabel}</span>
          </div>
          {hasBudget && (
            <div style={{ display: "flex", justifyContent: "space-between" }}>
              <span>Budget</span>
              <span>${budgetUsd!.toFixed(2)}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
