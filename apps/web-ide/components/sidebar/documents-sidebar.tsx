"use client";
import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import type { UserProjectDetails } from "../../lib/api-client";

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
  onSendToChat?: (msg: string, options?: { providerHint?: string; modelHint?: string }) => void;
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

// Blocco di vincoli operativi prepended a ogni prompt: forza l'uso del tool
// nexus_doc_generate (che registra il documento nel DB project_documents)
// invece di write_file (che scrive solo sul filesystem e bypassa il catalogo).
//
// Audit 27/05/2026: refactor da prompt "esplorativo" a "first-action".
// Prima i prompt istruivano l'agente a "esplorare TUTTO il codebase" prima di
// chiamare il tool, il che mandava i modelli piu' deboli (mistral-small,
// devstral) in loop di read_file fino a saturare il context (visti 8M token,
// 6513% ctx) senza mai concludere. Il backend `handle_doc_generate` ha gia'
// una logica di auto-generazione del content_json quando viene chiamato senza:
// legge README/package.json/Cargo.toml + struttura directory e chiede al
// brain (purpose=docs_generator, gpt-4.1-nano) di produrre il JSON
// strutturato. Quindi la cosa giusta da fare per l'agente di chat e' chiamare
// il tool IMMEDIATAMENTE col solo title, senza esplorare.
const TOOL_CONSTRAINTS = `ISTRUZIONE PRIORITARIA (FIRST ACTION):
La PRIMA azione che devi fare e' chiamare IMMEDIATAMENTE il tool nexus_doc_generate, senza esplorazioni preliminari. Il backend genera automaticamente il content_json analizzando il progetto (README, package.json, Cargo.toml, struttura directory) tramite il purpose model 'docs_generator'. Non leggere file, non listare directory, non eseguire git log: chiama solo il tool.

Chiamata richiesta (UNA SOLA VOLTA, come prima azione):
  nexus_doc_generate({ doc_type: "<tipo>", title: "<titolo descrittivo>" })

Parametri:
- doc_type (obbligatorio): una di {functional_analysis, technical_analysis, er_diagram, project_management, release_notes}
- title (obbligatorio): stringa breve, es. "Analisi Tecnica - Demo WSL"
- content_json (opzionale): se omesso, il backend lo genera automaticamente. Includilo SOLO se hai gia' raccolto contesto reale dal progetto e vuoi scriverlo a mano.

VIETATI: write_file, nexus_write_file, edit_file (bypassano il catalogo DB).
Se non chiami nexus_doc_generate alla prima azione, il documento NON apparira' nel pannello DOCUMENTI.

`;

// I prompt per ciascun doc_type sono MINIMAL: solo titolo e doc_type.
// Il backend `handle_doc_generate` genera automaticamente un content_json
// professionale tramite il purpose model `docs_generator` (gpt-4.1-nano)
// se content_json e' omesso. Non duplichiamo "esplora il codebase" qui:
// e' un anti-pattern che mandava in loop infinito i modelli (Mistral, DeepSeek
// e perfino Sonnet) prima che chiamassero il tool. Vedi commento al
// TOOL_CONSTRAINTS sopra per il razionale completo.
const GENERATE_PROMPTS_RAW: Record<string, string> = {
  functional_analysis: `Tipo: Analisi Funzionale (standard IEEE 830 / ISO 29148).

Chiama subito: nexus_doc_generate({ doc_type: "functional_analysis", title: "Analisi Funzionale - <nome progetto>" }).
Il backend genera il contenuto. Non esplorare file ne' costruire content_json a mano.`,

  technical_analysis: `Tipo: Analisi Tecnica (qualita' professionale, livello consulenza enterprise).

Chiama subito: nexus_doc_generate({ doc_type: "technical_analysis", title: "Analisi Tecnica - <nome progetto>" }).
Il backend genera il contenuto. Non esplorare file ne' costruire content_json a mano.`,

  er_diagram: `Tipo: Diagramma Entity-Relationship del database.

Chiama subito: nexus_doc_generate({ doc_type: "er_diagram", title: "Diagramma ER - <nome progetto>" }).
Il backend genera il contenuto. Non esplorare file ne' costruire content_json a mano.`,

  project_management: `Tipo: Piano di Gestione Progetto professionale.

Chiama subito: nexus_doc_generate({ doc_type: "project_management", title: "Gestione Progetto - <nome progetto>" }).
Il backend genera il contenuto. Non esplorare file ne' costruire content_json a mano.`,

  release_notes: `Tipo: Release Notes del progetto.

Chiama subito: nexus_doc_generate({ doc_type: "release_notes", title: "Release Notes - <nome progetto> v<versione>" }).
Il backend genera il contenuto. Non esplorare file ne' costruire content_json a mano.`,
};

// Prefissa ogni prompt con i vincoli operativi sul tool da usare.
const GENERATE_PROMPTS: Record<string, string> = Object.fromEntries(
  Object.entries(GENERATE_PROMPTS_RAW).map(([k, v]) => [k, TOOL_CONSTRAINTS + v]),
);

export function DocumentsSidebar({ project, onSendToChat, onOpenInEditor }: DocumentsSidebarProps) {
  const tc = useThemeColors();
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

  const handleGenerate = (docType: string) => {
    if (!onSendToChat || !project) return;
    setGenerating(docType);
    // La generazione documenti richiede modelli capaci (tool use + output lungo).
    // Passiamo un hint al pannello chat per forzare un provider adeguato.
    onSendToChat(
      GENERATE_PROMPTS[docType] || `Genera documentazione di tipo ${docType} per il progetto.`,
      /* Nessun provider hint: la routing matrix (intent=docs) sceglie il
         provider migliore disponibile, con fallback automatico se in cooldown. */
    );
    // After sending, stop spinner after a moment and refresh docs
    setTimeout(() => {
      setGenerating(null);
      setShowGenerate(false);
    }, 2000);
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
      alert("Errore durante il download");
    }
  };

  const handleDelete = async (doc: DocumentItem) => {
    if (!project?.id) return;
    if (!confirm(`Eliminare "${doc.title}" v${doc.version}?`)) return;
    try {
      await fetch(`/api/projects/${project.id}/documents/${doc.id}`, { method: "DELETE" });
      fetchDocuments();
    } catch {
      alert("Errore durante l'eliminazione");
    }
  };

  /** Apre il documento nell'editor di Nexus.
   *  File binari (.docx) vengono scaricati. File testo (.md, .txt, .html)
   *  vengono aperti nell'editor.
   *  Calcola il path relativo al progetto a partire dal file_path assoluto
   *  registrato nel DB (es. /home/.../projects/<slug>/docs/x.md -> docs/x.md). */
  const handleOpen = (doc: DocumentItem) => {
    const fp = doc.file_path;
    const ext = fp.split(".").pop()?.toLowerCase() ?? "";

    // File binari (.docx, .xlsx, .pdf): scarica invece di aprire nell'editor.
    if (["docx", "xlsx", "pdf", "odt", "pptx"].includes(ext)) {
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
          {onSendToChat && (
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
      {showGenerate && onSendToChat && (
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
                    disabled={!onSendToChat || generating === doc.doc_type}
                    title="Rigenera"
                    style={{
                      fontSize: 11,
                      background: "transparent",
                      color: tc.accent,
                      border: `1px solid ${tc.accent}44`,
                      borderRadius: 4,
                      padding: "3px 8px",
                      cursor: onSendToChat ? "pointer" : "not-allowed",
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
