"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useProjectStore, selectDatabaseQueries } from "../../lib/project-dispatcher/store";

// ── Sub-componente: query recenti dal dispatcher SSE ───────────────────────
// Mostra le ultime DbQueryRun emesse dal tool project_db_query (max 100 in
// store, qui ne renderizziamo 10). Live: niente API call, niente polling.
export function RecentQueriesSection() {
  const tc = useThemeColors();
  const queries = useProjectStore(selectDatabaseQueries);
  const [collapsed, setCollapsed] = useState(true);

  if (queries.length === 0) return null;

  const recent = queries.slice(0, 10);
  return (
    <div style={{ marginTop: 16 }}>
      <button
        onClick={() => setCollapsed((c) => !c)}
        style={{
          width: "100%",
          textAlign: "left",
          background: "transparent",
          border: `1px solid ${tc.border}`,
          borderRadius: 6,
          padding: "8px 10px",
          color: tc.text,
          fontSize: 12,
          fontWeight: 600,
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <span>Query recenti ({queries.length})</span>
        <span style={{ color: tc.textMuted, fontSize: 11 }}>{collapsed ? "▸" : "▾"}</span>
      </button>
      {!collapsed && (
        <div style={{ marginTop: 6, display: "flex", flexDirection: "column", gap: 4 }}>
          {recent.map((q, i) => (
            <div
              key={i}
              style={{
                fontSize: 11,
                color: tc.textMuted,
                padding: "4px 8px",
                borderLeft: `2px solid ${q.kind === "select" ? "#22c55e" : "#f59e0b"}`,
                background: tc.bgCard,
                display: "flex",
                gap: 8,
                alignItems: "center",
              }}
            >
              <span
                style={{
                  textTransform: "uppercase",
                  fontWeight: 600,
                  color: q.kind === "select" ? "#22c55e" : "#f59e0b",
                  minWidth: 50,
                }}
              >
                {q.kind}
              </span>
              <span style={{ flex: 1 }}>{q.rows} rows</span>
              <span>{q.duration_ms}ms</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
