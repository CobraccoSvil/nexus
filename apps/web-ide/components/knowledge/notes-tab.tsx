"use client";

import { useState, useEffect, useCallback } from "react";
import { useI18n, type TranslationKey } from "../../lib/i18n";
import {
  listKnowledgeNotes,
  createKnowledgeNoteManual,
  initOrRefreshKnowledge,
  deleteKnowledgeNote,
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
  const { confirmDialog, alertDialog } = useGlobalDialog();
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
  const [refreshing, setRefreshing] = useState(false);
  const [refreshMsg, setRefreshMsg] = useState<string | null>(null);
  // ID della nota in corso di cancellazione (per disabilitare il bottone +
  // dare feedback visivo). null = nessuna cancellazione in atto.
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const limit = 20;

  // Cancella una nota dalla KB. Conferma con dialog modale Nexus (via
  // GlobalDialogProvider): l'utente puo' annullare. Dopo il successo,
  // refresh della lista e (se la nota era selezionata) chiusura del dettaglio.
  const handleDeleteNote = useCallback(
    async (noteId: string, noteTitle: string) => {
      if (deletingId) return;
      const ok = await confirmDialog({
        message:
          `Vuoi davvero eliminare la nota "${noteTitle}"?\n\n` +
          "L'operazione e' irreversibile: rimuove la nota dal DB, " +
          "i link in/out e l'embedding Qdrant.",
        title: "Elimina nota",
        danger: true,
        confirmLabel: "Elimina",
      });
      if (!ok) return;
      setDeletingId(noteId);
      try {
        await deleteKnowledgeNote(projectId, noteId);
        // Rimuovi subito dallo stato locale (ottimistico).
        setNotes((curr) => curr.filter((n) => n.id !== noteId));
        setTotal((t) => Math.max(0, t - 1));
        if (selectedNoteId === noteId) setSelectedNoteId(null);
      } catch (err) {
        await alertDialog(
          err instanceof Error ? err.message : String(err),
          "Cancellazione fallita",
        );
      } finally {
        setDeletingId(null);
      }
    },
    [deletingId, projectId, selectedNoteId, confirmDialog, alertDialog],
  );

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

  // Inizializza o aggiorna l'intera KB in un solo flusso (resiliente).
  // Esegue: (1) FunctionalSpecAgent (chat + file .md/sorgenti chiave),
  // (2) 3 generator tech/functional/test, (3) rebuild idempotente da chat
  // user, (4) ricalcolo link automatici. `reset` cancella prima le note auto.
  const handleInitOrRefresh = async (reset: boolean) => {
    const confirmMsg = reset
      ? "RESET KB: cancellare TUTTE le note auto-generate (chat/technical/functional/test) e ricostruire la KB completa? Le note curate manualmente (kind diverso, source_message_id NULL) NON saranno toccate. L'agente LLM rianalizzera' file .md, sorgenti chiave e chat user."
      : "Inizializzare/aggiornare la KB del progetto? L'agente LLM analizzera' i file .md, i sorgenti chiave (routes, handlers, models, schema, migrations), i messaggi chat user, e generera' note tech/functional/test arricchite. Le note esistenti vengono aggiornate, non duplicate. Puo' richiedere diversi minuti.";
    const title = reset ? "Reset Knowledge Base" : "Inizializza / Aggiorna KB";
    const ok = await confirmDialog(confirmMsg, title);
    if (!ok) return;
    setRefreshing(true);
    setRefreshMsg(null);
    try {
      const r = await initOrRefreshKnowledge(projectId, {
        reset,
        chat_limit: 100,
        files_limit: 100,
      });
      const f = r.functional_agent;
      const g = r.generators;
      const rb = r.rebuild_from_chat;
      const l = r.links;
      const warn = r.warnings?.length
        ? ` Warning: ${r.warnings.join("; ")}`
        : "";
      setRefreshMsg(
        `KB aggiornata.${reset ? ` Reset: ${r.deleted_notes} note rimosse.` : ""}` +
          ` Agente funzionale: ${f.specs_applied ?? 0}/${f.specs_extracted ?? 0} spec applicate (file ${f.files_with_specs ?? 0}/${f.files_scanned ?? 0}, chat ${f.messages_with_specs ?? 0}/${f.messages_scanned ?? 0}).` +
          ` Generator: ${g.notes_applied ?? 0}/${g.notes_generated ?? 0} note.` +
          ` Rebuild chat: ${rb.notes_created ?? 0}/${rb.messages_total ?? 0}.` +
          ` Link: ${l.links_created ?? 0} (${l.notes_processed ?? 0} note).` +
          warn,
      );
      await load();
    } catch (e) {
      setRefreshMsg("Errore: " + (e instanceof Error ? e.message : String(e)));
    } finally {
      setRefreshing(false);
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
      {/* Azione KB unificata: un solo bottone primario fa tutto
          (FunctionalSpecAgent + generators + rebuild chat + link).
          Reset e' azione separata distruttiva. */}
      <div style={{ display: "flex", gap: 4, marginBottom: 6 }}>
        <button
          onClick={() => handleInitOrRefresh(false)}
          disabled={refreshing}
          title="Inizializza o aggiorna la KB del progetto: scansiona file .md + sorgenti chiave, chat user, genera note tech/functional/test, ricalcola link. Idempotente."
          style={{
            flex: 1,
            minWidth: 0,
            padding: "8px 12px",
            fontSize: 12,
            fontWeight: 600,
            background: refreshing ? "#a3a3a3" : "#0ea5e9",
            color: "#fff",
            border: "none",
            borderRadius: 6,
            cursor: refreshing ? "default" : "pointer",
          }}
        >
          {refreshing ? "Aggiornamento in corso..." : "Inizializza / Aggiorna KB"}
        </button>
        <button
          onClick={() => handleInitOrRefresh(true)}
          disabled={refreshing}
          title="ATTENZIONE: cancella tutte le note auto-generate (chat/technical/functional/test) e ricostruisce la KB da zero"
          style={{
            flexShrink: 0,
            padding: "8px 10px",
            fontSize: 11,
            fontWeight: 600,
            background: "transparent",
            color: "#dc2626",
            border: "1px solid #dc2626",
            borderRadius: 6,
            cursor: refreshing ? "default" : "pointer",
          }}
        >
          Reset
        </button>
      </div>

      {refreshMsg && (
        <div
          style={{
            fontSize: 11,
            color: refreshMsg.startsWith("Errore") ? "#dc2626" : "#16a34a",
            marginBottom: 8,
            padding: 6,
            background: refreshMsg.startsWith("Errore") ? "#fef2f2" : "#f0fdf4",
            borderRadius: 4,
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
          }}
        >
          {refreshMsg}
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
            {/* Bottone Cancella nota: stopPropagation per non aprire il dettaglio */}
            <button
              type="button"
              aria-label={`Cancella nota "${note.title}"`}
              title="Cancella nota"
              onClick={(e) => {
                e.stopPropagation();
                void handleDeleteNote(note.id, note.title);
              }}
              disabled={deletingId === note.id}
              style={{
                fontSize: 14,
                lineHeight: 1,
                padding: "2px 6px",
                borderRadius: 4,
                border: "1px solid transparent",
                background: "transparent",
                color: "#a3a3a3",
                cursor: deletingId === note.id ? "wait" : "pointer",
                flexShrink: 0,
                opacity: deletingId === note.id ? 0.5 : 1,
              }}
              onMouseEnter={(e) => {
                if (deletingId !== note.id) {
                  e.currentTarget.style.color = "#dc2626";
                  e.currentTarget.style.borderColor = "#fecaca";
                }
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.color = "#a3a3a3";
                e.currentTarget.style.borderColor = "transparent";
              }}
            >
              {deletingId === note.id ? "..." : "×"}
            </button>
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
