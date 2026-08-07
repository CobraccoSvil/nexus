"use client";

import type {
  GitRepositoryState,
  UserProjectDetails,
} from "../../../lib/api-client";
import {
  commitGit,
  stageGitPaths,
  unstageGitPaths,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { buttonStyle, cardStyle, inputStyle, sectionTitleStyle } from "./styles";
import { useI18n } from "../../../lib/i18n";

interface RepositoryActionsCardProps {
  project: UserProjectDetails;
  git?: GitRepositoryState | null;
  busy: boolean;
  allChangedPaths: string[];
  commitMessage: string;
  setCommitMessage: (msg: string) => void;
  runAction: (action: () => Promise<unknown>) => Promise<void>;
}

export function RepositoryActionsCard({
  project,
  git,
  busy,
  allChangedPaths,
  commitMessage,
  setCommitMessage,
  runAction,
}: RepositoryActionsCardProps) {
  const { t } = useI18n();
  const tc = useThemeColors();

  return (
    <div style={cardStyle(tc)}>
      <div style={sectionTitleStyle(tc)}>{t("git.gestioneRepository")}</div>
      <div style={{ color: tc.textMuted, fontSize: 12 }}>
        Comandi rapidi per stage, commit e sincronizzazione del repository.
      </div>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          disabled={busy || !project.canManageGit || allChangedPaths.length === 0}
          onClick={() => runAction(() => stageGitPaths(project.id, allChangedPaths))}
          title={t("git.stageAllAggiungiTutte")}
          style={buttonStyle(tc, busy || !project.canManageGit || allChangedPaths.length === 0)}
        >
          {t("git.stageTutto")}
        </button>
        <button
          disabled={busy || !project.canManageGit || (git?.staged.length ?? 0) === 0}
          onClick={() => runAction(() => unstageGitPaths(project.id, git?.staged.map((item) => item.path) ?? []))}
          title={t("git.unstageAllRimuoviTutte")}
          style={buttonStyle(tc, busy || !project.canManageGit || (git?.staged.length ?? 0) === 0)}
        >
          {t("git.rimuoviStage")}
        </button>
      </div>

      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <input
          value={commitMessage}
          onChange={(event) => setCommitMessage(event.target.value)}
          placeholder={t("git.messaggioCommit")}
          style={inputStyle(tc)}
        />
        <button
          disabled={busy || !project.canManageGit || !commitMessage.trim()}
          onClick={() =>
            runAction(async () => {
              await commitGit(project.id, commitMessage.trim());
              setCommitMessage("");
            })
          }
          title={t("git.commitSalvaLeModifiche")}
          style={buttonStyle(tc, busy || !project.canManageGit || !commitMessage.trim())}
        >
          {t("git.commit")}
        </button>
      </div>
    </div>
  );
}
