"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";

export interface TokenUsageBarProps {
  sessionId?: string;
  totalTokens: number;
  totalCostUsd: number;
  budgetUsd?: number;
  /** Context window in token del modello corrente. Se presente, attiva l'indicatore di riempimento. */
  contextWindow?: number | null;
  /** Token di input usati nell'ultimo turno (stima del riempimento context window). */
  lastInputTokens?: number | null;
  /** Modello corrente, mostrato nel tooltip dettagliato. */
  modelLabel?: string | null;
}

export function TokenUsageBar({
  totalTokens,
  totalCostUsd,
  budgetUsd,
  contextWindow,
  lastInputTokens,
  modelLabel,
}: TokenUsageBarProps) {
  const tc = useThemeColors();
  const [expanded, setExpanded] = useState(false);

  if (totalTokens === 0 && totalCostUsd === 0) return null;

  const hasBudget = budgetUsd != null && budgetUsd > 0;
  const hasContext =
    contextWindow != null &&
    contextWindow > 0 &&
    lastInputTokens != null &&
    lastInputTokens > 0;
  const ratio = hasBudget
    ? totalCostUsd / budgetUsd!
    : hasContext
      ? lastInputTokens! / contextWindow!
      : null;

  let barColor = tc.textMuted;
  if (ratio != null) {
    if (ratio < 0.5) barColor = tc.success;
    else if (ratio < 0.8) barColor = tc.warning;
    else barColor = tc.error;
  }

  const fillPct = ratio != null ? Math.min(ratio * 100, 100) : null;

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
        title={
          hasBudget
            ? `Budget: $${budgetUsd!.toFixed(2)}`
            : hasContext
              ? `Context window: ${lastInputTokens!.toLocaleString()} / ${contextWindow!.toLocaleString()} token`
              : "Token consumati nella sessione"
        }
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
          color: ratio != null ? barColor : tc.textMuted,
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
          {ratio != null && (
            <span style={{ marginLeft: 4, opacity: 0.8 }}>
              ({Math.round(ratio * 100)}% {hasBudget ? "budget" : "ctx"})
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
          {hasContext && (
            <>
              <div style={{ display: "flex", justifyContent: "space-between", marginTop: 4 }}>
                <span>Ultimo input</span>
                <span style={{ color: barColor }}>
                  {lastInputTokens!.toLocaleString()} token
                </span>
              </div>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span>Context window{modelLabel ? ` (${modelLabel})` : ""}</span>
                <span>{contextWindow!.toLocaleString()} token</span>
              </div>
              {ratio != null && ratio >= 0.7 && !hasBudget && (
                <div
                  style={{
                    marginTop: 6,
                    paddingTop: 6,
                    borderTop: `1px solid ${tc.border}`,
                    color: ratio >= 0.8 ? tc.error : tc.warning,
                    fontSize: 10,
                    lineHeight: 1.4,
                  }}
                >
                  {ratio >= 0.8
                    ? "Context quasi pieno: compatta la chat (icona ⌁) per evitare perdita di informazioni."
                    : "Context sopra il 70%: valuta di compattare la chat a breve."}
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
