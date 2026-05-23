"use client";

import { useState, useEffect, useCallback } from "react";
import { useI18n, type TranslationKey } from "../../lib/i18n";
import {
  listKnowledgeNotes,
  createKnowledgeNoteManual,
  rebuildKnowledge,
  extractFunctionalSpecs,
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
import { useGlobalDialog } from "../global-dialog-provider";

interface Props {
  projectId: string;
}

const STATUS_OPTIONS = ["", "active", "draft", "archived", "deprecated"];

const INTENT_OPTIONS = [
  { value: "feature", label: "Feature" },
  { value: "requirement", label: "Requirement" },
  { value: "decision", label: "Decisione" },
  { value: "domain", label: "Dominio" },
  { value: "user_story", label: "User story" },
  { value: "architecture", label: "Architettura" },
  { value: "fix", label: "Fix" },
  { value: "refactor", label: "Refactor" },
  { value: "docs", label: "Doc" },
  { value: "other", label: "Altro" },
];

export function NotesTab({ projectId }: Props) {
  const { t } = useI18n();
  const { confirmDialog } = useGlobalDialog();
  const knowledgeChanged = useProjectStore(selectKnowledgeChangedAt);
  const [notes, setNotes] = useState<KnowledgeNote[]>([]);
  const [total, setTotal] = useState(0);
  const [offset, setOffset] = useState(0);
  const [statusFilter, setStatusFilter] = useState("");
  const [selectedNoteId, setSelectedNoteId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [newTitle, setNewTitle] = useState("");
  const [newBody, setNewBody] = useState("");
  const [newIntent, setNewIntent] = useState("feature");
  const [newTags, setNewTags] = useState("");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState(false);
  const [rebuildMsg, setRebuildMsg] = useState<string | null>(null);
  const [extractingFunctional, setExtractingFunctional] = useState(false);
  const [extractFunctionalMsg, setExtractFunctionalMsg] = useState<string | null>(null);
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

  const handleExtractFunctional = async () => {
    const ok = await confirmDialog(
      "Avviare l'agente di estrazione specifiche funzionali? Verranno scansionati: (1) i file .md e i sorgenti chiave del repository (route, handler, model, schema, migrations); (2) i messaggi user della chat. Per ogni feature/requirement/decision/user_story rilevata verra' creata o aggiornata una nota kind='functional'. Puo' richiedere diversi minuti.",
      "Estrai specifiche funzionali",
    );
    if (!ok) return;
    setExtractingFunctional(true);
    setExtractFunctionalMsg(null);
    try {
      const r = await extractFunctionalSpecs(projectId, {
        limit: 100,
        include_files: true,
        files_limit: 100,
      });
      setExtractFunctionalMsg(
        `File: ${r.files_with_specs}/${r.files_scanned} con spec. Chat: ${r.messages_with_specs}/${r.messages_scanned} con spec. Note funzionali applicate: ${r.specs_applied}/${r.specs_extracted} (${r.links_created} link).`,
      );
      await load();
    } catch (e) {
      setExtractFunctionalMsg(
        "Errore: " + (e instanceof Error ? e.message : String(e)),
      );
    } finally {
      setExtractingFunctional(false);
    }
  };

  const handleRebuild = async (reset: boolean) => {
    const confirmMsg = reset
      ? "Cancellare TUTTE le note auto del progetto e ricostruirle dai messaggi chat? Le note curate manualmente NON saranno toccate."
      : "Ricostruire le note KB mancanti dai messaggi chat? Le note esistenti non saranno modificate.";
    const title = reset ? "Reset Knowledge Base" : "Rigenera Knowledge Base";
    const ok = await confirmDialog(confirmMsg, title);
    if (!ok) return;
    setRebuilding(true);
    setRebuildMsg(null);
    try {
      const r = await rebuildKnowledge(projectId, { reset });
      setRebuildMsg(
        `Note create: ${r.notes_created}/${r.messages_total} (su ${r.linked_notes} note, ${r.links_created} link).`,
      );
      await load();
    } catch (e) {
      setRebuildMsg("Errore: " + (e instanceof Error ? e.message : String(e)));
    } finally {
      setRebuilding(false);
    }
  };

  const submitNewNote = async () => {
    const title = newTitle.trim();
    const body = newBody.trim();
    if (!title || !body) {
      setCreateError("Titolo e contenuto sono obbligatori");
      return;
    }
    setCreating(true);
    setCreateError(null);
    try {
      const tags = newTags
        .split(/[,\s]+/)
        .map((s) => s.trim())
        .filter(Boolean);
      await createKnowledgeNoteManual(projectId, {
        title,
        body_md: body,
        intent: newIntent,
        tags,
      });
      setNewTitle("");
      setNewBody("");
      setNewTags("");
      setNewIntent("feature");
      setCreateOpen(false);
      await load();
    } catch (e) {
      setCreateError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreating(false);
    }
  };

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
      {/* Azioni KB */}
      <div style={{ display: "flex", gap: 4, marginBottom: 6, flexWrap: "wrap" }}>
        <button
          onClick={() => handleRebuild(false)}
          disabled={rebuilding}
          title="Ricostruisci le note mancanti dai messaggi chat (idempotente)"
          style={{
            flex: 1,
            minWidth: 0,
            padding: "6px 10px",
            fontSize: 11,
            fontWeight: 600,
            background: rebuilding ? "#a3a3a3" : "#0ea5e9",
            color: "#fff",
            border: "none",
            borderRadius: 6,
            cursor: rebuilding ? "default" : "pointer",
          }}
        >
          {rebuilding ? "..." : "Rigenera KB"}
        </button>
        <button
          onClick={() => handleRebuild(true)}
          disabled={rebuilding}
          title="ATTENZIONE: cancella tutte le note auto e ricostruisce da zero"
          style={{
            flexShrink: 0,
            padding: "6px 10px",
            fontSize: 11,
            fontWeight: 600,
            background: "transparent",
            color: "#dc2626",
            border: "1px solid #dc2626",
            borderRadius: 6,
            cursor: rebuilding ? "default" : "pointer",
          }}
        >
          Reset
        </button>
      </div>

      {/* FunctionalSpecAgent — estrae feature/requirement/user_story dalla chat */}
      <div style={{ display: "flex", gap: 4, marginBottom: 6 }}>
        <button
          onClick={handleExtractFunctional}
          disabled={extractingFunctional}
          title="Avvia l'agente che analizza i messaggi chat e crea note funzionali (feature, requirement, decision, user_story, domain)"
          style={{
            flex: 1,
            minWidth: 0,
            padding: "6px 10px",
            fontSize: 11,
            fontWeight: 600,
            background: extractingFunctional ? "#a3a3a3" : "#7c3aed",
            color: "#fff",
            border: "none",
            borderRadius: 6,
            cursor: extractingFunctional ? "default" : "pointer",
          }}
        >
          {extractingFunctional ? "Estrazione in corso..." : "Estrai spec funzionali (LLM)"}
        </button>
      </div>

      {rebuildMsg && (
        <div
          style={{
            fontSize: 11,
            color: rebuildMsg.startsWith("Errore") ? "#dc2626" : "#16a34a",
            marginBottom: 8,
            padding: 6,
            background: rebuildMsg.startsWith("Errore") ? "#fef2f2" : "#f0fdf4",
            borderRadius: 4,
          }}
        >
          {rebuildMsg}
        </div>
      )}

      {extractFunctionalMsg && (
        <div
          style={{
            fontSize: 11,
            color: extractFunctionalMsg.startsWith("Errore") ? "#dc2626" : "#7c3aed",
            marginBottom: 8,
            padding: 6,
            background: extractFunctionalMsg.startsWith("Errore") ? "#fef2f2" : "#f5f3ff",
            borderRadius: 4,
          }}
        >
          {extractFunctionalMsg}
        </div>
      )}

      {/* Pulsante "Nuova nota funzionale" */}
      <button
        onClick={() => setCreateOpen((v) => !v)}
        style={{
          width: "100%",
          padding: "8px 12px",
          fontSize: 12,
          fontWeight: 600,
          background: createOpen ? "#525252" : "#16a34a",
          color: "#fff",
          border: "none",
          borderRadius: 6,
          cursor: "pointer",
          marginBottom: 10,
        }}
      >
        {createOpen ? "Annulla" : "+ Nuova nota funzionale"}
      </button>

      {createOpen && (
        <div
          style={{
            padding: 10,
            border: "1px solid #d4d4d4",
            borderRadius: 8,
            background: "#fafafa",
            marginBottom: 12,
          }}
        >
          <input
            value={newTitle}
            onChange={(e) => setNewTitle(e.target.value)}
            placeholder="Titolo (es. Login con Google)"
            style={{
              width: "100%",
              padding: "5px 8px",
              fontSize: 12,
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              outline: "none",
              boxSizing: "border-box",
              marginBottom: 6,
            }}
          />
          <select
            value={newIntent}
            onChange={(e) => setNewIntent(e.target.value)}
            style={{
              width: "100%",
              padding: "5px 8px",
              fontSize: 12,
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              outline: "none",
              boxSizing: "border-box",
              marginBottom: 6,
              background: "#fff",
            }}
          >
            {INTENT_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
          <textarea
            value={newBody}
            onChange={(e) => setNewBody(e.target.value)}
            placeholder="Contenuto Markdown..."
            rows={6}
            style={{
              width: "100%",
              padding: "5px 8px",
              fontSize: 12,
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              outline: "none",
              boxSizing: "border-box",
              fontFamily: "Menlo, monospace",
              resize: "vertical",
              marginBottom: 6,
            }}
          />
          <input
            value={newTags}
            onChange={(e) => setNewTags(e.target.value)}
            placeholder="Tag separati da virgola (opzionale)"
            style={{
              width: "100%",
              padding: "5px 8px",
              fontSize: 12,
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              outline: "none",
              boxSizing: "border-box",
              marginBottom: 6,
            }}
          />
          {createError && (
            <div style={{ fontSize: 11, color: "#dc2626", marginBottom: 6 }}>{createError}</div>
          )}
          <button
            onClick={submitNewNote}
            disabled={creating}
            style={{
              width: "100%",
              padding: "6px 12px",
              fontSize: 12,
              fontWeight: 600,
              background: creating ? "#a3a3a3" : "#171717",
              color: "#fff",
              border: "none",
              borderRadius: 6,
              cursor: creating ? "default" : "pointer",
            }}
          >
            {creating ? "Salvataggio..." : "Salva nota"}
          </button>
        </div>
      )}

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
