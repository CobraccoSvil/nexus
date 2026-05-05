"use client";

import { useEffect, useState, useTransition } from "react";
import {
  cloneProject,
  deleteProject,
  getGitHubAccount,
  listUserGitHubRepositories,
  type GitHubRepositorySummary,
  type UserProjectSummary,
} from "../lib/api-client";
import { useThemeColors } from "../lib/theme";

function iconButtonStyle(
  tc: ReturnType<typeof useThemeColors>,
  disabled = false,
  active = false,
  danger = false,
) {
  return {
    width: 28,
    height: 28,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    borderRadius: 6,
    border: `1px solid ${danger ? "#ef444440" : active ? tc.accent : tc.border}`,
    background: disabled ? tc.bgInput : danger ? "#ef444412" : active ? tc.accentBg : "transparent",
    color: disabled ? tc.textMuted : danger ? "#ef4444" : active ? tc.accent : tc.textMuted,
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 13,
    lineHeight: 1,
    flexShrink: 0,
    transition: "all 0.12s",
  } as const;
}

type DeleteState =
  | { phase: "idle" }
  | { phase: "confirm"; projectId: string; projectName: string }
  | { phase: "dirty"; projectId: string; projectName: string; dirtyCount: number; rootPath: string }
  | { phase: "busy"; projectId: string };

type ProjectSwitcherProps = {
  projects: UserProjectSummary[];
  activeProjectId?: string;
  onSelect: (projectId: string) => Promise<void>;
  onRefreshProjects?: () => void;
  onRegister?: (absolutePath: string, name?: string) => Promise<void>;
};

export function ProjectSwitcher({ projects, activeProjectId, onSelect, onRefreshProjects }: ProjectSwitcherProps) {
  const tc = useThemeColors();
  const [isPending, startTransition] = useTransition();
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [cloneUrl, setCloneUrl] = useState("");
  const [cloneName, setCloneName] = useState("");
  const [cloneBusy, setCloneBusy] = useState(false);
  const [cloneError, setCloneError] = useState<string | null>(null);
  const [deleteState, setDeleteState] = useState<DeleteState>({ phase: "idle" });
  const [deleteError, setDeleteError] = useState<string | null>(null);

  // GitHub repo picker
  const [githubConnected, setGithubConnected] = useState(false);
  const [githubRepos, setGithubRepos] = useState<GitHubRepositorySummary[]>([]);
  const [reposLoading, setReposLoading] = useState(false);
  const [repoSearch, setRepoSearch] = useState("");

  // Load GitHub status + repos when modal opens
  useEffect(() => {
    if (!isModalOpen) return;
    void (async () => {
      try {
        const { account } = await getGitHubAccount();
        if (account.status !== "connected") return;
        setGithubConnected(true);
        setReposLoading(true);
        const { repositories } = await listUserGitHubRepositories();
        setGithubRepos(repositories);
      } catch { /* non bloccante */ } finally {
        setReposLoading(false);
      }
    })();
  }, [isModalOpen]);

  async function handleClone() {
    const url = cloneUrl.trim();
    if (!url) return;
    setCloneBusy(true);
    setCloneError(null);
    try {
      const result = await cloneProject(url, cloneName.trim() || undefined);
      setCloneUrl("");
      setCloneName("");
      setIsModalOpen(false);
      onRefreshProjects?.();
      startTransition(() => { void onSelect(result.project.id); });
    } catch (err) {
      setCloneError(err instanceof Error ? err.message : "Errore durante il clone.");
    } finally {
      setCloneBusy(false);
    }
  }

  function requestDelete(project: UserProjectSummary) {
    setDeleteError(null);
    setDeleteState({ phase: "confirm", projectId: project.id, projectName: project.name });
  }

  async function executeDelete(projectId: string, force: boolean) {
    setDeleteState((prev) => ({ ...prev, phase: "busy" } as DeleteState));
    setDeleteError(null);
    try {
      const result = await deleteProject(projectId, force);
      if (result.hasPendingChanges) {
        setDeleteState({
          phase: "dirty",
          projectId,
          projectName: result.projectName ?? "Progetto",
          dirtyCount: result.dirtyCount ?? 0,
          rootPath: result.rootPath ?? "",
        });
        return;
      }
      // Deleted successfully
      setDeleteState({ phase: "idle" });
      onRefreshProjects?.();
      // If the deleted project was active, reset selection
      if (projectId === activeProjectId) {
        window.location.href = "/";
      }
    } catch (err) {
      setDeleteState({ phase: "idle" });
      setDeleteError(err instanceof Error ? err.message : "Errore durante l'eliminazione.");
    }
  }

  const ds = deleteState;

  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
        <select
          value={activeProjectId ?? ""}
          onChange={(event) => {
            const next = event.target.value;
            if (!next) return;
            startTransition(() => { void onSelect(next); });
          }}
          title="Selettore progetto"
          aria-label="Selettore progetto"
          style={{
            minWidth: 220,
            maxWidth: 360,
            padding: "6px 10px",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: tc.text,
          }}
        >
          <option value="">Seleziona progetto</option>
          {projects.map((project) => (
            <option key={project.id} value={project.id}>
              {project.name}{project.isShared ? " [shared]" : ""}
            </option>
          ))}
        </select>

        <button
          type="button"
          onClick={() => setIsModalOpen(true)}
          title="Gestisci progetti"
          aria-label="Gestisci progetti"
          style={{
            ...iconButtonStyle(tc, false, isModalOpen),
            width: 32,
            height: 32,
            fontSize: 14,
          }}
        >
          ⌘
        </button>
      </div>

      {isModalOpen && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.35)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 60,
            padding: 20,
          }}
          onClick={(e) => { if (e.target === e.currentTarget) setIsModalOpen(false); }}
        >
          <div
            style={{
              width: 580,
              maxWidth: "96vw",
              maxHeight: "86vh",
              overflow: "auto",
              borderRadius: 12,
              border: `1px solid ${tc.border}`,
              background: tc.bgCard,
              boxShadow: "0 10px 32px rgba(0,0,0,0.35)",
              padding: 16,
              display: "flex",
              flexDirection: "column",
              gap: 12,
            }}
          >
            {/* Header */}
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
              <div style={{ color: tc.text, fontWeight: 700, fontSize: 14 }}>Progetti</div>
              <button
                type="button"
                onClick={() => setIsModalOpen(false)}
                style={iconButtonStyle(tc)}
              >
                ✕
              </button>
            </div>

            {/* Project list */}
            <div style={{ display: "flex", flexDirection: "column", gap: 2, maxHeight: 280, overflowY: "auto" }}>
              {projects.length === 0 && (
                <div style={{ fontSize: 12, color: tc.textMuted, padding: "8px 0" }}>
                  Nessun progetto. Clona un repository per iniziare.
                </div>
              )}
              {projects.map((project) => {
                const isActive = project.id === activeProjectId;
                const isDeleting = ds.phase === "busy" && ds.projectId === project.id;
                return (
                  <div
                    key={project.id}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 4,
                      padding: "5px 6px",
                      borderRadius: 6,
                      background: isActive ? tc.accentBg : "transparent",
                    }}
                  >
                    {/* Project name button */}
                    <button
                      type="button"
                      disabled={isPending || isDeleting}
                      onClick={() => {
                        startTransition(() => {
                          void onSelect(project.id).then(() => setIsModalOpen(false));
                        });
                      }}
                      style={{
                        flex: 1,
                        textAlign: "left",
                        background: "none",
                        border: "none",
                        color: isActive ? tc.accent : tc.text,
                        cursor: isPending || isDeleting ? "wait" : "pointer",
                        fontSize: 13,
                        padding: "0 4px",
                        fontWeight: isActive ? 600 : 400,
                        minWidth: 0,
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {project.name}
                      {project.isShared ? <span style={{ fontSize: 11, color: tc.textMuted, marginLeft: 4 }}>[shared]</span> : null}
                    </button>

                    {/* Open in new tab */}
                    <button
                      type="button"
                      onClick={() => { window.open("/?project=" + project.id, "_blank"); }}
                      title="Apri in nuova scheda"
                      style={iconButtonStyle(tc)}
                    >
                      ⧉
                    </button>

                    {/* Delete button */}
                    <button
                      type="button"
                      disabled={isDeleting}
                      onClick={() => requestDelete(project)}
                      title="Elimina progetto"
                      style={iconButtonStyle(tc, isDeleting, false, true)}
                    >
                      {isDeleting ? "…" : "🗑"}
                    </button>
                  </div>
                );
              })}
            </div>

            {deleteError && (
              <div style={{ fontSize: 12, color: tc.error, padding: "4px 6px", borderRadius: 6, background: `${tc.error}18` }}>
                {deleteError}
              </div>
            )}

            {/* Clone from GitHub */}
            <div style={{ borderTop: `1px solid ${tc.border}`, paddingTop: 12, display: "flex", flexDirection: "column", gap: 8 }}>
              <div style={{ color: tc.textSecondary, fontSize: 12, fontWeight: 600 }}>
                Clone da GitHub
              </div>

              {/* GitHub repo list (shown when connected) */}
              {githubConnected && (
                <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                  <input
                    type="text"
                    value={repoSearch}
                    onChange={(e) => setRepoSearch(e.target.value)}
                    placeholder={reposLoading ? "Caricamento repository…" : "Cerca repository…"}
                    disabled={reposLoading || cloneBusy}
                    style={{
                      width: "100%",
                      padding: "7px 10px",
                      borderRadius: 8,
                      border: `1px solid ${tc.border}`,
                      background: tc.bgInput,
                      color: tc.text,
                      fontSize: 12,
                      boxSizing: "border-box",
                    }}
                  />
                  {!reposLoading && githubRepos.length > 0 && (
                    <div style={{
                      maxHeight: 180,
                      overflowY: "auto",
                      border: `1px solid ${tc.border}`,
                      borderRadius: 8,
                      background: tc.bg,
                      display: "flex",
                      flexDirection: "column",
                    }}>
                      {githubRepos
                        .filter((r) => !repoSearch.trim() || r.fullName.toLowerCase().includes(repoSearch.toLowerCase()))
                        .map((repo) => {
                          const isSelected = cloneUrl === repo.cloneUrl;
                          return (
                            <button
                              key={repo.id}
                              type="button"
                              onClick={() => {
                                setCloneUrl(repo.cloneUrl);
                                setCloneName((prev) => prev || repo.name);
                              }}
                              style={{
                                textAlign: "left",
                                padding: "7px 10px",
                                background: isSelected ? tc.accentBg : "transparent",
                                border: "none",
                                borderBottom: `1px solid ${tc.border}`,
                                color: isSelected ? tc.accent : tc.text,
                                cursor: "pointer",
                                fontSize: 12,
                                display: "flex",
                                alignItems: "center",
                                justifyContent: "space-between",
                                gap: 8,
                              }}
                            >
                              <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                                {isSelected ? "✓ " : ""}{repo.fullName}
                              </span>
                              <span style={{ fontSize: 10, color: isSelected ? tc.accent : tc.textMuted, flexShrink: 0 }}>
                                {repo.private ? "🔒" : "🌐"} {repo.defaultBranch}
                              </span>
                            </button>
                          );
                        })}
                    </div>
                  )}
                  {!reposLoading && githubRepos.length === 0 && (
                    <div style={{ fontSize: 11, color: tc.textMuted, padding: "4px 2px" }}>
                      Nessun repository trovato nell&apos;account GitHub.
                    </div>
                  )}
                  <div style={{ fontSize: 11, color: tc.textMuted }}>
                    Oppure inserisci un URL manualmente:
                  </div>
                </div>
              )}

              {/* Manual URL input */}
              <input
                type="url"
                value={cloneUrl}
                onChange={(e) => setCloneUrl(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && !cloneBusy) void handleClone(); }}
                placeholder="https://github.com/utente/repository.git"
                disabled={cloneBusy}
                style={{
                  width: "100%",
                  padding: "8px 10px",
                  borderRadius: 8,
                  border: `1px solid ${cloneUrl ? tc.accent : tc.border}`,
                  background: tc.bgInput,
                  color: tc.text,
                  fontSize: 13,
                  boxSizing: "border-box",
                }}
              />
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  type="text"
                  value={cloneName}
                  onChange={(e) => setCloneName(e.target.value)}
                  placeholder="Nome progetto (opzionale)"
                  disabled={cloneBusy}
                  style={{
                    flex: 1,
                    minWidth: 0,
                    padding: "8px 10px",
                    borderRadius: 8,
                    border: `1px solid ${tc.border}`,
                    background: tc.bgInput,
                    color: tc.text,
                    fontSize: 13,
                    boxSizing: "border-box",
                  }}
                />
                <button
                  type="button"
                  disabled={!cloneUrl.trim() || cloneBusy}
                  onClick={() => { void handleClone(); }}
                  style={{
                    padding: "8px 16px",
                    borderRadius: 8,
                    border: "none",
                    background: !cloneUrl.trim() || cloneBusy ? tc.bgInput : tc.accent,
                    color: !cloneUrl.trim() || cloneBusy ? tc.textMuted : "#fff",
                    cursor: !cloneUrl.trim() || cloneBusy ? "not-allowed" : "pointer",
                    fontSize: 13,
                    fontWeight: 600,
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {cloneBusy ? "Cloning…" : "Clone"}
                </button>
              </div>
              {cloneError && <div style={{ fontSize: 12, color: tc.error }}>{cloneError}</div>}
              {cloneBusy && <div style={{ fontSize: 12, color: tc.textMuted, fontStyle: "italic" }}>Clone in corso, attendere…</div>}
            </div>
          </div>
        </div>
      )}

      {/* Delete confirm dialog */}
      {(ds.phase === "confirm" || ds.phase === "dirty") && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.5)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 70,
            padding: 20,
          }}
        >
          <div
            style={{
              width: 440,
              maxWidth: "96vw",
              borderRadius: 12,
              border: `1px solid #ef444440`,
              background: tc.bgCard,
              boxShadow: "0 10px 32px rgba(0,0,0,0.4)",
              padding: 20,
              display: "flex",
              flexDirection: "column",
              gap: 14,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <span style={{ fontSize: 22 }}>🗑</span>
              <div>
                <div style={{ color: tc.text, fontWeight: 700, fontSize: 14 }}>
                  Elimina «{ds.projectName}»
                </div>
                {ds.phase === "dirty" && (
                  <div style={{ fontSize: 12, color: "#f97316", marginTop: 2 }}>
                    ⚠ {ds.dirtyCount} file con modifiche non committate
                  </div>
                )}
              </div>
            </div>

            {ds.phase === "confirm" && (
              <div style={{ fontSize: 13, color: tc.textSecondary, lineHeight: 1.5 }}>
                Questa operazione eliminerà il progetto dal database e rimuoverà
                la directory locale in modo <strong>permanente</strong>.<br />
                L&apos;operazione non è reversibile.
              </div>
            )}

            {ds.phase === "dirty" && (
              <div style={{ fontSize: 13, color: tc.textSecondary, lineHeight: 1.5 }}>
                Ci sono <strong>{ds.dirtyCount} file non committati</strong> in{" "}
                <code style={{ fontSize: 11, background: tc.bgInput, padding: "1px 4px", borderRadius: 4 }}>
                  {ds.rootPath}
                </code>
                .<br /><br />
                Vuoi procedere ugualmente? Le modifiche andranno <strong>perse definitivamente</strong>.
              </div>
            )}

            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button
                type="button"
                onClick={() => setDeleteState({ phase: "idle" })}
                style={{
                  padding: "8px 16px",
                  borderRadius: 8,
                  border: `1px solid ${tc.border}`,
                  background: tc.bgInput,
                  color: tc.text,
                  cursor: "pointer",
                  fontSize: 13,
                }}
              >
                Annulla
              </button>

              {ds.phase === "dirty" && (
                <button
                  type="button"
                  onClick={() => void executeDelete(ds.projectId, true)}
                  style={{
                    padding: "8px 16px",
                    borderRadius: 8,
                    border: "none",
                    background: "#ef4444",
                    color: "#fff",
                    cursor: "pointer",
                    fontSize: 13,
                    fontWeight: 600,
                  }}
                >
                  Elimina comunque
                </button>
              )}

              {ds.phase === "confirm" && (
                <button
                  type="button"
                  onClick={() => void executeDelete(ds.projectId, false)}
                  style={{
                    padding: "8px 16px",
                    borderRadius: 8,
                    border: "none",
                    background: "#ef4444",
                    color: "#fff",
                    cursor: "pointer",
                    fontSize: 13,
                    fontWeight: 600,
                  }}
                >
                  Elimina progetto
                </button>
              )}
            </div>
          </div>
        </div>
      )}
    </>
  );
}
