"use client";

import { TruncatedText } from "../../truncated-text";
import type { QualityFinding } from "../../../lib/api-client";
import { SEVERITY_COLOR } from "./types";
import type { Tc } from "./types";

interface FindingItemProps {
  tc: Tc;
  finding: QualityFinding;
  selected: boolean;
  onSendToChat?: (message: string) => void;
  toggleFindingSelection: (id: string) => void;
  handleFix: (finding: QualityFinding) => void;
  handleMarkFixed: (finding: QualityFinding) => void;
  markFalsePositive: (findingId: string, ruleKey?: string) => void;
}

export function FindingItem({
  tc,
  finding,
  selected,
  onSendToChat,
  toggleFindingSelection,
  handleFix,
  handleMarkFixed,
  markFalsePositive,
}: FindingItemProps) {
  return (
    <div
      style={{
        padding: "8px 12px", borderBottom: `1px solid ${tc.border}`,
        display: "flex", flexDirection: "column", gap: 4,
        background: selected ? `${tc.accent}18` : "transparent",
      }}
    >
      <div style={{ display: "flex", alignItems: "flex-start", gap: 8 }}>
        <input
          type="checkbox"
          checked={selected}
          onChange={() => toggleFindingSelection(finding.id)}
          style={{ flexShrink: 0, marginTop: 3, cursor: "pointer" }}
        />
        <span style={{
          color: SEVERITY_COLOR[finding.severity] ?? tc.textMuted,
          fontSize: 10, fontWeight: 700, flexShrink: 0, marginTop: 2,
        }}>
          •
        </span>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
            <TruncatedText
              text={finding.filePath + (finding.lineNumber ? `:${finding.lineNumber}` : "")}
              maxWidth={300}
              tc={tc}
              style={{
                fontFamily: 'var(--font-mono)',
                fontSize: 11,
                color: tc.accent,
              }}
            />
            <span style={{
              fontSize: 10, background: tc.bgInput, borderRadius: 4,
              padding: "1px 6px", color: tc.textMuted,
            }}>
              {finding.category}
            </span>
          </div>
          <div style={{ fontSize: 12, color: tc.text, marginTop: 2 }}>{finding.title}</div>
          <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 1 }}>{finding.detail}</div>
        </div>
        <div style={{ display: "flex", gap: 4, flexShrink: 0 }}>
          {onSendToChat && (
            <button
              onClick={() => handleFix(finding)}
              style={{
                background: tc.accent, color: "#fff", border: "none", borderRadius: 4,
                padding: "2px 8px", fontSize: 10, cursor: "pointer",
              }}
            >
              Fix
            </button>
          )}
          <button
            onClick={() => handleMarkFixed(finding)}
            style={{
              background: "none", border: `1px solid ${tc.border}`, borderRadius: 4,
              padding: "2px 6px", fontSize: 10, cursor: "pointer", color: tc.textMuted,
            }}
            title="Segna come risolto"
          >
            ✓
          </button>
          <button
            title="Segna come falso positivo"
            onClick={(e) => { e.stopPropagation(); markFalsePositive(finding.id, finding.rule_key || finding.category); }}
            style={{
              background: "none", border: "none", borderRadius: 4,
              padding: "2px 4px", fontSize: 10, cursor: "pointer",
              color: "#f87171", opacity: 0.4,
            }}
            onMouseEnter={e => (e.currentTarget.style.opacity = "1")}
            onMouseLeave={e => (e.currentTarget.style.opacity = "0.4")}
          >✗</button>
        </div>
      </div>
    </div>
  );
}
