"use client";

import { useState, useCallback } from "react";
import { useI18n } from "../../lib/i18n";
import { listKnowledgeNotes, findSimilarKnowledge, type KnowledgeNote, type SimilarHit } from "../../lib/api-client";

interface Props {
  projectId: string;
}

export function SearchTab({ projectId }: Props) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<"fulltext" | "semantic">("fulltext");
  const [results, setResults] = useState<KnowledgeNote[]>([]);
  const [semanticResults, setSemanticResults] = useState<SimilarHit[]>([]);
  const [searching, setSearching] = useState(false);

  const search = useCallback(async () => {
    if (!query.trim()) return;
    setSearching(true);
    try {
      if (mode === "fulltext") {
        const res = await listKnowledgeNotes(projectId, { q: query.trim(), limit: 30 });
        setResults(res.notes);
        setSemanticResults([]);
      } else {
        const res = await findSimilarKnowledge(projectId, query.trim());
        setSemanticResults(res.hits);
        setResults([]);
      }
    } catch {
      // silenzioso
    } finally {
      setSearching(false);
    }
  }, [projectId, query, mode]);

  return (
    <div style={{ padding: 12 }}>
      <div style={{ display: "flex", gap: 6, marginBottom: 8 }}>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && search()}
          placeholder={t("knowledge.search.placeholder")}
          style={{
            flex: 1,
            padding: "6px 10px",
            fontSize: 13,
            border: "1px solid #d4d4d4",
            borderRadius: 6,
            outline: "none",
          }}
        />
        <button
          onClick={search}
          disabled={searching || !query.trim()}
          style={{
            padding: "6px 14px",
            fontSize: 12,
            fontWeight: 600,
            background: "#171717",
            color: "#fff",
            border: "none",
            borderRadius: 6,
            cursor: searching ? "default" : "pointer",
            opacity: searching ? 0.5 : 1,
          }}
        >
          {searching ? "..." : t("knowledge.tab.search")}
        </button>
      </div>

      <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
        <label style={{ fontSize: 11, color: "#737373", display: "flex", alignItems: "center", gap: 4, cursor: "pointer" }}>
          <input type="radio" name="searchMode" checked={mode === "fulltext"} onChange={() => setMode("fulltext")} />
          {t("knowledge.search.fulltext")}
        </label>
        <label style={{ fontSize: 11, color: "#737373", display: "flex", alignItems: "center", gap: 4, cursor: "pointer" }}>
          <input type="radio" name="searchMode" checked={mode === "semantic"} onChange={() => setMode("semantic")} />
          {t("knowledge.search.semantic")}
        </label>
      </div>

      {/* Risultati full-text */}
      {results.map((note) => (
        <div key={note.id} style={{ padding: "8px 10px", marginBottom: 4, borderRadius: 6, border: "1px solid #e5e5e5", fontSize: 13, overflow: "hidden" }}>
          <div style={{ display: "flex", gap: 8, alignItems: "center", minWidth: 0 }}>
            <span style={{ fontWeight: 600, color: "#171717", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: 1, minWidth: 0 }} title={note.title}>{note.title}</span>
            {note.intent && <span style={{ fontSize: 11, color: "#6366f1", flexShrink: 0 }}>{note.intent}</span>}
          </div>
        </div>
      ))}

      {/* Risultati semantici */}
      {semanticResults.map((hit) => (
        <div key={hit.noteId} style={{ padding: "8px 10px", marginBottom: 4, borderRadius: 6, border: "1px solid #e5e5e5", fontSize: 13, overflow: "hidden" }}>
          <div style={{ display: "flex", justifyContent: "space-between", gap: 8, minWidth: 0 }}>
            <span style={{ fontWeight: 600, color: "#171717", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: 1, minWidth: 0 }} title={hit.title}>{hit.title}</span>
            <span style={{ fontSize: 11, color: "#22c55e", fontWeight: 600, flexShrink: 0 }}>{(hit.score * 100).toFixed(0)}%</span>
          </div>
          {hit.intent && <span style={{ fontSize: 11, color: "#6366f1" }}>{hit.intent}</span>}
        </div>
      ))}
    </div>
  );
}
