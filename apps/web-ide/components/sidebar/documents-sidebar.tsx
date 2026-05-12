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
const TOOL_CONSTRAINTS = `VINCOLI ASSOLUTI (NON NEGOZIABILI):
- VIETATO usare write_file, nexus_write_file, edit_file o qualunque tool di scrittura diretta per produrre il documento.
- DEVI chiamare il tool nexus_doc_generate UNA SOLA VOLTA al termine, con i parametri:
  - doc_type: una di {functional_analysis, technical_analysis, er_diagram, project_management, release_notes}
  - title: titolo del documento (stringa)
  - content_json: oggetto con sections (array {title, content})
- Se non chiami nexus_doc_generate il documento NON apparira' nel pannello DOCUMENTI e il lavoro andra' perso.
- Esempio chiamata corretta:
  nexus_doc_generate({ doc_type: "technical_analysis", title: "Analisi Tecnica - X", content_json: { sections: [{ title: "Executive Summary", content: "..." }, ...] } })

`;

const GENERATE_PROMPTS_RAW: Record<string, string> = {
  functional_analysis: `Genera l'Analisi Funzionale del progetto seguendo lo standard IEEE 830 / ISO 29148.

ISTRUZIONI OPERATIVE:
1. Esplora TUTTO il codebase con list_files (ricorsivo) per mappare ogni modulo, controller, servizio, modello.
2. Leggi i file chiave: entry point, configurazioni, schema DB (migrazioni SQL), API routes, modelli dati.
3. Costruisci un content_json con ALMENO 8 sezioni principali e sottosezioni:
   - 1. Introduzione (scopo, ambito, definizioni, riferimenti)
   - 2. Descrizione Generale (prospettiva prodotto, funzionalita', classi utenti, ambiente operativo, vincoli)
   - 3. Requisiti Funzionali (elenca OGNI requisito come RF-001..RF-N con priorita', input, output, precondizioni)
   - 4. Requisiti Non Funzionali (performance, sicurezza, usabilita', affidabilita')
   - 5. Interfacce Esterne (UI, API, integrazioni terze parti)
   - 6. Modello Dati (entita', relazioni, vincoli)
   - 7. Flussi Operativi (scenari principali, diagrammi di sequenza in testo)
   - 8. Matrice di Tracciabilita' (requisito -> componente -> test)
4. OGNI sezione deve contenere almeno 4-6 frasi dettagliate basate sul codice REALE analizzato, non generiche.
5. Chiama nexus_doc_generate con il content_json completo. NON rispondere in chat — genera il .docx.`,

  technical_analysis: `Genera l'Analisi Tecnica del progetto con qualita' professionale da consulenza enterprise.

ISTRUZIONI OPERATIVE:
1. Esplora TUTTO il codebase: list_files ricorsivo su ogni directory. Leggi TUTTI i file di configurazione, entry point, modelli, controller, servizi.
2. Analizza in profondita': dipendenze (package.json/Cargo.toml/.csproj), schema DB (migrazioni SQL), endpoint API (routes/controller), pattern architetturali usati.
3. Costruisci un content_json con ALMENO 10 sezioni principali:
   - 1. Executive Summary (panoramica progetto, obiettivi, stack scelto e motivazioni)
   - 2. Architettura del Sistema (pattern architetturale, diagramma componenti in testo, layer separation)
   - 3. Stack Tecnologico (linguaggio, framework, librerie principali con VERSIONI ESATTE lette dai file)
   - 4. Struttura del Codebase (albero directory commentato, responsabilita' di ogni modulo)
   - 5. Modello Dati e Database (DBMS, schema completo con tabelle/colonne/tipi/FK/indici letti dalle migrazioni)
   - 6. API Reference (OGNI endpoint: metodo HTTP, path, request body, response, autenticazione)
   - 7. Autenticazione e Sicurezza (meccanismo auth, gestione sessioni, CORS, rate limiting, input validation)
   - 8. Gestione Errori e Logging (pattern, livelli, formato log, monitoraggio)
   - 9. Build, Deploy e DevOps (comandi build, CI/CD, containerizzazione, ambienti)
   - 10. Dipendenze Esterne e Integrazioni (servizi terzi, API esterne, message broker)
   - 11. Performance e Scalabilita' (caching, connection pooling, strategie di scaling)
   - 12. Debito Tecnico e Raccomandazioni (problemi identificati, miglioramenti suggeriti)
4. OGNI sezione deve avere contenuto REALE estratto dal codice (nomi file, classi, funzioni, configurazioni specifiche). Minimo 5-8 frasi per sezione.
5. Chiama nexus_doc_generate con il content_json completo. NON rispondere solo in chat.`,

  er_diagram: `Genera il Diagramma Entity-Relationship completo del database del progetto.

ISTRUZIONI OPERATIVE:
1. Leggi TUTTE le migrazioni SQL in db/migrations/ o cartelle equivalenti. Se non ci sono migrazioni, cerca modelli ORM (Entity Framework, Sequelize, Prisma, Diesel).
2. Per ogni tabella/entita' documenta: nome, TUTTE le colonne con tipo e nullable, chiavi primarie, chiavi esterne, indici, vincoli CHECK/UNIQUE.
3. Costruisci content_json con:
   - 1. Panoramica del Modello Dati (DBMS, numero entita', strategia naming, convenzioni)
   - 2. Diagramma ER (sintassi Mermaid erDiagram con TUTTE le relazioni: 1:1, 1:N, N:M)
   - 3. Catalogo Entita' (per OGNI tabella: descrizione, tutte le colonne, tipi, vincoli)
   - 4. Relazioni e Vincoli Referenziali (FK con ON DELETE/UPDATE, vincoli compositi)
   - 5. Indici e Ottimizzazioni (indici per performance, indici parziali, indici GIN/GiST)
   - 6. Diagramma delle Dipendenze (ordine di creazione tabelle, dipendenze circolari)
4. NON inventare tabelle — documenta SOLO quelle reali trovate nel codice.
5. Chiama nexus_doc_generate con doc_type="er_diagram".`,

  project_management: `Genera il Piano di Gestione Progetto professionale.

ISTRUZIONI OPERATIVE:
1. Analizza il codebase per inferire: complessita', stato di maturita', copertura test, debito tecnico.
2. Leggi git log (se disponibile) per capire velocita' di sviluppo, contributor, aree attive.
3. Costruisci content_json con:
   - 1. Panoramica Progetto (obiettivi, stakeholder, ambito, vincoli)
   - 2. Work Breakdown Structure (WBS con attivita' raggruppate per area funzionale)
   - 3. Timeline e Milestone (milestone realistiche basate sullo stato attuale del codice)
   - 4. Gestione Rischi (rischi tecnici identificati dall'analisi del codice, matrice probabilita'/impatto, mitigazioni)
   - 5. Risorse e Competenze (profili tecnici necessari basati sullo stack)
   - 6. Strategia di Testing (unit, integration, e2e — stato attuale e raccomandazioni)
   - 7. Piano di Deploy e Rilascio (ambienti, strategia rilascio, rollback)
   - 8. Metriche e KPI (metriche di qualita' codice, velocita', coverage)
4. Basa TUTTO sull'analisi reale del codice, non su template generici.
5. Chiama nexus_doc_generate con doc_type="project_management".`,

  release_notes: `Genera le Release Notes del progetto.

ISTRUZIONI OPERATIVE:
1. Usa run_command per eseguire "git log --oneline -50" (o equivalente) per ottenere i commit recenti.
2. Analizza anche i file modificati di recente con "git diff --stat HEAD~20" per capire le aree impattate.
3. Costruisci content_json con:
   - 1. Panoramica della Versione (numero versione SemVer, data, sommario cambiamenti)
   - 2. Nuove Funzionalita' (raggruppate per area, con descrizione utente-facing)
   - 3. Miglioramenti (ottimizzazioni, refactoring, UX improvement)
   - 4. Bug Fix (con riferimento al problema risolto)
   - 5. Breaking Changes (con istruzioni di migrazione)
   - 6. Problemi Noti (bug aperti, limitazioni)
   - 7. Istruzioni di Aggiornamento (passi per aggiornare dalla versione precedente)
   - 8. Contributori (se disponibile da git log)
4. Classifica i commit secondo Conventional Commits (feat, fix, refactor, docs, chore).
5. Chiama nexus_doc_generate con doc_type="release_notes".`,
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
   *  Calcola il path relativo al progetto a partire dal file_path assoluto
   *  registrato nel DB (es. /home/.../projects/<slug>/docs/x.md → docs/x.md). */
  const handleOpen = (doc: DocumentItem) => {
    if (!onOpenInEditor) return;
    const abs = doc.file_path;
    // Prova a estrarre la parte dopo "/projects/<slug>/" o, in fallback,
    // tutto cio' che segue l'ultima occorrenza di "/<projectName>/".
    let relative = abs;
    const projectsMarker = "/projects/";
    const idx = abs.indexOf(projectsMarker);
    if (idx >= 0) {
      const afterProjects = abs.slice(idx + projectsMarker.length);
      // Salta il segmento dello slug progetto.
      const firstSlash = afterProjects.indexOf("/");
      if (firstSlash >= 0) {
        relative = afterProjects.slice(firstSlash + 1);
      }
    } else {
      // Fallback: prendi solo il nome file.
      relative = abs.split("/").pop() ?? abs;
    }
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
