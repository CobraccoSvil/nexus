"use client";
import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import type { UserProjectDetails } from "../../lib/api-client";
import { isBinaryDocPath } from "../../lib/file-kind";

interface DocumentItem {
  id: string;
  doc_type: string;
  title: string;
  version: string;
  file_path: string;
  status: string;
  created_at?: string;
  updated_at?: string;
}

interface DocumentsSidebarProps {
  project: UserProjectDetails | null;
  /** Apre il documento nell'editor di Nexus. Riceve il path relativo al progetto. */
  onOpenInEditor?: (relativePath: string) => void;
}

const DOC_TYPE_LABELS: Record<string, string> = {
  functional_analysis: "Analisi Funzionale",
  technical_analysis: "Analisi Tecnica",
  er_diagram: "Diagramma ER",
  project_management: "Gestione Progetto",
  release_notes: "Release Notes",
};

const DOC_TYPE_ICONS: Record<string, string> = {
  functional_analysis: "\u{1F4CB}",
  technical_analysis: "\u{1F527}",
  er_diagram: "\u{1F5C2}",
  project_management: "\u{1F4C5}",
  release_notes: "\u{1F4E6}",
};

const DOC_TYPES = Object.keys(DOC_TYPE_LABELS) as (keyof typeof DOC_TYPE_LABELS)[];

const STATUS_COLORS: Record<string, string> = {
  draft: "#FFA500",
  review: "#1E90FF",
  approved: "#32CD32",
  outdated: "#FF4444",
};

export function DocumentsSidebar({ project, onOpenInEditor }: DocumentsSidebarProps) {
  const tc = useThemeColors();
  // Dialog di Nexus (no window.confirm/alert nativi del browser: rompono
  // il look&feel e in alcuni embed/webview vengono soppressi).
  const { confirmDialog, alertDialog } = useGlobalDialog();
  const [documents, setDocuments] = useState<DocumentItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [generating, setGenerating] = useState<string | null>(null);
  const [showGenerate, setShowGenerate] = useState(false);

  const fetchDocuments = useCallback(async () => {
    if (!project?.id) return;
    setLoading(true);
    setError(null);
    try {
      const res = await fetch(`/api/projects/${project.id}/documents`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      setDocuments(data.documents || []);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Errore caricamento");
    } finally {
      setLoading(false);
    }
  }, [project?.id]);

  useEffect(() => {
    fetchDocuments();
  }, [fetchDocuments]);

  // Refresh su richiesta esterna: ide-shell dispatcha `nexus:documents:refresh`
  // a fine turno chat (un turno puo' aver generato un documento) e quando si
  // apre il pannello cliccando il link a un .docx nella chat. Senza questo,
  // il pannello gia' montato e visibile non rifa' la fetch e resta vuoto.
  useEffect(() => {
    const handler = () => { void fetchDocuments(); };
    window.addEventListener("nexus:documents:refresh", handler);
    return () => window.removeEventListener("nexus:documents:refresh", handler);
  }, [fetchDocuments]);

  // La generazione chiama l'endpoint REST dedicato
  // (POST /api/projects/:id/documents/generate), che NON passa per l'agente
  // conversazionale (niente "revisione" non richiesta) e avvia la generazione in
  // BACKGROUND ritornando subito 202: con modelli heavy/thinking puo' durare
  // minuti, una richiesta sincrona andrebbe in timeout di proxy (-> 500). Il
  // documento compare quando arriva l'evento SSE DocumentGenerated, che il
  // dispatcher converte in `nexus:documents:refresh` (gia' ascoltato sopra). In
  // caso di errore arriva un toast via evento Notification.
  const handleGenerate = async (docType: string) => {
    if (!project?.id) return;
    setGenerating(docType);
    setError(null);
    const label = DOC_TYPE_LABELS[docType] || docType;
    const title = `${label} - ${project.name}`;
    try {
      const res = await fetch(`/api/projects/${project.id}/documents/generate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ doc_type: docType, title }),
      });
      if (!res.ok) {
        let detail = `HTTP ${res.status}`;
        try {
          const j = await res.json();
          if (j?.error) detail = j.error;
        } catch {
          /* corpo non-JSON: tieni lo status */
        }
        throw new Error(detail);
      }
      // 202 Accepted: generazione avviata. Niente fetch immediata (il documento
      // non esiste ancora); il refresh arriva via SSE al termine.
      setShowGenerate(false);
      await alertDialog(
        "Generazione avviata. Il documento comparira' nel pannello al termine (puo' richiedere fino a qualche minuto).",
        "Generazione in corso",
      );
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : "Errore generazione documento";
      await alertDialog(msg, "Generazione documento fallita");
    } finally {
      setGenerating(null);
    }
  };

  const handleDownload = async (doc: DocumentItem) => {
    if (!project?.id) return;
    try {
      const res = await fetch(`/api/projects/${project.id}/documents/${doc.id}/download`);
      if (!res.ok) throw new Error(`Download failed: ${res.status}`);
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = doc.file_path.split("/").pop() || "document.docx";
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      await alertDialog("Errore durante il download", "Download fallito");
    }
  };

  const handleDelete = async (doc: DocumentItem) => {
    if (!project?.id) return;
    const ok = await confirmDialog({
      title: "Elimina documento",
      message: `Eliminare "${doc.title}" v${doc.version}?\n\nL'operazione non e' reversibile.`,
      danger: true,
      confirmLabel: "Elimina",
      cancelLabel: "Annulla",
    });
    if (!ok) return;
    try {
      await fetch(`/api/projects/${project.id}/documents/${doc.id}`, { method: "DELETE" });
      fetchDocuments();
    } catch {
      await alertDialog("Errore durante l'eliminazione", "Eliminazione fallita");
    }
  };

  /** Apre il documento nell'editor di Nexus.
   *  File binari (.docx) vengono scaricati. File testo (.md, .txt, .html)
   *  vengono aperti nell'editor.
   *  Calcola il path relativo al progetto a partire dal file_path assoluto
   *  registrato nel DB (es. /home/.../projects/<slug>/docs/x.md -> docs/x.md). */
  const handleOpen = (doc: DocumentItem) => {
    const fp = doc.file_path;

    // File binari (.docx, .xlsx, .pdf): scarica invece di aprire nell'editor.
    // Stessa classificazione usata da openFileInGroup (regola L: punto unico).
    if (isBinaryDocPath(fp)) {
      handleDownload(doc);
      return;
    }

    if (!onOpenInEditor) return;

    // Estrai il path relativo alla root del progetto.
    let relative = fp;

    // Caso 1: path assoluto con /projects/<slug>/...
    const projectsMarker = "/projects/";
    const idx = fp.indexOf(projectsMarker);
    if (idx >= 0) {
      const afterProjects = fp.slice(idx + projectsMarker.length);
      const firstSlash = afterProjects.indexOf("/");
      if (firstSlash >= 0) {
        relative = afterProjects.slice(firstSlash + 1);
      }
    } else if (fp.startsWith("/")) {
      // Caso 2: path assoluto generico — prendi solo il nome file.
      relative = fp.split("/").pop() ?? fp;
    }
    // Caso 3: path gia' relativo (es. "docs/file.md") — usa cosi' com'e'.

    onOpenInEditor(relative);
  };

  // Group by doc_type
  const grouped = documents.reduce<Record<string, DocumentItem[]>>((acc, doc) => {
    if (!acc[doc.doc_type]) acc[doc.doc_type] = [];
    acc[doc.doc_type].push(doc);
    return acc;
  }, {});

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      {/* Header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "10px 12px",
          borderBottom: `1px solid ${tc.border}`,
        }}
      >
        <span style={{ fontSize: 12, fontWeight: 600, color: tc.text, textTransform: "uppercase", letterSpacing: 0.5 }}>
          Documenti
        </span>
        <div style={{ display: "flex", gap: 4 }}>
          {project && (
            <button
              type="button"
              onClick={() => setShowGenerate((v) => !v)}
              title="Genera documentazione"
              style={{
                background: showGenerate ? tc.accent : "transparent",
                border: `1px solid ${showGenerate ? tc.accent : tc.border}`,
                color: showGenerate ? "#fff" : tc.textMuted,
                cursor: "pointer",
                fontSize: 12,
                borderRadius: 5,
                padding: "2px 8px",
                fontWeight: 600,
              }}
            >
              + Genera
            </button>
          )}
          <button
            type="button"
            onClick={fetchDocuments}
            title="Aggiorna"
            style={{
              background: "transparent",
              border: "none",
              color: tc.textMuted,
              cursor: "pointer",
              fontSize: 14,
            }}
          >
            {"\u21BB"}
          </button>
        </div>
      </div>

      {/* Generate panel */}
      {showGenerate && project && (
        <div
          style={{
            borderBottom: `1px solid ${tc.border}`,
            background: tc.bgCard,
            padding: "10px 12px",
          }}
        >
          <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 8, fontWeight: 600 }}>
            GENERA DOCUMENTO
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {DOC_TYPES.map((docType) => (
              <button
                key={docType}
                type="button"
                onClick={() => handleGenerate(docType)}
                disabled={!project || generating === docType}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "7px 10px",
                  borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: generating === docType ? `${tc.accent}22` : tc.bg,
                  color: generating === docType ? tc.accent : tc.text,
                  cursor: !project || generating === docType ? "not-allowed" : "pointer",
                  fontSize: 12,
                  textAlign: "left",
                  opacity: !project ? 0.5 : 1,
                  transition: "background 0.15s",
                }}
              >
                <span style={{ fontSize: 14 }}>{DOC_TYPE_ICONS[docType]}</span>
                <span style={{ flex: 1 }}>{DOC_TYPE_LABELS[docType]}</span>
                {generating === docType && (
                  <span style={{ fontSize: 10, color: tc.accent }}>...</span>
                )}
              </button>
            ))}
          </div>
          {!project && (
            <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 6 }}>
              Apri un progetto per generare la documentazione.
            </div>
          )}
        </div>
      )}

      <div style={{ flex: 1, overflow: "auto", padding: 8 }}>
        {loading && (
          <div style={{ color: tc.textMuted, fontSize: 12, padding: 10 }}>
            Caricamento...
          </div>
        )}

        {error && (
          <div style={{ color: tc.error, fontSize: 12, padding: 10 }}>
            {error}
          </div>
        )}

        {!loading && documents.length === 0 && !error && (
          <div style={{ color: tc.textMuted, fontSize: 12, padding: 10, textAlign: "center" }}>
            Nessun documento generato.
            <br />
            <span style={{ fontSize: 11 }}>
              Usa il tasto <strong>+ Genera</strong> oppure chiedi a Nexus nella chat.
            </span>
          </div>
        )}

        {Object.entries(grouped).map(([docType, docs]) => (
          <div key={docType} className="mb-12">
            <div
              style={{
                fontSize: 11,
                fontWeight: 600,
                color: tc.textMuted,
                textTransform: "uppercase",
                letterSpacing: 0.5,
                padding: "4px 4px",
                marginBottom: 4,
              }}
            >
              {DOC_TYPE_ICONS[docType] || "\u{1F4C4}"} {DOC_TYPE_LABELS[docType] || docType}
            </div>

            {docs.map((doc) => (
              <div
                key={doc.id}
                style={{
                  display: "flex",
                  flexDirection: "column",
                  gap: 4,
                  padding: "8px 8px",
                  borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: tc.bgCard,
                  marginBottom: 4,
                }}
              >
                <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
                  <span style={{ fontSize: 12, color: tc.text, fontWeight: 500 }}>
                    {doc.title}
                  </span>
                  <div style={{ display: "flex", gap: 2 }}>
                    <span
                      style={{
                        fontSize: 10,
                        padding: "1px 5px",
                        borderRadius: 3,
                        background: `${STATUS_COLORS[doc.status] || tc.textMuted}22`,
                        color: STATUS_COLORS[doc.status] || tc.textMuted,
                        fontWeight: 600,
                      }}
                    >
                      {doc.status}
                    </span>
                    <span
                      style={{
                        fontSize: 10,
                        padding: "1px 5px",
                        borderRadius: 3,
                        background: `${tc.accent}22`,
                        color: tc.accent,
                      }}
                    >
                      v{doc.version}
                    </span>
                  </div>
                </div>

                <div style={{ display: "flex", gap: 6, marginTop: 2 }}>
                  {onOpenInEditor && (
                    <button
                      type="button"
                      onClick={() => handleOpen(doc)}
                      title="Apri nell'editor"
                      style={{
                        fontSize: 11,
                        background: tc.accent,
                        color: "#fff",
                        border: "none",
                        borderRadius: 4,
                        padding: "3px 8px",
                        cursor: "pointer",
                      }}
                    >
                      Apri
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => handleDownload(doc)}
                    title="Download .docx"
                    style={{
                      fontSize: 11,
                      background: tc.bgCard,
                      color: tc.text,
                      border: `1px solid ${tc.border}`,
                      borderRadius: 4,
                      padding: "3px 8px",
                      cursor: "pointer",
                    }}
                  >
                    Download
                  </button>
                  <button
                    type="button"
                    onClick={() => handleGenerate(doc.doc_type)}
                    disabled={!project || generating === doc.doc_type}
                    title="Rigenera"
                    style={{
                      fontSize: 11,
                      background: "transparent",
                      color: tc.accent,
                      border: `1px solid ${tc.accent}44`,
                      borderRadius: 4,
                      padding: "3px 8px",
                      cursor: project ? "pointer" : "not-allowed",
                      opacity: generating === doc.doc_type ? 0.5 : 1,
                    }}
                  >
                    {generating === doc.doc_type ? "..." : "Rigenera"}
                  </button>
                  <button
                    type="button"
                    onClick={() => handleDelete(doc)}
                    title="Elimina"
                    style={{
                      fontSize: 11,
                      background: "transparent",
                      color: tc.error,
                      border: `1px solid ${tc.error}44`,
                      borderRadius: 4,
                      padding: "3px 8px",
                      cursor: "pointer",
                    }}
                  >
                    Elimina
                  </button>
                </div>
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
