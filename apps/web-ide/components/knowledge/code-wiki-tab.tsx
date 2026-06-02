"use client";

// W4 code-wiki: vista dedicata con navigazione ad albero file/modulo.
// Carica le note kind='code_doc' (tag 'kind:code_doc'), le organizza in un
// albero per directory e mostra la documentazione del file selezionato
// (riusa NoteDetail, che rende Markdown + diagrammi Mermaid).

import { useState, useEffect, useCallback } from "react";
import {
  listKnowledgeNotes,
  generateCodeWiki,
  type KnowledgeNote,
} from "../../lib/api-client";
import { NoteDetail } from "./note-detail";

interface Props {
  projectId: string;
}

interface TreeNode {
  name: string;
  fullPath: string;
  noteId?: string;
  children: Map<string, TreeNode>;
}

function buildTree(notes: { id: string; title: string }[]): TreeNode {
  const root: TreeNode = { name: "", fullPath: "", children: new Map() };
  for (const n of notes) {
    const parts = n.title.split("/").filter(Boolean);
    let cur = root;
    parts.forEach((part, i) => {
      if (!cur.children.has(part)) {
        cur.children.set(part, {
          name: part,
          fullPath: parts.slice(0, i + 1).join("/"),
          children: new Map(),
        });
      }
      cur = cur.children.get(part)!;
      if (i === parts.length - 1) cur.noteId = n.id;
    });
  }
  return root;
}

function sortedChildren(node: TreeNode): TreeNode[] {
  return [...node.children.values()].sort((a, b) => {
    const aDir = a.children.size > 0;
    const bDir = b.children.size > 0;
    if (aDir !== bDir) return aDir ? -1 : 1; // directory prima dei file
    return a.name.localeCompare(b.name);
  });
}

function TreeView({
  node,
  depth,
  expanded,
  toggle,
  onSelect,
}: {
  node: TreeNode;
  depth: number;
  expanded: Set<string>;
  toggle: (p: string) => void;
  onSelect: (id: string) => void;
}) {
  return (
    <>
      {sortedChildren(node).map((child) => {
        const isDir = child.children.size > 0;
        const isOpen = expanded.has(child.fullPath);
        return (
          <div key={child.fullPath}>
            <div
              onClick={() =>
                isDir ? toggle(child.fullPath) : child.noteId && onSelect(child.noteId)
              }
              style={{
                paddingLeft: 8 + depth * 14,
                paddingTop: 4,
                paddingBottom: 4,
                paddingRight: 8,
                cursor: "pointer",
                fontSize: 12,
                color: isDir ? "#525252" : "#0369a1",
                fontWeight: isDir ? 600 : 400,
                whiteSpace: "nowrap",
                overflow: "hidden",
                textOverflow: "ellipsis",
                fontFamily: isDir ? "inherit" : '"JetBrains Mono", "Consolas", monospace',
              }}
              title={child.fullPath}
            >
              {isDir ? (isOpen ? "▾ " : "▸ ") : ""}
              {child.name}
            </div>
            {isDir && isOpen && (
              <TreeView
                node={child}
                depth={depth + 1}
                expanded={expanded}
                toggle={toggle}
                onSelect={onSelect}
              />
            )}
          </div>
        );
      })}
    </>
  );
}

export function CodeWikiTab({ projectId }: Props) {
  const [notes, setNotes] = useState<KnowledgeNote[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedNoteId, setSelectedNoteId] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [generating, setGenerating] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await listKnowledgeNotes(projectId, { tag: "kind:code_doc", limit: 1000 });
      setNotes(r.notes || []);
      // Espande di default le directory di primo livello.
      const top = new Set<string>();
      for (const n of r.notes || []) {
        const first = n.title.split("/").filter(Boolean)[0];
        if (first) top.add(first);
      }
      setExpanded(top);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    load();
  }, [load]);

  // Navigazione codice -> doc: l'editor emette nexus:kb:open-code-doc con il
  // path del file; selezioniamo la nota code_doc corrispondente (match
  // flessibile per gestire path relativo/assoluto).
  useEffect(() => {
    const handler = async (ev: Event) => {
      const ce = ev as CustomEvent<{ filePath?: string }>;
      const fp = ce.detail?.filePath;
      if (!fp) return;
      try {
        const r = await listKnowledgeNotes(projectId, {
          tag: "kind:code_doc",
          q: fp,
          limit: 50,
        });
        const notes = r.notes || [];
        const match =
          notes.find((n) => n.title === fp) ||
          notes.find((n) => fp.endsWith(n.title) || n.title.endsWith(fp)) ||
          notes[0];
        if (match) {
          setSelectedNoteId(match.id);
          setError(null);
        } else {
          setSelectedNoteId(null);
          setError(
            `Nessuna documentazione per "${fp}". Premi "Genera / Aggiorna Code Wiki".`,
          );
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    };
    window.addEventListener("nexus:kb:open-code-doc", handler);
    return () => window.removeEventListener("nexus:kb:open-code-doc", handler);
  }, [projectId]);

  const toggle = (p: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(p)) next.delete(p);
      else next.add(p);
      return next;
    });

  const handleGenerate = async () => {
    setGenerating(true);
    setError(null);
    try {
      await generateCodeWiki(projectId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setGenerating(false);
    }
  };

  if (selectedNoteId) {
    return (
      <NoteDetail
        projectId={projectId}
        noteId={selectedNoteId}
        onBack={() => setSelectedNoteId(null)}
      />
    );
  }

  const tree = buildTree(notes.map((n) => ({ id: n.id, title: n.title })));

  return (
    <div style={{ padding: 12 }}>
      <button
        onClick={handleGenerate}
        disabled={generating}
        title="Genera/aggiorna la documentazione AI per ogni file di codice indicizzato. Language-agnostic. Gira in background; ricarica tra qualche minuto."
        style={{
          width: "100%",
          padding: "8px 12px",
          fontSize: 12,
          fontWeight: 600,
          background: "transparent",
          color: "#7c3aed",
          border: "1px solid #7c3aed",
          borderRadius: 6,
          cursor: generating ? "default" : "pointer",
          marginBottom: 10,
        }}
      >
        {generating ? "Avvio..." : "Genera / Aggiorna Code Wiki"}
      </button>

      <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 6 }}>
        <span style={{ fontSize: 11, color: "#737373" }}>
          {notes.length} file documentati
        </span>
        <button
          onClick={load}
          style={{
            fontSize: 11,
            background: "none",
            border: "none",
            color: "#0369a1",
            cursor: "pointer",
          }}
        >
          Aggiorna
        </button>
      </div>

      {error && (
        <div style={{ fontSize: 11, color: "#dc2626", marginBottom: 8 }}>{error}</div>
      )}

      {loading ? (
        <div style={{ fontSize: 12, color: "#737373" }}>Caricamento...</div>
      ) : notes.length === 0 ? (
        <div style={{ fontSize: 12, color: "#737373", lineHeight: 1.6 }}>
          Nessuna documentazione del codice ancora. Premi &quot;Genera / Aggiorna Code
          Wiki&quot; per creare una pagina per ogni file del progetto. La
          documentazione viene poi usata automaticamente da Nexus nelle chat per
          evitare ripetizioni ed errori.
        </div>
      ) : (
        <div
          style={{
            border: "1px solid #e5e5e5",
            borderRadius: 8,
            overflow: "auto",
            maxHeight: "calc(100vh - 220px)",
          }}
        >
          <TreeView
            node={tree}
            depth={0}
            expanded={expanded}
            toggle={toggle}
            onSelect={setSelectedNoteId}
          />
        </div>
      )}
    </div>
  );
}
