"use client";

import { useState, useEffect, useCallback } from "react";
import { useI18n, type TranslationKey } from "../../lib/i18n";
import {
  listKnowledgeNotes,
  type KnowledgeNote,
} from "../../lib/api-client";

const STATUS_I18N: Record<string, TranslationKey> = {
  draft: "knowledge.note.draft",
  active: "knowledge.note.active",
  archived: "knowledge.note.archived",
  deprecated: "knowledge.note.deprecated",
};
import { useProjectStore, selectKnowledgeChangedAt } from "../../lib/project-dispatcher/store";
import { NoteDetail } from "./note-detail";

interface Props {
  projectId: string;
}

const STATUS_OPTIONS = ["", "active", "draft", "archived", "deprecated"];

export function NotesTab({ projectId }: Props) {
  const { t } = useI18n();
  const knowledgeChanged = useProjectStore(selectKnowledgeChangedAt);
  const [notes, setNotes] = useState<KnowledgeNote[]>([]);
  const [total, setTotal] = useState(0);
  const [offset, setOffset] = useState(0);
  const [statusFilter, setStatusFilter] = useState("");
  const [selectedNoteId, setSelectedNoteId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const limit = 20;

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await listKnowledgeNotes(projectId, {
        status: statusFilter || undefined,
        limit,
        offset,
      });
      setNotes(res.notes);
      setTotal(res.total);
    } catch {
      // silenzioso
    } finally {
      setLoading(false);
    }
  }, [projectId, statusFilter, offset]);

  useEffect(() => { load(); }, [load, knowledgeChanged]);

  if (selectedNoteId) {
    return (
      <NoteDetail
        projectId={projectId}
        noteId={selectedNoteId}
        onBack={() => { setSelectedNoteId(null); load(); }}
      />
    );
  }

  return (
    <div style={{ padding: 12 }}>
      {/* Filtri */}
      <div style={{ display: "flex", gap: 8, marginBottom: 12, flexWrap: "wrap" }}>
        {STATUS_OPTIONS.map((s) => (
          <button
            key={s}
            onClick={() => { setStatusFilter(s); setOffset(0); }}
            style={{
              padding: "3px 10px",
              fontSize: 11,
              borderRadius: 12,
              border: statusFilter === s ? "1px solid #171717" : "1px solid #d4d4d4",
              background: statusFilter === s ? "#171717" : "#fff",
              color: statusFilter === s ? "#fff" : "#525252",
              cursor: "pointer",
            }}
          >
            {s === "" ? "Tutti" : t(STATUS_I18N[s] ?? "knowledge.note.draft")}
          </button>
        ))}
      </div>

      {loading && <p style={{ fontSize: 12, color: "#a3a3a3" }}>...</p>}

      {!loading && notes.length === 0 && (
        <p style={{ fontSize: 13, color: "#a3a3a3", textAlign: "center", marginTop: 32 }}>
          {t("knowledge.empty")}
        </p>
      )}

      {notes.map((note) => (
        <div
          key={note.id}
          onClick={() => setSelectedNoteId(note.id)}
          style={{
            padding: "10px 12px",
            marginBottom: 6,
            borderRadius: 8,
            border: "1px solid #e5e5e5",
            cursor: "pointer",
            transition: "border-color 0.15s",
            overflow: "hidden",
          }}
          onMouseEnter={(e) => (e.currentTarget.style.borderColor = "#a3a3a3")}
          onMouseLeave={(e) => (e.currentTarget.style.borderColor = "#e5e5e5")}
        >
          {/* Riga 1: titolo + badge stato */}
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8, minWidth: 0 }}>
            <span
              style={{
                fontSize: 13,
                fontWeight: 600,
                color: "#171717",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
                minWidth: 0,
                flex: 1,
              }}
              title={note.title}
            >
              {note.title}
            </span>
            <span
              style={{
                fontSize: 10,
                padding: "2px 6px",
                borderRadius: 8,
                background: note.status === "active" ? "#dcfce7" : note.status === "draft" ? "#fef9c3" : "#f5f5f5",
                color: note.status === "active" ? "#166534" : note.status === "draft" ? "#854d0e" : "#737373",
                flexShrink: 0,
                whiteSpace: "nowrap",
              }}
            >
              {t(STATUS_I18N[note.status] ?? "knowledge.note.draft")}
            </span>
          </div>
          {/* Riga 2: intent + tag + data */}
          <div style={{ display: "flex", gap: 6, marginTop: 4, alignItems: "center", minWidth: 0, overflow: "hidden" }}>
            {note.intent && (
              <span style={{ fontSize: 11, color: "#6366f1", fontWeight: 500, flexShrink: 0 }}>{note.intent}</span>
            )}
            <div style={{ display: "flex", gap: 4, overflow: "hidden", flex: 1, minWidth: 0 }}>
              {note.tags.slice(0, 2).map((tag) => (
                <span
                  key={tag}
                  style={{
                    fontSize: 10,
                    color: "#737373",
                    whiteSpace: "nowrap",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    maxWidth: 90,
                  }}
                  title={`#${tag}`}
                >
                  #{tag}
                </span>
              ))}
              {note.tags.length > 2 && (
                <span style={{ fontSize: 10, color: "#a3a3a3", flexShrink: 0 }}>+{note.tags.length - 2}</span>
              )}
            </div>
            <span style={{ fontSize: 10, color: "#a3a3a3", flexShrink: 0, whiteSpace: "nowrap" }}>
              {new Date(note.createdAt).toLocaleDateString()}
            </span>
          </div>
        </div>
      ))}

      {/* Paginazione */}
      {total > limit && (
        <div style={{ display: "flex", justifyContent: "center", gap: 8, marginTop: 12 }}>
          <button
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - limit))}
            style={{ fontSize: 12, padding: "4px 12px", cursor: offset === 0 ? "default" : "pointer", opacity: offset === 0 ? 0.4 : 1, border: "1px solid #d4d4d4", borderRadius: 6, background: "#fff" }}
          >
            &larr;
          </button>
          <span style={{ fontSize: 12, color: "#737373", lineHeight: "28px" }}>
            {offset + 1}-{Math.min(offset + limit, total)} / {total}
          </span>
          <button
            disabled={offset + limit >= total}
            onClick={() => setOffset(offset + limit)}
            style={{ fontSize: 12, padding: "4px 12px", cursor: offset + limit >= total ? "default" : "pointer", opacity: offset + limit >= total ? 0.4 : 1, border: "1px solid #d4d4d4", borderRadius: 6, background: "#fff" }}
          >
            &rarr;
          </button>
        </div>
      )}
    </div>
  );
}
