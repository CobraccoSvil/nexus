"use client";

import { useState, useEffect } from "react";
import { useI18n, type TranslationKey } from "../../lib/i18n";
import { getKnowledgeNote, type KnowledgeNote } from "../../lib/api-client";

const STATUS_I18N: Record<string, TranslationKey> = {
  draft: "knowledge.note.draft",
  active: "knowledge.note.active",
  archived: "knowledge.note.archived",
  deprecated: "knowledge.note.deprecated",
};

interface Props {
  projectId: string;
  noteId: string;
  onBack: () => void;
}

export function NoteDetail({ projectId, noteId, onBack }: Props) {
  const { t } = useI18n();
  const [note, setNote] = useState<KnowledgeNote | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const n = await getKnowledgeNote(projectId, noteId);
        if (!cancelled) setNote(n);
      } catch (e: unknown) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Errore");
      }
    })();
    return () => { cancelled = true; };
  }, [projectId, noteId]);

  if (error) {
    return (
      <div style={{ padding: 16 }}>
        <button onClick={onBack} style={{ fontSize: 12, color: "#6366f1", background: "none", border: "none", cursor: "pointer", marginBottom: 8 }}>&larr; Indietro</button>
        <p style={{ color: "#ef4444", fontSize: 13 }}>{error}</p>
      </div>
    );
  }

  if (!note) {
    return <div style={{ padding: 16, fontSize: 12, color: "#a3a3a3" }}>...</div>;
  }

  return (
    <div style={{ padding: 16 }}>
      <button onClick={onBack} style={{ fontSize: 12, color: "#6366f1", background: "none", border: "none", cursor: "pointer", marginBottom: 12 }}>&larr; Indietro</button>

      <h4 style={{ fontSize: 15, fontWeight: 700, color: "#171717", margin: "0 0 8px" }}>{note.title}</h4>

      {/* Meta */}
      <div style={{ display: "flex", gap: 12, flexWrap: "wrap", marginBottom: 12, fontSize: 11, color: "#737373" }}>
        {note.intent && (
          <span><strong>{t("knowledge.detail.intent")}:</strong> {note.intent}</span>
        )}
        <span>
          <strong>{t("knowledge.detail.created")}:</strong>{" "}
          {new Date(note.createdAt).toLocaleString()}
        </span>
        <span>
          <strong>{t("knowledge.detail.updated")}:</strong>{" "}
          {new Date(note.updatedAt).toLocaleString()}
        </span>
        <span
          style={{
            padding: "1px 6px",
            borderRadius: 8,
            background: note.status === "active" ? "#dcfce7" : note.status === "draft" ? "#fef9c3" : "#f5f5f5",
            color: note.status === "active" ? "#166534" : note.status === "draft" ? "#854d0e" : "#737373",
          }}
        >
          {t(STATUS_I18N[note.status] ?? "knowledge.note.draft")}
        </span>
      </div>

      {/* Tag */}
      {note.tags.length > 0 && (
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginBottom: 12 }}>
          {note.tags.map((tag) => (
            <span key={tag} style={{ fontSize: 11, padding: "2px 8px", borderRadius: 10, background: "#f0f0ff", color: "#4338ca" }}>
              #{tag}
            </span>
          ))}
        </div>
      )}

      {/* File coinvolti */}
      {note.filePaths.length > 0 && (
        <div style={{ marginBottom: 12 }}>
          <span style={{ fontSize: 11, fontWeight: 600, color: "#525252" }}>{t("knowledge.detail.files")}:</span>
          <div style={{ marginTop: 4 }}>
            {note.filePaths.map((fp) => (
              <code key={fp} style={{ display: "block", fontSize: 11, color: "#6366f1", padding: "2px 0" }}>{fp}</code>
            ))}
          </div>
        </div>
      )}

      {/* Corpo */}
      <div
        style={{
          padding: 12,
          background: "#fafafa",
          borderRadius: 8,
          border: "1px solid #e5e5e5",
          fontSize: 13,
          lineHeight: 1.6,
          color: "#171717",
          whiteSpace: "pre-wrap",
          marginBottom: 16,
        }}
      >
        {note.bodyMd}
      </div>

      {/* Link in uscita */}
      {note.outgoing && note.outgoing.length > 0 && (
        <div style={{ marginBottom: 12 }}>
          <h5 style={{ fontSize: 12, fontWeight: 600, color: "#525252", margin: "0 0 6px" }}>
            {t("knowledge.links.outgoing")}
          </h5>
          {note.outgoing.map((link) => (
            <div key={link.linkId} style={{ fontSize: 12, padding: "4px 0", color: "#171717" }}>
              <span style={{ color: "#6366f1" }}>{link.toTitle}</span>
              <span style={{ color: "#a3a3a3", marginLeft: 8 }}>
                ({link.relType}, {(link.confidence * 100).toFixed(0)}%)
              </span>
            </div>
          ))}
        </div>
      )}

      {/* Backlink */}
      {note.backlinks && note.backlinks.length > 0 && (
        <div style={{ marginBottom: 12 }}>
          <h5 style={{ fontSize: 12, fontWeight: 600, color: "#525252", margin: "0 0 6px" }}>
            {t("knowledge.links.backlinks")}
          </h5>
          {note.backlinks.map((link) => (
            <div key={link.linkId} style={{ fontSize: 12, padding: "4px 0", color: "#171717" }}>
              <span style={{ color: "#6366f1" }}>{link.fromTitle}</span>
              <span style={{ color: "#a3a3a3", marginLeft: 8 }}>
                ({link.relType}, {(link.confidence * 100).toFixed(0)}%)
              </span>
            </div>
          ))}
        </div>
      )}

      {!note.outgoing?.length && !note.backlinks?.length && (
        <p style={{ fontSize: 12, color: "#a3a3a3", fontStyle: "italic" }}>{t("knowledge.links.noLinks")}</p>
      )}
    </div>
  );
}
