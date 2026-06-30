"use client";

import { useState } from "react";
import { type GitLogEntry } from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";

interface CommitLogProps {
  logEntries: GitLogEntry[];
}

export function CommitLog({ logEntries }: CommitLogProps) {
  const tc = useThemeColors();
  const [expandedCommit, setExpandedCommit] = useState<string | null>(null);

  return (
    <div>
      <div style={{ fontSize: 10, fontWeight: 700, color: tc.textMuted, textTransform: "uppercase", letterSpacing: "0.06em", padding: "2px 6px", marginBottom: 2 }}>
        Log recente
      </div>
      {logEntries.slice(0, 12).map((entry) => {
        const dateStr = entry.date
          ? new Date(entry.date).toLocaleDateString("it-IT", { day: "2-digit", month: "short" })
          : "";
        const isExpanded = expandedCommit === entry.commit;
        const hasBody = Boolean(entry.body?.trim());
        return (
          <div key={entry.commit}>
            <div
              title={`${entry.subject}\n${entry.author} — ${entry.date}`}
              style={{ display: "flex", alignItems: "center", gap: 4, padding: "3px 6px", borderRadius: 3, cursor: hasBody ? "pointer" : "default", minWidth: 0, width: "100%" }}
              onClick={() => hasBody && setExpandedCommit(isExpanded ? null : entry.commit)}
              onMouseEnter={e => { (e.currentTarget as HTMLDivElement).style.background = tc.bgInput; }}
              onMouseLeave={e => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
            >
              <span style={{ fontFamily: '"JetBrains Mono", monospace', fontSize: 10, color: tc.accent, flexShrink: 0, minWidth: "40px", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {entry.shortCommit}
              </span>
              <span style={{ fontSize: 11, color: tc.textMuted, flexShrink: 0, whiteSpace: "nowrap" }}>
                {dateStr}
              </span>
              <span style={{ fontSize: 12, color: tc.text, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: 1, minWidth: 0 }}>
                {entry.subject}
              </span>
              <span style={{ fontSize: 10, color: tc.textMuted, flexShrink: 0, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis" }}>
                {entry.author}
              </span>
              {hasBody && (
                <span style={{ fontSize: 10, color: tc.textMuted, flexShrink: 0 }}>
                  {isExpanded ? "▲" : "▼"}
                </span>
              )}
            </div>
            {isExpanded && hasBody && (
              <div style={{
                margin: "0 6px 4px 6px",
                padding: "6px 8px",
                borderRadius: 4,
                background: tc.bgCard,
                border: `1px solid ${tc.border}`,
                fontSize: 11,
                color: tc.textSecondary,
                fontFamily: '"JetBrains Mono", monospace',
                whiteSpace: "pre-wrap",
                wordBreak: "break-word",
              }}>
                {entry.body}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
