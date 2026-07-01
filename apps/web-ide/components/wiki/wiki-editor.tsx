"use client";

// WikiEditor — split orizzontale "raw markdown | preview live".
// Riusa MarkdownBlock con anchor heading + wikilink cliccabili (opt-in).

import * as React from "react";
import { MarkdownBlock } from "../chat/markdown-renderer";
import { useThemeColors } from "../../lib/theme";

interface Props {
  draftTitle: string;
  setDraftTitle: (v: string) => void;
  draftTags: string;
  setDraftTags: (v: string) => void;
  draftBody: string;
  setDraftBody: (v: string) => void;
  onWikiLink: (target: string) => void;
}

export function WikiEditor({
  draftTitle,
  setDraftTitle,
  draftTags,
  setDraftTags,
  draftBody,
  setDraftBody,
  onWikiLink,
}: Props) {
  const tc = useThemeColors();
  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div
        style={{
          padding: "12px 16px",
          borderBottom: `1px solid ${tc.border}`,
          display: "flex",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <label
          style={{
            flex: 2,
            minWidth: 240,
            fontSize: 11,
            color: tc.textSecondary,
          }}
        >
          Titolo
          <input
            value={draftTitle}
            onChange={(e) => setDraftTitle(e.target.value)}
            style={{
              display: "block",
              width: "100%",
              marginTop: 3,
              padding: "5px 8px",
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              color: tc.text,
              fontSize: 14,
              fontWeight: 600,
            }}
          />
        </label>
        <label
          style={{
            flex: 1,
            minWidth: 180,
            fontSize: 11,
            color: tc.textSecondary,
          }}
        >
          Tag (separati da virgola)
          <input
            value={draftTags}
            onChange={(e) => setDraftTags(e.target.value)}
            style={{
              display: "block",
              width: "100%",
              marginTop: 3,
              padding: "5px 8px",
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              color: tc.text,
              fontSize: 12,
            }}
          />
        </label>
      </div>
      <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
        <textarea
          value={draftBody}
          onChange={(e) => setDraftBody(e.target.value)}
          spellCheck={false}
          style={{
            flex: 1,
            background: tc.bgInput,
            color: tc.text,
            border: "none",
            borderRight: `1px solid ${tc.border}`,
            padding: 14,
            fontFamily: 'var(--font-mono)',
            fontSize: 12.5,
            lineHeight: 1.6,
            resize: "none",
            outline: "none",
          }}
        />
        <div
          style={{
            flex: 1,
            overflowY: "auto",
            padding: "14px 20px",
            background: tc.bg,
          }}
        >
          <div style={{ fontSize: 11, color: tc.textSecondary, marginBottom: 6 }}>
            Anteprima live
          </div>
          <MarkdownBlock
            content={draftBody}
            skipNormalize
            enableHeadingAnchors
            slugPrefix="preview-"
            onWikiLink={onWikiLink}
          />
        </div>
      </div>
    </div>
  );
}
