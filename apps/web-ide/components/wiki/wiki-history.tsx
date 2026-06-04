"use client";

// WikiHistory — tabella revisioni + diff line-by-line + restore.
// Diff zero-deps (line-set diff con marker +/-/contesto). LCS deliberatamente
// evitato per non aggiungere dipendenze: il rendering e' "sufficient" per
// review umana di doc Markdown.

import * as React from "react";
import { useThemeColors } from "../../lib/theme";
import type { WikiRevision } from "../../lib/api-client";

interface Props {
  revisions: WikiRevision[];
  revFrom: number | null;
  revTo: number | null;
  diff: { from: WikiRevision; to: WikiRevision } | null;
  setRevFrom: (n: number | null) => void;
  setRevTo: (n: number | null) => void;
  onCompare: () => void;
  onRestore: (v: number) => void;
}

export function WikiHistory({
  revisions,
  revFrom,
  revTo,
  diff,
  setRevFrom,
  setRevTo,
  onCompare,
  onRestore,
}: Props) {
  const tc = useThemeColors();
  return (
    <div style={{ display: "flex", gap: 16, flexWrap: "wrap" }}>
      <table
        style={{
          flex: 1,
          minWidth: 320,
          borderCollapse: "collapse",
          fontSize: 12,
        }}
      >
        <thead>
          <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
            <th style={{ textAlign: "left", padding: 6 }}>Versione</th>
            <th style={{ textAlign: "left", padding: 6 }}>Data</th>
            <th style={{ textAlign: "left", padding: 6 }}>Origine</th>
            <th style={{ textAlign: "left", padding: 6 }}>Autore</th>
            <th style={{ textAlign: "left", padding: 6 }}>From</th>
            <th style={{ textAlign: "left", padding: 6 }}>To</th>
            <th style={{ textAlign: "left", padding: 6 }}>Azione</th>
          </tr>
        </thead>
        <tbody>
          {revisions.map((r) => (
            <tr
              key={r.version_no}
              style={{ borderBottom: `1px solid ${tc.border}` }}
            >
              <td style={{ padding: 6 }}>v{r.version_no}</td>
              <td style={{ padding: 6 }}>
                {new Date(r.created_at).toLocaleString()}
              </td>
              <td style={{ padding: 6 }}>{r.source}</td>
              <td style={{ padding: 6, color: tc.textSecondary }}>
                {r.author || "—"}
              </td>
              <td style={{ padding: 6 }}>
                <input
                  type="radio"
                  name="diff-from"
                  checked={revFrom === r.version_no}
                  onChange={() => setRevFrom(r.version_no)}
                />
              </td>
              <td style={{ padding: 6 }}>
                <input
                  type="radio"
                  name="diff-to"
                  checked={revTo === r.version_no}
                  onChange={() => setRevTo(r.version_no)}
                />
              </td>
              <td style={{ padding: 6 }}>
                <button
                  type="button"
                  onClick={() => onRestore(r.version_no)}
                  style={{
                    background: "none",
                    border: `1px solid ${tc.border}`,
                    color: tc.accent,
                    cursor: "pointer",
                    fontSize: 11,
                    padding: "1px 8px",
                    borderRadius: 3,
                  }}
                >
                  Ripristina
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div style={{ flex: 2, minWidth: 320 }}>
        <button
          type="button"
          onClick={onCompare}
          disabled={revFrom == null || revTo == null}
          style={{
            background: tc.accent,
            color: "#fff",
            border: "none",
            padding: "4px 12px",
            borderRadius: 4,
            cursor: "pointer",
            marginBottom: 12,
            opacity: revFrom == null || revTo == null ? 0.5 : 1,
            fontSize: 12,
          }}
        >
          Confronta v{revFrom ?? "?"} → v{revTo ?? "?"}
        </button>
        {diff && <WikiDiffView from={diff.from} to={diff.to} />}
      </div>
    </div>
  );
}

function WikiDiffView({
  from,
  to,
}: {
  from: WikiRevision;
  to: WikiRevision;
}) {
  const tc = useThemeColors();
  const lines = React.useMemo(
    () => renderTextDiff(from.body_md ?? "", to.body_md ?? ""),
    [from.body_md, to.body_md],
  );
  return (
    <pre
      style={{
        background: tc.bgInput,
        padding: 12,
        borderRadius: 6,
        border: `1px solid ${tc.border}`,
        fontSize: 11.5,
        lineHeight: 1.5,
        fontFamily: '"JetBrains Mono", "Consolas", monospace',
        maxHeight: 480,
        overflowY: "auto",
        whiteSpace: "pre-wrap",
        color: tc.text,
      }}
    >
      {lines.map((line, i) => (
        <span
          key={i}
          style={{
            display: "block",
            color:
              line.kind === "add"
                ? tc.success
                : line.kind === "del"
                  ? tc.error
                  : undefined,
          }}
        >
          {line.kind === "add" ? "+ " : line.kind === "del" ? "- " : "  "}
          {line.text}
        </span>
      ))}
    </pre>
  );
}

type DiffLine = { kind: "add" | "del" | "ctx"; text: string };

/** Diff line-set semplice (no LCS): righe nuove sono add, rimosse sono del. */
function renderTextDiff(a: string, b: string): DiffLine[] {
  const linesA = a.split("\n");
  const linesB = b.split("\n");
  const setA = new Set(linesA);
  const setB = new Set(linesB);
  const out: DiffLine[] = [];
  for (const line of linesB) {
    out.push({ kind: setA.has(line) ? "ctx" : "add", text: line });
  }
  for (const line of linesA) {
    if (!setB.has(line)) out.push({ kind: "del", text: line });
  }
  return out;
}
