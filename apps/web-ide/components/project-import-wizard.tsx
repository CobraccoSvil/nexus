"use client";

import { useState } from "react";
import {
  browseServerDirectories,
  registerProject,
  analyzeProject,
  getProjectDbConfig,
  setProjectDbConfig,
  type BrowseDirectoriesResponse,
  type ProjectAnalysis,
  type ProjectDbConfig,
} from "../lib/api-client";
import { useThemeColors } from "../lib/theme";

export interface ProjectImportWizardProps {
  onComplete: (projectId: string) => void;
  onClose: () => void;
}

function btn(
  tc: ReturnType<typeof useThemeColors>,
  variant: "primary" | "secondary" | "ghost" = "secondary",
  disabled = false,
) {
  const base = {
    padding: "8px 16px",
    borderRadius: 8,
    border: "none",
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 13,
    fontWeight: 500,
    opacity: disabled ? 0.5 : 1,
    transition: "background 0.15s",
  } as const;
  if (variant === "primary") {
    return { ...base, background: tc.accent, color: "#fff" } as const;
  }
  if (variant === "ghost") {
    return {
      ...base,
      background: "transparent",
      color: tc.textSecondary,
      border: `1px solid ${tc.border}`,
    } as const;
  }
  return {
    ...base,
    background: tc.bgInput,
    color: tc.text,
    border: `1px solid ${tc.border}`,
  } as const;
}

// ── Step 1: Directory browser ─────────────────────────────────────────────────

function DirectoryBrowser({
  onSelect,
}: {
  onSelect: (path: string) => void;
}) {
  const tc = useThemeColors();
  const [browseData, setBrowseData] = useState<BrowseDirectoriesResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [manualPath, setManualPath] = useState("");

  async function load(path?: string) {
    setBusy(true);
    setError(null);
    try {
      const data = await browseServerDirectories(path);
      setBrowseData(data);
      if (!selectedPath && data.currentPath) {
        setSelectedPath(data.currentPath);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Errore durante la navigazione.");
    } finally {
      setBusy(false);
    }
  }

  function handleManualNavigate() {
    const p = manualPath.trim();
    if (!p) return;
    void load(p);
    setSelectedPath(p);
    setManualPath("");
  }

  // Load on mount
  useState(() => {
    void load();
  });

  return (
    <div className="flex-col-gap-10">
      {/* Root shortcuts */}
      <div className="flex-row" style={{ gap: 6, flexWrap: "wrap" }}>
        {browseData?.roots.map((root) => (
          <button
            key={root}
            type="button"
            onClick={() => { void load(root); setSelectedPath(root); }}
            className="text-xs"
            style={{
              padding: "3px 8px",
              borderRadius: 6,
              border: `1px solid ${tc.border}`,
              background: browseData.currentPath === root ? tc.accentBg : tc.bgInput,
              color: browseData.currentPath === root ? tc.accent : tc.text,
              cursor: "pointer",
            }}
          >
            {root}
          </button>
        ))}
        <button
          type="button"
          disabled={!browseData?.parentPath || busy}
          onClick={() => {
            if (!browseData?.parentPath) return;
            void load(browseData.parentPath);
            setSelectedPath(browseData.parentPath);
          }}
          className="text-xs"
          style={{
            marginLeft: "auto",
            padding: "3px 8px",
            borderRadius: 6,
            border: `1px solid ${tc.border}`,
            background: tc.bgInput,
            color: (!browseData?.parentPath || busy) ? tc.textMuted : tc.text,
            cursor: (!browseData?.parentPath || busy) ? "not-allowed" : "pointer",
            opacity: (!browseData?.parentPath || busy) ? 0.5 : 1,
          }}
        >
          ↑ Su
        </button>
      </div>

      {/* Manual path input */}
      <div className="flex-row" style={{ gap: 6 }}>
        <input
          type="text"
          value={manualPath}
          onChange={(e) => setManualPath(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") handleManualNavigate(); }}
          placeholder="Percorso assoluto (es. /opt/ai-orchestrator)…"
          className="text-xs flex-1"
          style={{
            padding: "5px 10px",
            borderRadius: 7,
            border: `1px solid ${tc.border}`,
            background: tc.bgInput,
            color: tc.text,
            fontFamily: "monospace",
          }}
        />
        <button
          type="button"
          disabled={!manualPath.trim() || busy}
          onClick={handleManualNavigate}
          style={btn(tc, "secondary", !manualPath.trim() || busy)}
        >
          Vai
        </button>
      </div>

      {/* Current path indicator */}
      <div className="text-sm" style={{ color: tc.textSecondary, wordBreak: "break-all" }}>
        {browseData?.currentPath ?? "Caricamento..."}
      </div>

      {/* Directory listing */}
      <div
        className="overflow-auto"
        style={{
          border: `1px solid ${tc.border}`,
          borderRadius: 8,
          background: tc.bgInput,
          minHeight: 180,
          maxHeight: 260,
        }}
      >
        {busy ? (
          <div className="text-sm text-muted" style={{ padding: 12 }}>Caricamento...</div>
        ) : error ? (
          <div className="text-sm" style={{ padding: 12, color: tc.error }}>{error}</div>
        ) : browseData && browseData.directories.length > 0 ? (
          browseData.directories.map((dir) => (
            <button
              key={dir.path}
              type="button"
              onClick={() => {
                void load(dir.path);
                setSelectedPath(dir.path);
              }}
              className="text-base w-full"
              style={{
                textAlign: "left",
                padding: "8px 12px",
                border: "none",
                borderBottom: `1px solid ${tc.border}`,
                background: selectedPath === dir.path ? tc.accentBg : "transparent",
                color: selectedPath === dir.path ? tc.accent : tc.text,
                cursor: "pointer",
              }}
            >
              📁 {dir.name}
              {dir.hasChildren ? " /" : ""}
            </button>
          ))
        ) : (
          <div style={{ padding: 12, fontSize: 12, color: tc.textMuted }}>
            Nessuna sottodirectory disponibile.
          </div>
        )}
      </div>

      {/* Select button */}
      <button
        type="button"
        disabled={!browseData}
        onClick={() => {
          if (browseData) onSelect(browseData.currentPath);
        }}
        style={btn(tc, "primary", !browseData)}
      >
        Seleziona questa cartella: {browseData?.currentPath ?? "…"}
      </button>
    </div>
  );
}

// ── Step indicators ───────────────────────────────────────────────────────────

function StepDots({ current, total }: { current: number; total: number }) {
  const tc = useThemeColors();
  return (
    <div className="flex-row" style={{ gap: 8, justifyContent: "center" }}>
      {Array.from({ length: total }, (_, i) => {
        const step = i + 1;
        const done = step < current;
        const active = step === current;
        return (
          <div key={step} className="flex-row" style={{ gap: 8 }}>
            <div
              className="text-base font-bold"
              style={{
                width: 28,
                height: 28,
                borderRadius: "50%",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                background: done ? tc.success : active ? tc.accent : tc.bgInput,
                color: done || active ? "#fff" : tc.textMuted,
                border: `2px solid ${done ? tc.success : active ? tc.accent : tc.border}`,
              }}
            >
              {done ? "✓" : step}
            </div>
            {i < total - 1 && (
              <div
                style={{
                  width: 32,
                  height: 2,
                  background: done ? tc.success : tc.border,
                  borderRadius: 1,
                }}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

// ── Analysis result display ───────────────────────────────────────────────────

function AnalysisResult({ analysis }: { analysis: ProjectAnalysis }) {
  const tc = useThemeColors();
  const totalFiles = analysis.totalFiles;

  return (
    <div className="flex-col-gap-12">
      {/* Summary row */}
      <div className="flex-row" style={{ gap: 10, flexWrap: "wrap" }}>
        <div
          className="text-xs"
          style={{
            padding: "6px 12px",
            borderRadius: 6,
            background: tc.bgInput,
            border: `1px solid ${tc.border}`,
            color: tc.textSecondary,
          }}
        >
          📄 {totalFiles} file analizzati
        </div>
        {analysis.git.isGitRepo && (
          <div
            className="text-xs"
            style={{
              padding: "6px 12px",
              borderRadius: 6,
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              color: analysis.git.dirtyFiles ? tc.warning : tc.success,
            }}
          >
            🔀 git:{" "}
            {analysis.git.branch ?? "?"}
            {analysis.git.dirtyFiles
              ? ` (${analysis.git.dirtyFiles} modifiche)`
              : " (clean)"}
          </div>
        )}
        {!analysis.git.isGitRepo && (
          <div
            className="text-xs"
            style={{
              padding: "6px 12px",
              borderRadius: 6,
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              color: tc.textMuted,
            }}
          >
            🔀 Nessun repository git
          </div>
        )}
      </div>

      {/* Languages */}
      {analysis.languages.length > 0 && (
        <div>
          <div className="text-sm font-semibold" style={{ marginBottom: 6, color: tc.textSecondary }}>
            Linguaggi
          </div>
          <div className="flex-row" style={{ gap: 6, flexWrap: "wrap" }}>
            {analysis.languages.slice(0, 8).map((lang) => {
              const pct = totalFiles > 0 ? Math.round((lang.fileCount / totalFiles) * 100) : 0;
              return (
                <div
                  key={lang.language}
                  className="text-xs"
                  style={{
                    padding: "4px 8px",
                    borderRadius: 6,
                    background: tc.accentBg,
                    border: `1px solid ${tc.border}`,
                    color: tc.accent,
                  }}
                >
                  {lang.language} ({lang.fileCount} file{pct > 0 ? ` · ${pct}%` : ""})
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Frameworks */}
      {analysis.frameworks.length > 0 && (
        <div>
          <div className="text-sm font-semibold" style={{ marginBottom: 6, color: tc.textSecondary }}>
            Framework rilevati
          </div>
          <div className="flex-row" style={{ gap: 6, flexWrap: "wrap" }}>
            {analysis.frameworks.map((fw) => (
              <div
                key={fw}
                className="text-xs"
                style={{
                  padding: "4px 8px",
                  borderRadius: 6,
                  background: tc.bgInput,
                  border: `1px solid ${tc.border}`,
                  color: tc.text,
                }}
              >
                {fw}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Dependencies */}
      {Object.keys(analysis.dependencies).length > 0 && (
        <div>
          <div className="text-sm font-semibold" style={{ marginBottom: 4, color: tc.textSecondary }}>
            Dipendenze trovate
          </div>
          <div className="text-sm text-muted">
            {Object.keys(analysis.dependencies).join(", ")}
          </div>
        </div>
      )}

      {/* Structure flags */}
      <div className="flex-row" style={{ gap: 8, flexWrap: "wrap" }}>
        {[
          { key: "hasReadme", label: "README" },
          { key: "hasGitignore", label: ".gitignore" },
          { key: "hasLicense", label: "LICENSE" },
          { key: "hasCi", label: "CI" },
        ].map(({ key, label }) => {
          const present = analysis.structure[key as keyof typeof analysis.structure];
          return (
            <div
              key={key}
              className="text-xs"
              style={{
                padding: "3px 8px",
                borderRadius: 5,
                background: present ? tc.bgInput : "transparent",
                border: `1px solid ${present ? tc.success : tc.border}`,
                color: present ? tc.success : tc.textMuted,
              }}
            >
              {present ? "✓" : "✗"} {label}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ── Step 4: Suggested actions ─────────────────────────────────────────────────

function SuggestedActions({
  analysis,
  onOpenProject,
}: {
  analysis: ProjectAnalysis;
  onOpenProject: () => void;
}) {
  const tc = useThemeColors();

  const suggestions: { id: string; label: string; description: string; command?: string }[] = [];

  const deps = analysis.dependencies as Record<string, Record<string, unknown>>;

  if (deps.npm ?? deps.packageJson) {
    suggestions.push({
      id: "npm-install",
      label: "Installa dipendenze Node.js",
      description: "package.json trovato. Esegui npm install per installare le dipendenze.",
      command: "npm install",
    });
  }

  if (deps.pip ?? deps.requirements) {
    suggestions.push({
      id: "pip-install",
      label: "Crea virtualenv e installa dipendenze Python",
      description: "requirements.txt trovato. Crea un virtualenv e installa le dipendenze.",
      command: "python -m venv .venv && .venv/bin/pip install -r requirements.txt",
    });
  }

  if (!analysis.git.isGitRepo) {
    suggestions.push({
      id: "git-init",
      label: "Inizializza repository git",
      description: "Nessun repository git trovato. Inizializza git per il controllo versione.",
      command: "git init",
    });
  }

  if (deps.envExample) {
    suggestions.push({
      id: "env-copy",
      label: "Crea file .env da .env.example",
      description: ".env.example trovato. Copia il file e configura le variabili d'ambiente.",
      command: "cp .env.example .env",
    });
  }

  return (
    <div className="flex-col-gap-16">
      {suggestions.length === 0 ? (
        <div className="text-base" style={{ color: tc.textSecondary, textAlign: "center", padding: 20 }}>
          Nessuna azione suggerita. Il progetto sembra già configurato correttamente.
        </div>
      ) : (
        suggestions.map((s) => (
          <div
            key={s.id}
            className="card-sm flex-col-gap-6"
            style={{
              background: tc.bgInput,
            }}
          >
            <div className="text-base font-bold">{s.label}</div>
            <div className="text-sm" style={{ color: tc.textSecondary }}>{s.description}</div>
            {s.command && (
              <div
                style={{
                  fontFamily: "monospace",
                  fontSize: 12,
                  color: tc.accent,
                  background: tc.bgCard,
                  padding: "4px 8px",
                  borderRadius: 5,
                  border: `1px solid ${tc.border}`,
                }}
              >
                {s.command}
              </div>
            )}
          </div>
        ))
      )}

      <button
        type="button"
        onClick={onOpenProject}
        style={{
          ...btn(tc, "primary"),
          marginTop: 8,
          fontSize: 14,
          padding: "10px 20px",
        }}
      >
        Apri progetto nell&apos;IDE →
      </button>
    </div>
  );
}

// ── Main wizard ───────────────────────────────────────────────────────────────

export function ProjectImportWizard({ onComplete, onClose }: ProjectImportWizardProps) {
  const tc = useThemeColors();

  const [step, setStep] = useState(1);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [projectName, setProjectName] = useState("");
  const [registeredProjectId, setRegisteredProjectId] = useState<string | null>(null);
  const [analysis, setAnalysis] = useState<ProjectAnalysis | null>(null);

  const [registering, setRegistering] = useState(false);
  const [registerError, setRegisterError] = useState<string | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [analyzeError, setAnalyzeError] = useState<string | null>(null);

  // Step 4: DB config state
  const [dbProfile, setDbProfile] = useState<ProjectDbConfig | null>(null);
  const [dbHostingMode, setDbHostingMode] = useState<"internal" | "external">("external");
  const [dbHost, setDbHost] = useState("localhost");
  const [dbPort, setDbPort] = useState("5432");
  const [dbName, setDbName] = useState("app");
  const [dbUser, setDbUser] = useState("postgres");
  const [dbPassword, setDbPassword] = useState("");
  const [dbSaving, setDbSaving] = useState(false);
  const [dbSaveError, setDbSaveError] = useState<string | null>(null);

  // Derive default name from path
  function deriveNameFromPath(path: string): string {
    const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
    return parts[parts.length - 1] ?? path;
  }

  function handlePathSelected(path: string) {
    setSelectedPath(path);
    setProjectName(deriveNameFromPath(path));
    setStep(2);
  }

  async function handleRegister() {
    if (!selectedPath) return;
    setRegistering(true);
    setRegisterError(null);
    try {
      const result = await registerProject(selectedPath, projectName.trim() || undefined);
      setRegisteredProjectId(result.project.id);
      setStep(3);
      void handleAnalyze(result.project.id);
    } catch (err) {
      setRegisterError(err instanceof Error ? err.message : "Errore durante la registrazione.");
    } finally {
      setRegistering(false);
    }
  }

  async function handleAnalyze(projectId: string) {
    setAnalyzing(true);
    setAnalyzeError(null);
    try {
      const result = await analyzeProject(projectId);
      setAnalysis(result);
      // Carica profilo DB rilevato dal detector (se disponibile dopo analisi)
      try {
        const dbConf = await getProjectDbConfig(projectId);
        setDbProfile(dbConf);
      } catch {
        // non bloccante
      }
      setStep(4);
    } catch (err) {
      setAnalyzeError(err instanceof Error ? err.message : "Errore durante l'analisi.");
    } finally {
      setAnalyzing(false);
    }
  }

  async function handleSaveDbConfig() {
    if (!registeredProjectId) return;
    setDbSaving(true);
    setDbSaveError(null);
    try {
      if (dbHostingMode === "internal") {
        await setProjectDbConfig(registeredProjectId, {
          hosting_mode: "internal",
          engine: "postgres",
          migration_tool: dbProfile?.migration_tool ?? undefined,
          migration_path: dbProfile?.migration_path ?? undefined,
        });
      } else {
        await setProjectDbConfig(registeredProjectId, {
          hosting_mode: "external",
          engine: "postgres",
          migration_tool: dbProfile?.migration_tool ?? undefined,
          migration_path: dbProfile?.migration_path ?? undefined,
          connection_host: dbHost,
          connection_port: parseInt(dbPort),
          connection_database: dbName,
          connection_user: dbUser,
          connection_password: dbPassword,
        });
      }
      setStep(5);
    } catch (err) {
      setDbSaveError(err instanceof Error ? err.message : "Errore durante il salvataggio.");
    } finally {
      setDbSaving(false);
    }
  }

  function handleOpenProject() {
    if (registeredProjectId) {
      onComplete(registeredProjectId);
    }
  }

  const stepLabels = ["Directory", "Registrazione", "Analisi", "Database", "Azioni"];

  return (
    <div
      className="fixed inset-0 flex-row"
      style={{
        background: "rgba(0,0,0,0.5)",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 80,
        padding: 20,
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="flex-col"
        style={{
          width: 680,
          maxWidth: "96vw",
          maxHeight: "90vh",
          overflow: "auto",
          borderRadius: 14,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          boxShadow: "0 20px 60px rgba(0,0,0,0.45)",
          gap: 0,
        }}
      >
        {/* Header */}
        <div
          className="flex-row"
          style={{
            padding: "16px 20px",
            borderBottom: `1px solid ${tc.border}`,
            justifyContent: "space-between",
          }}
        >
          <div className="font-bold" style={{ color: tc.text, fontSize: 15 }}>
            Importa progetto esistente
          </div>
          <button
            type="button"
            onClick={onClose}
            className="text-lg"
            style={{
              width: 28,
              height: 28,
              borderRadius: 7,
              border: `1px solid ${tc.border}`,
              background: tc.bgInput,
              color: tc.textSecondary,
              cursor: "pointer",
            }}
          >
            ✕
          </button>
        </div>

        {/* Step indicator */}
        <div style={{ padding: "16px 20px", borderBottom: `1px solid ${tc.border}` }}>
          <StepDots current={step} total={5} />
          <div className="text-xs text-muted" style={{ textAlign: "center", marginTop: 8 }}>
            Step {step} di 5 — {stepLabels[step - 1]}
          </div>
        </div>

        {/* Content */}
        <div style={{ padding: 20, flex: 1 }}>
          {/* Step 1 */}
          {step === 1 && (
            <div>
              <div className="text-base" style={{ color: tc.textSecondary, marginBottom: 14 }}>
                Naviga il filesystem del server e seleziona la cartella del progetto esistente.
              </div>
              <DirectoryBrowser onSelect={handlePathSelected} />
            </div>
          )}

          {/* Step 2 */}
          {step === 2 && (
            <div className="flex-col-gap-16">
              <div className="text-base" style={{ color: tc.textSecondary }}>
                Conferma il percorso e assegna un nome al progetto.
              </div>

              <div className="flex-col-gap-6">
                <label className="text-sm text-muted">Percorso selezionato</label>
                <div
                  className="text-base"
                  style={{
                    padding: "8px 12px",
                    borderRadius: 8,
                    border: `1px solid ${tc.border}`,
                    background: tc.bgInput,
                    color: tc.textSecondary,
                    fontFamily: "monospace",
                    wordBreak: "break-all",
                  }}
                >
                  {selectedPath}
                </div>
              </div>

              <div className="flex-col-gap-6">
                <label className="text-sm text-muted">Nome progetto</label>
                <input
                  value={projectName}
                  onChange={(e) => setProjectName(e.target.value)}
                  placeholder="Nome progetto"
                  className="text-base w-full"
                  style={{
                    padding: "8px 12px",
                    borderRadius: 8,
                    border: `1px solid ${tc.border}`,
                    background: tc.bgInput,
                    color: tc.text,
                    outline: "none",
                  }}
                />
              </div>

              {registerError && (
                <div
                  className="text-xs"
                  style={{
                    padding: "8px 12px",
                    borderRadius: 8,
                    background: tc.bgInput,
                    border: `1px solid ${tc.error}`,
                    color: tc.error,
                  }}
                >
                  {registerError}
                </div>
              )}

              <div style={{ display: "flex", gap: 8, justifyContent: "space-between" }}>
                <button type="button" onClick={() => setStep(1)} style={btn(tc, "ghost")}>
                  ← Indietro
                </button>
                <button
                  type="button"
                  disabled={registering || !selectedPath}
                  onClick={() => { void handleRegister(); }}
                  style={btn(tc, "primary", registering || !selectedPath)}
                >
                  {registering ? "Registrazione…" : "Registra progetto →"}
                </button>
              </div>
            </div>
          )}

          {/* Step 3 */}
          {step === 3 && (
            <div
              className="flex-col-gap-16"
              style={{
                alignItems: "center",
                padding: "30px 0",
              }}
            >
              {analyzing ? (
                <>
                  <div
                    style={{
                      width: 48,
                      height: 48,
                      borderRadius: "50%",
                      border: `3px solid ${tc.border}`,
                      borderTopColor: tc.accent,
                      animation: "spin 0.8s linear infinite",
                    }}
                  />
                  <div className="text-lg" style={{ color: tc.textSecondary }}>
                    Analisi in corso…
                  </div>
                  <div className="text-xs" style={{ color: tc.textMuted }}>
                    Rilevamento linguaggi, framework e dipendenze
                  </div>
                </>
              ) : analyzeError ? (
                <div className="flex-col-gap-12 w-full">
                  <div className="text-base" style={{ color: tc.error }}>{analyzeError}</div>
                  <div className="flex-row" style={{ gap: 8 }}>
                    <button type="button" onClick={() => setStep(2)} style={btn(tc, "ghost")}>
                      ← Indietro
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        if (registeredProjectId) void handleAnalyze(registeredProjectId);
                      }}
                      style={btn(tc, "primary")}
                    >
                      Riprova
                    </button>
                    <button
                      type="button"
                      onClick={() => setStep(4)}
                      style={btn(tc, "secondary")}
                    >
                      Salta analisi →
                    </button>
                  </div>
                </div>
              ) : null}
            </div>
          )}

          {/* Step 4 — Configurazione DB */}
          {step === 4 && (
            <div className="flex-col-gap-16">
              <div className="text-base font-semibold" style={{ color: tc.text }}>
                Configurazione database
              </div>

              {dbProfile?.migration_tool && (
                <div
                  style={{
                    background: tc.bgCard,
                    border: `1px solid ${tc.border}`,
                    borderRadius: 8,
                    padding: 12,
                  }}
                >
                  <div className="text-xs text-muted">Rilevato automaticamente</div>
                  <div className="text-sm" style={{ color: tc.text, marginTop: 4 }}>
                    Tool: <strong>{dbProfile.migration_tool}</strong> &nbsp;|&nbsp;
                    Engine: <strong>{dbProfile.engine ?? "postgres"}</strong>
                  </div>
                  {dbProfile.migration_path && (
                    <div className="text-xs text-muted" style={{ marginTop: 4 }}>
                      Percorso migrations: {dbProfile.migration_path}
                    </div>
                  )}
                </div>
              )}

              <div>
                <div className="text-sm font-semibold" style={{ color: tc.text, marginBottom: 8 }}>
                  Hosting database
                </div>
                <div className="flex-col" style={{ gap: 8 }}>
                  <label className="flex-row" style={{ gap: 8, alignItems: "center", cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="hosting"
                      value="internal"
                      checked={dbHostingMode === "internal"}
                      onChange={() => setDbHostingMode("internal")}
                    />
                    <span className="text-sm" style={{ color: tc.text }}>
                      <strong>Nexus-managed</strong> — container Docker automatico (consigliato)
                    </span>
                  </label>
                  <label className="flex-row" style={{ gap: 8, alignItems: "center", cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="hosting"
                      value="external"
                      checked={dbHostingMode === "external"}
                      onChange={() => setDbHostingMode("external")}
                    />
                    <span className="text-sm" style={{ color: tc.text }}>
                      <strong>Server esterno</strong> — connessione a DB esistente
                    </span>
                  </label>
                </div>
              </div>

              {dbHostingMode === "external" && (
                <div className="flex-col" style={{ gap: 8 }}>
                  <div className="flex-row" style={{ gap: 8 }}>
                    <div className="flex-col" style={{ flex: 2, gap: 4 }}>
                      <label className="text-xs text-muted">Host</label>
                      <input
                        type="text"
                        value={dbHost}
                        onChange={(e) => setDbHost(e.target.value)}
                        placeholder="localhost"
                        style={{
                          background: tc.bgCard, color: tc.text,
                          border: `1px solid ${tc.border}`, borderRadius: 6,
                          padding: "6px 10px", fontSize: 13,
                        }}
                      />
                    </div>
                    <div className="flex-col" style={{ flex: 1, gap: 4 }}>
                      <label className="text-xs text-muted">Porta</label>
                      <input
                        type="text"
                        value={dbPort}
                        onChange={(e) => setDbPort(e.target.value)}
                        placeholder="5432"
                        style={{
                          background: tc.bgCard, color: tc.text,
                          border: `1px solid ${tc.border}`, borderRadius: 6,
                          padding: "6px 10px", fontSize: 13,
                        }}
                      />
                    </div>
                  </div>
                  <div className="flex-col" style={{ gap: 4 }}>
                    <label className="text-xs text-muted">Database</label>
                    <input
                      type="text"
                      value={dbName}
                      onChange={(e) => setDbName(e.target.value)}
                      placeholder="app"
                      style={{
                        background: tc.bgCard, color: tc.text,
                        border: `1px solid ${tc.border}`, borderRadius: 6,
                        padding: "6px 10px", fontSize: 13,
                      }}
                    />
                  </div>
                  <div className="flex-row" style={{ gap: 8 }}>
                    <div className="flex-col" style={{ flex: 1, gap: 4 }}>
                      <label className="text-xs text-muted">Utente</label>
                      <input
                        type="text"
                        value={dbUser}
                        onChange={(e) => setDbUser(e.target.value)}
                        placeholder="postgres"
                        style={{
                          background: tc.bgCard, color: tc.text,
                          border: `1px solid ${tc.border}`, borderRadius: 6,
                          padding: "6px 10px", fontSize: 13,
                        }}
                      />
                    </div>
                    <div className="flex-col" style={{ flex: 1, gap: 4 }}>
                      <label className="text-xs text-muted">Password</label>
                      <input
                        type="password"
                        value={dbPassword}
                        onChange={(e) => setDbPassword(e.target.value)}
                        placeholder="••••••"
                        style={{
                          background: tc.bgCard, color: tc.text,
                          border: `1px solid ${tc.border}`, borderRadius: 6,
                          padding: "6px 10px", fontSize: 13,
                        }}
                      />
                    </div>
                  </div>
                </div>
              )}

              {dbSaveError && (
                <div className="text-sm" style={{ color: tc.error }}>{dbSaveError}</div>
              )}

              <div className="flex-row" style={{ gap: 8 }}>
                <button type="button" onClick={() => setStep(3)} style={btn(tc, "ghost")}>
                  Indietro
                </button>
                <button
                  type="button"
                  onClick={() => setStep(5)}
                  style={btn(tc, "secondary")}
                >
                  Salta
                </button>
                <button
                  type="button"
                  onClick={() => void handleSaveDbConfig()}
                  disabled={dbSaving}
                  style={btn(tc, "primary")}
                >
                  {dbSaving ? "Salvataggio..." : "Salva e continua"}
                </button>
              </div>
            </div>
          )}

          {/* Step 5 — Azioni */}
          {step === 5 && (
            <div className="flex-col-gap-16">
              {analysis && (
                <div>
                  <div
                    className="text-base font-semibold"
                    style={{ color: tc.textSecondary, marginBottom: 12 }}
                  >
                    Risultato analisi
                  </div>
                  <AnalysisResult analysis={analysis} />
                </div>
              )}

              <div style={{ borderTop: `1px solid ${tc.border}`, paddingTop: 16 }}>
                <div
                  className="text-base font-semibold"
                  style={{ color: tc.textSecondary, marginBottom: 12 }}
                >
                  Azioni suggerite
                </div>
                {analysis ? (
                  <SuggestedActions
                    analysis={analysis}
                    onOpenProject={handleOpenProject}
                  />
                ) : (
                  <div className="flex-col-gap-10">
                    <div className="text-xs" style={{ color: tc.textMuted }}>
                      Analisi non disponibile. Puoi comunque aprire il progetto nell&apos;IDE.
                    </div>
                    <button
                      type="button"
                      onClick={handleOpenProject}
                      style={btn(tc, "primary")}
                    >
                      Apri progetto nell&apos;IDE →
                    </button>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
