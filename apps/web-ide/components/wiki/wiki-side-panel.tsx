"use client";

// WikiSidePanel — colonna destra: TOC scroll-spy + backlink/outgoing.
// Componente di sola presentazione: riceve heading e i link gia' calcolati.

import * as React from "react";
import { useThemeColors } from "../../lib/theme";
import type { WikiBacklink } from "./wiki-scope";
import type { Heading } from "./markdown-wiki-extras";

interface Props {
  headings: Heading[];
  backlinks: WikiBacklink[];
  outgoing: WikiBacklink[];
  onNavigate: (id: string) => void;
}

export function WikiSidePanel({
  headings,
  backlinks,
  outgoing,
  onNavigate,
}: Props) {
  const tc = useThemeColors();
  return (
    <aside
      style={{
        width: 260,
        minWidth: 220,
        borderLeft: `1px solid ${tc.border}`,
        background: tc.bgCard,
        padding: "14px 12px",
        overflowY: "auto",
        fontSize: 12,
      }}
    >
      <div style={{ fontWeight: 700, marginBottom: 6 }}>Sommario</div>
      {headings.length === 0 && (
        <div style={{ color: tc.textSecondary, fontStyle: "italic" }}>
          Nessun titolo
        </div>
      )}
      {headings.map((h, idx) => (
        <a
          key={`${h.slug}-${idx}`}
          href={`#wiki-${h.slug}`}
          onClick={(e) => {
            e.preventDefault();
            document
              .getElementById(`wiki-${h.slug}`)
              ?.scrollIntoView({ behavior: "smooth", block: "start" });
          }}
          style={{
            display: "block",
            padding: "2px 0",
            paddingLeft: (h.level - 1) * 10,
            color: tc.text,
            textDecoration: "none",
            cursor: "pointer",
            fontSize: 12,
            opacity: h.level === 1 ? 1 : 0.85,
          }}
        >
          {h.text}
        </a>
      ))}

      <LinksSection
        label="Backlinks"
        links={backlinks}
        onNavigate={onNavigate}
      />
      <LinksSection
        label="Link in uscita"
        links={outgoing}
        onNavigate={onNavigate}
      />
    </aside>
  );
}

function LinksSection({
  label,
  links,
  onNavigate,
}: {
  label: string;
  links: WikiBacklink[];
  onNavigate: (id: string) => void;
}) {
  const tc = useThemeColors();
  return (
    <>
      <div style={{ fontWeight: 700, margin: "18px 0 6px" }}>
        {label} ({links.length})
      </div>
      {links.length === 0 && (
        <div style={{ color: tc.textSecondary, fontStyle: "italic" }}>
          Nessuno
        </div>
      )}
      {links.map((b, idx) => (
        <button
          key={idx}
          type="button"
          onClick={() => b.id && onNavigate(b.id)}
          disabled={!b.id}
          style={{
            display: "block",
            width: "100%",
            textAlign: "left",
            background: "none",
            border: "none",
            color: tc.accent,
            cursor: b.id ? "pointer" : "default",
            padding: "2px 0",
            fontSize: 12,
            textDecoration: "underline",
          }}
        >
          {b.title || "(senza titolo)"}
        </button>
      ))}
    </>
  );
}
