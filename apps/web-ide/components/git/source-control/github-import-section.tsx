"use client";

import type {
  GitHubAccountStatus,
  GitHubRepositorySummary,
  UserProjectDetails,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { cardStyle, inputStyle, sectionTitleStyle, smallButtonStyle } from "./styles";

interface GitHubImportSectionProps {
  project: UserProjectDetails;
  githubAccount: GitHubAccountStatus | null;
  githubBusy: boolean;
  busy: boolean;
  githubLoading: boolean;
  githubMessage: string | null;
  githubError: string | null;
  // Pubblica progetto locale su GitHub
  createRepoOpen: boolean;
  setCreateRepoOpen: (open: boolean) => void;
  createRepoName: string;
  setCreateRepoName: (name: string) => void;
  createRepoDesc: string;
  setCreateRepoDesc: (desc: string) => void;
  createRepoPrivate: boolean;
  setCreateRepoPrivate: (priv: boolean) => void;
  createRepoBusy: boolean;
  onCreateGithubRepo: () => void;
  // Importa repository
  repoQuery: string;
  setRepoQuery: (query: string) => void;
  selectedCloneUrl: string;
  setSelectedCloneUrl: (url: string) => void;
  filteredRepositories: GitHubRepositorySummary[];
  selectedRepository?: GitHubRepositorySummary;
  cloneTargetExists: boolean | null;
  canCloneSelected: boolean;
  onLoadRepositories: () => void;
  onCloneSelectedRepository: () => void;
}

export function GitHubImportSection({
  project,
  githubAccount,
  githubBusy,
  busy,
  githubLoading,
  githubMessage,
  githubError,
  createRepoOpen,
  setCreateRepoOpen,
  createRepoName,
  setCreateRepoName,
  createRepoDesc,
  setCreateRepoDesc,
  createRepoPrivate,
  setCreateRepoPrivate,
  createRepoBusy,
  onCreateGithubRepo,
  repoQuery,
  setRepoQuery,
  selectedCloneUrl,
  setSelectedCloneUrl,
  filteredRepositories,
  selectedRepository,
  cloneTargetExists,
  canCloneSelected,
  onLoadRepositories,
  onCloneSelectedRepository,
}: GitHubImportSectionProps) {
  const tc = useThemeColors();

  return (
    <>
      {/* Pubblica progetto locale su GitHub: orchestrazione completa
          (init git + commit + crea repo + push). Mostrato solo se
          l'utente e' connesso a GitHub. */}
      {githubAccount?.connected ? (
        <div style={cardStyle(tc)}>
          <div style={sectionTitleStyle(tc)}>Pubblica progetto su GitHub</div>
          <div style={{ color: tc.textMuted, fontSize: 12 }}>
            Il progetto non e' ancora versionato. Nexus puo' inizializzare git,
            creare un repository su <strong>github.com/{githubAccount.username ?? "..."}</strong>{" "}
            e fare il push iniziale in un solo passaggio.
          </div>
          {!createRepoOpen ? (
            <button
              disabled={createRepoBusy || githubBusy || busy}
              onClick={() => {
                setCreateRepoOpen(true);
                if (!createRepoName) setCreateRepoName(project?.slug ?? project?.name ?? "");
              }}
              style={smallButtonStyle(tc, createRepoBusy || githubBusy || busy)}
            >
              Pubblica su GitHub
            </button>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <input
                value={createRepoName}
                onChange={(e) => setCreateRepoName(e.target.value)}
                placeholder="Nome repository (alfanumerico, '-', '_', '.')"
                style={inputStyle(tc)}
              />
              <input
                value={createRepoDesc}
                onChange={(e) => setCreateRepoDesc(e.target.value)}
                placeholder="Descrizione (opzionale)"
                style={inputStyle(tc)}
              />
              <label style={{ display: "flex", alignItems: "center", gap: 8, color: tc.text }}>
                <input
                  type="checkbox"
                  checked={createRepoPrivate}
                  onChange={(e) => setCreateRepoPrivate(e.target.checked)}
                />
                Repository privato
              </label>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                <button
                  disabled={createRepoBusy || !createRepoName.trim()}
                  onClick={() => void onCreateGithubRepo()}
                  style={smallButtonStyle(tc, createRepoBusy || !createRepoName.trim())}
                >
                  {createRepoBusy ? "Pubblicazione in corso..." : "Conferma e pubblica"}
                </button>
                <button
                  disabled={createRepoBusy}
                  onClick={() => setCreateRepoOpen(false)}
                  style={smallButtonStyle(tc, createRepoBusy)}
                >
                  Annulla
                </button>
              </div>
              <div style={{ fontSize: 10, color: tc.textMuted }}>
                L'operazione esegue: git init -b main → .gitignore (se manca) →
                git add -A → git commit → crea repo GitHub → git push -u origin main.
              </div>
            </div>
          )}
          {githubMessage ? <div style={{ color: tc.success, fontSize: 12 }}>{githubMessage}</div> : null}
          {githubError ? <div style={{ color: tc.error, fontSize: 12 }}>{githubError}</div> : null}
        </div>
      ) : null}
      <div style={cardStyle(tc)}>
        <div style={sectionTitleStyle(tc)}>Importa Repository GitHub</div>
        <div style={{ color: tc.textMuted, fontSize: 12 }}>
          La directory selezionata non e' un repository Git. Per clonare in questo progetto la cartella deve essere vuota; altrimenti crea un nuovo progetto/cartella e riprova.
        </div>
        {githubAccount?.connected ? (
          <>
            <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
              <input
                value={repoQuery}
                onChange={(event) => setRepoQuery(event.target.value)}
                placeholder="Cerca repository..."
                style={inputStyle(tc)}
              />
              <button
                disabled={githubBusy || busy}
                onClick={() => void onLoadRepositories()}
                style={smallButtonStyle(tc, githubBusy || busy)}
              >
                Aggiorna lista
              </button>
            </div>
            <select
              value={selectedCloneUrl}
              onChange={(event) => setSelectedCloneUrl(event.target.value)}
              style={inputStyle(tc)}
            >
              {filteredRepositories.length === 0 ? (
                <option value="">Nessun repository disponibile</option>
              ) : (
                filteredRepositories.map((repo) => (
                  <option key={repo.id} value={repo.cloneUrl}>
                    {repo.fullName}{repo.private ? " (privato)" : ""}
                  </option>
                ))
              )}
            </select>
            {selectedRepository ? (
              <div style={{ color: tc.textSecondary, fontSize: 12 }}>
                Branch default: {selectedRepository.defaultBranch} · aggiornato:{" "}
                {new Date(selectedRepository.updatedAt).toLocaleString()}
              </div>
            ) : null}
            {cloneTargetExists === true && selectedCloneUrl && (
              <div style={{ color: tc.warning, fontSize: 12 }}>
                ⚠ Directory già esistente — il repository è già stato clonato.
              </div>
            )}
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
              <button
                disabled={!canCloneSelected}
                onClick={() => void onCloneSelectedRepository()}
                style={smallButtonStyle(tc, !canCloneSelected)}
              >
                Clona repository nel progetto
              </button>
            </div>
          </>
        ) : (
          <div style={{ color: tc.warning, fontSize: 12 }}>
            Collega GitHub per visualizzare i repository disponibili.
          </div>
        )}
        {githubLoading ? <div style={{ color: tc.textMuted }}>Aggiornamento stato GitHub...</div> : null}
        {githubMessage ? <div style={{ color: tc.success }}>{githubMessage}</div> : null}
        {githubError ? <div style={{ color: tc.error }}>{githubError}</div> : null}
      </div>
    </>
  );
}
