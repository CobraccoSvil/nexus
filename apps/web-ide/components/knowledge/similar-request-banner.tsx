"use client";

import { useThemeColors } from "../../lib/theme";
import type { SimilarHit } from "../../lib/api-client";

interface Props {
  hits: SimilarHit[];
  onProceed: () => void;
  onOpenNote: (noteId: string) => void;
  onDismiss: () => void;
}

export function SimilarRequestBanner({ hits, onProceed, onOpenNote, onDismiss }: Props) {
  const tc = useThemeColors();
  if (hits.length === 0) return null;

  return (
    <div
      style={{
        margin: "0 12px 8px",
        padding: "10px 14px",
        borderRadius: 8,
        background: tc.bgCard ?? "#fffbeb",
        border: `1px solid ${tc.warning ?? "#f59e0b"}`,
        fontSize: 13,
      }}
    >
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 6 }}>
        <strong style={{ color: tc.text ?? "#171717" }}>Note simili trovate</strong>
        <button
          onClick={onDismiss}
          style={{
            background: "none",
            border: "none",
            cursor: "pointer",
            color: tc.textMuted ?? "#737373",
            fontSize: 16,
            lineHeight: 1,
            padding: "0 2px",
          }}
          aria-label="Chiudi"
        >
          x
        </button>
      </div>
      <ul style={{ margin: "0 0 8px", paddingLeft: 18, color: tc.text ?? "#171717" }}>
        {hits.slice(0, 3).map((h) => (
          <li key={h.noteId} style={{ marginBottom: 2 }}>
            <button
              onClick={() => onOpenNote(h.noteId)}
              style={{
                background: "none",
                border: "none",
                cursor: "pointer",
                color: tc.accent ?? "#2563eb",
                textDecoration: "underline",
                padding: 0,
                fontSize: 13,
              }}
            >
              {h.title}
            </button>
            <span style={{ color: tc.textMuted ?? "#737373", marginLeft: 6, fontSize: 11 }}>
              ({Math.round(h.score * 100)}%)
            </span>
          </li>
        ))}
      </ul>
      <div style={{ display: "flex", gap: 8 }}>
        <button
          onClick={onProceed}
          style={{
            padding: "4px 12px",
            borderRadius: 6,
            border: `1px solid ${tc.border ?? "#e5e5e5"}`,
            background: tc.bgCard ?? "#fff",
            color: tc.text ?? "#171717",
            cursor: "pointer",
            fontSize: 12,
          }}
        >
          Invia comunque
        </button>
      </div>
    </div>
  );
}