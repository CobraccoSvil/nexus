"use client";

import type {
  GitHubAccountStatus,
  GitHubRemoteStatus,
  UserProjectDetails,
} from "../../../lib/api-client";
import {
  publishGitHubBranch,
  pullGit,
  pushGit,
} from "../../../lib/api-client";
import type { ReactNode } from "react";
import { useThemeColors, type Theme } from "../../../lib/theme";

/** Cella label + valore. Punto unico (regola L) per i 4 div ripetuti del grid. */
function KeyValueCell({ tc, label, children }: { tc: Theme; label: string; children: ReactNode }) {
  return (
    <div style={{ color: tc.textSecondary }}>
      {label}
      <div style={{ color: tc.text, marginTop: 4 }}>{children}</div>
    </div>
  );
}
import {
  cardStyle,
  inputStyle,
  linkButtonStyle,
  smallButtonStyle,
  statusBadgeStyle,
} from "./styles";
import { remoteReasonLabel } from "./labels";

interface RemoteCardProps {
  project: UserProjectDetails;
  githubStatus: GitHubRemoteStatus | null;
  githubAccount: GitHubAccountStatus | null;
  busy: boolean;
  githubBusy: boolean;
  githubLoading: boolean;
  githubMessage: string | null;
  githubError: string | null;
  remoteName?: string;
  branchName?: string;
  canManageGit: boolean;
  canPushPull: boolean;
  canPublishBranch: boolean;
  canCreatePr: boolean;
  runActionWithGitHubRefresh: (action: () => Promise<unknown>) => Promise<void>;
  // Create repo inline
  createRepoOpen: boolean;
  setCreateRepoOpen: (updater: boolean | ((v: boolean) => boolean)) => void;
  createRepoName: string;
  setCreateRepoName: (name: string) => void;
  createRepoDesc: string;
  setCreateRepoDesc: (desc: string) => void;
  createRepoPrivate: boolean;
  setCreateRepoPrivate: (priv: boolean) => void;
  createRepoBusy: boolean;
  onCreateGithubRepo: () => void;
  // Create PR
  prTitle: string;
  setPrTitle: (title: string) => void;
  prBody: string;
  setPrBody: (body: string) => void;
  prBaseBranch: string;
  setPrBaseBranch: (branch: string) => void;
  onCreatePullRequest: () => void;
}

export function RemoteCard({
  project,
  githubStatus,
  githubAccount,
  busy,
  githubBusy,
  githubLoading,
  githubMessage,
  githubError,
  remoteName,
  branchName,
  canManageGit,
  canPushPull,
  canPublishBranch,
  canCreatePr,
  runActionWithGitHubRefresh,
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
  prTitle,
  setPrTitle,
  prBody,
  setPrBody,
  prBaseBranch,
  setPrBaseBranch,
  onCreatePullRequest,
}: RemoteCardProps) {
  const tc = useThemeColors();

  return (
    <div style={cardStyle(tc)}>
      <div style={{ display: "flex", justifyContent: "space-between", gap: 12, flexWrap: "wrap" }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <div style={{ color: tc.text, fontWeight: 700 }}>Remote</div>
          <div style={{ color: tc.textSecondary }}>{remoteReasonLabel(githubStatus)}</div>
        </div>
        <span
          style={statusBadgeStyle(
            tc,
            githubStatus?.reason === "github_https"
              ? "success"
              : githubStatus?.reason === "ssh_remote_unsupported"
                ? "warning"
                : githubStatus?.reason === "non_github_remote"
                  ? "neutral"
                  : "error",
          )}
        >
          {githubStatus?.reason ?? "loading"}
        </span>
      </div>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))",
          gap: 8,
        }}
      >
        <KeyValueCell tc={tc} label="Repo GitHub">
          {/* Fix M12: messaggio coerente con reason invece di "Non disponibile" generico */}
          {githubStatus?.repoFullName
            ?? githubStatus?.remoteUrl
            ?? (githubStatus?.reason === "missing_origin_remote"
              ? "Nessun remote configurato"
              : githubStatus?.reason === "not_git_repo"
              ? "Progetto non e' un repo Git"
              : "Non disponibile")}
        </KeyValueCell>
        <KeyValueCell tc={tc} label="Branch">
          {branchName ?? "Non disponibile"}
        </KeyValueCell>
        <KeyValueCell tc={tc} label="Upstream">
          {githubStatus?.upstream ?? "Non configurato"}
        </KeyValueCell>
        <KeyValueCell tc={tc} label="Sync">
          ahead {githubStatus?.ahead ?? 0} · behind {githubStatus?.behind ?? 0}
        </KeyValueCell>
      </div>

      {githubStatus?.apiError ? (
        <div
          style={{
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            padding: "8px 10px",
            color: tc.warning,
            background: tc.bgInput,
          }}
        >
          GitHub API: {githubStatus.apiError}
        </div>
      ) : null}

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          disabled={busy || githubBusy || !canPushPull}
          onClick={() =>
            void runActionWithGitHubRefresh(() =>
              pullGit(project.id, remoteName, branchName),
            )
          }
          style={smallButtonStyle(tc, busy || githubBusy || !canPushPull)}
          title="Pull dal remote corrente"
        >
          Pull
        </button>
        <button
          disabled={busy || githubBusy || !canPushPull}
          onClick={() =>
            void runActionWithGitHubRefresh(() =>
              pushGit(project.id, remoteName, branchName),
            )
          }
          style={smallButtonStyle(tc, busy || githubBusy || !canPushPull)}
          title="Push verso il remote corrente"
        >
          Push
        </button>
        {githubStatus?.reason === "github_https" ? (
          <button
            disabled={busy || githubBusy || !canPublishBranch}
            onClick={() =>
              void runActionWithGitHubRefresh(() =>
                publishGitHubBranch(project.id),
              )
            }
            style={smallButtonStyle(tc, busy || githubBusy || !canPublishBranch)}
          >
            Publish Branch
          </button>
        ) : null}
        {githubStatus?.pullRequest ? (
          <a
            href={githubStatus.pullRequest.htmlUrl}
            target="_blank"
            rel="noreferrer"
            style={linkButtonStyle(tc)}
          >
            Open PR
          </a>
        ) : null}
        {githubStatus?.reason === "missing_origin_remote" &&
        githubAccount?.connected === true &&
        canManageGit ? (
          <button
            disabled={createRepoBusy || githubBusy || busy}
            onClick={() => {
              setCreateRepoOpen((v) => !v);
              if (!createRepoName) setCreateRepoName(project.slug ?? project.name ?? "");
            }}
            style={smallButtonStyle(tc, createRepoBusy || githubBusy || busy)}
            title="Crea un nuovo repository GitHub e collega come origin"
          >
            Crea repo su GitHub
          </button>
        ) : null}
      </div>

      {createRepoOpen ? (
        <div
          style={{
            borderTop: `1px solid ${tc.border}`,
            paddingTop: 10,
            display: "flex",
            flexDirection: "column",
            gap: 8,
          }}
        >
          <div style={{ color: tc.text, fontWeight: 700 }}>Crea repository GitHub</div>
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
              {createRepoBusy ? "Creazione..." : "Crea repo"}
            </button>
            <button
              disabled={createRepoBusy}
              onClick={() => setCreateRepoOpen(false)}
              style={smallButtonStyle(tc, createRepoBusy)}
            >
              Annulla
            </button>
          </div>
        </div>
      ) : null}

      {canCreatePr ? (
        <div
          style={{
            borderTop: `1px solid ${tc.border}`,
            paddingTop: 10,
            display: "flex",
            flexDirection: "column",
            gap: 8,
          }}
        >
          <div style={{ color: tc.text, fontWeight: 700 }}>Create PR</div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <input
              value={prTitle}
              onChange={(event) => setPrTitle(event.target.value)}
              placeholder="Titolo pull request"
              style={inputStyle(tc)}
            />
            <input
              value={prBaseBranch}
              onChange={(event) => setPrBaseBranch(event.target.value)}
              placeholder={githubStatus?.defaultBranch ?? "Branch base"}
              style={{ ...inputStyle(tc), maxWidth: 180 }}
            />
          </div>
          <textarea
            value={prBody}
            onChange={(event) => setPrBody(event.target.value)}
            placeholder="Descrizione opzionale"
            rows={4}
            style={{
              ...inputStyle(tc),
              minHeight: 92,
              resize: "vertical",
              fontFamily: "inherit",
            }}
          />
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              disabled={githubBusy || busy || !prTitle.trim()}
              onClick={() => void onCreatePullRequest()}
              style={smallButtonStyle(tc, githubBusy || busy || !prTitle.trim())}
            >
              Create PR
            </button>
            {githubStatus?.defaultBranch ? (
              <div style={{ color: tc.textMuted, fontSize: 11, alignSelf: "center" }}>
                Base di default: {githubStatus.defaultBranch}
              </div>
            ) : null}
          </div>
        </div>
      ) : null}

      {githubLoading ? <div style={{ color: tc.textMuted }}>Aggiornamento stato GitHub...</div> : null}
      {githubMessage ? <div style={{ color: tc.success }}>{githubMessage}</div> : null}
      {githubError ? <div style={{ color: tc.error }}>{githubError}</div> : null}
    </div>
  );
}
