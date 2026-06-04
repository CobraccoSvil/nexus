"use client";

// WikiTree — albero gerarchico dei documenti, costruito dai vault_file_path.
// Cartelle collassabili, file selezionabili. Componente "presentational" puro:
// non sa nulla di scope, riceve l'albero gia' costruito e callback di selezione.

import * as React from "react";
import { useThemeColors } from "../../lib/theme";
import type { WikiTreeNode } from "./wiki-scope";

interface Props {
  node: WikiTreeNode;
  depth: number;
  selectedId: string | null;
  onSelect: (id: string) => void;
  /** Solo per il nodo radice: espandi i figli al primo livello. */
  defaultOpen?: boolean;
}

export function WikiTreeNodeView({
  node,
  depth,
  selectedId,
  onSelect,
  defaultOpen,
}: Props) {
  const tc = useThemeColors();
  const [open, setOpen] = React.useState(!!defaultOpen);

  // Radice: rende solo i figli senza header.
  if (depth === 0) {
    return (
      <>
        {node.children.map((c) => (
          <WikiTreeNodeView
            key={c.path}
            node={c}
            depth={depth + 1}
            selectedId={selectedId}
            onSelect={onSelect}
            defaultOpen={depth < 1}
          />
        ))}
      </>
    );
  }

  const isDir = !node.doc && node.children.length > 0;
  if (isDir) {
    return (
      <>
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          style={{
            display: "block",
            width: "100%",
            textAlign: "left",
            background: "none",
            border: "none",
            color: tc.text,
            cursor: "pointer",
            padding: "3px 0",
            paddingLeft: 10 + depth * 12,
            fontSize: 12.5,
            fontWeight: 600,
          }}
        >
          <span style={{ display: "inline-block", width: 12 }}>
            {open ? "▾" : "▸"}
          </span>
          {node.name}
        </button>
        {open &&
          node.children.map((c) => (
            <WikiTreeNodeView
              key={c.path}
              node={c}
              depth={depth + 1}
              selectedId={selectedId}
              onSelect={onSelect}
            />
          ))}
      </>
    );
  }

  // Foglia: documento selezionabile.
  const isSel = node.doc?.id === selectedId;
  return (
    <button
      type="button"
      onClick={() => node.doc && onSelect(node.doc.id)}
      title={node.doc?.title}
      style={{
        display: "block",
        width: "100%",
        textAlign: "left",
        background: isSel ? tc.accent + "30" : "none",
        border: "none",
        borderLeft: isSel ? `2px solid ${tc.accent}` : "2px solid transparent",
        color: tc.text,
        cursor: "pointer",
        padding: "3px 0",
        paddingLeft: 10 + depth * 12,
        fontSize: 12.5,
        whiteSpace: "nowrap",
        overflow: "hidden",
        textOverflow: "ellipsis",
      }}
    >
      <span style={{ opacity: 0.7, marginRight: 4 }}>·</span>
      {node.doc?.title ?? node.name}
    </button>
  );
}
