"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  analyzeProject,
  analyzeProjectDeep,
  pollProjectInsightsUntilDone,
  getProjectInsights,
  type DeepAnalysisInsights,
  checkCloneTargetExists,
  cloneProjectGitHubRepository,
  connectGitHub,
  createGitHubPullRequest,
  disconnectGitHub,
  getGitBranches,
  getGitHubAccount,
  getGitLog,
  getProjectGitHubStatus,
  listProjectGitHubRepositories,
  type GitBranchInfo,
  type GitHubAccountStatus,
  type GitHubRepositorySummary,
  type GitHubRemoteStatus,
  type GitLogEntry,
  type GitRepositoryState,
  publishProjectToGitHub,
  type UserProjectDetails,
} from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import { StagingArea } from "./staging-area";
import { CommitLog } from "./commit-log";
import { BranchManager } from "./branch-manager";
import { cardStyle, sectionTitleStyle } from "./source-control/styles";
import { NexusStatusCard } from "./source-control/nexus-status-card";
import { AnalysisInsightsCard } from "./source-control/analysis-insights-card";
import { GitHubAccountCard } from "./source-control/github-account-card";
import { GitHubImportSection } from "./source-control/github-import-section";
import { RemoteCard } from "./source-control/remote-card";
import { RepositoryActionsCard } from "./source-control/repository-actions-card";
import { useI18n } from "../../lib/i18n";

export function SourceControlPanel({
  project,
  git,
  onRefresh,
  onOpenFileAtLine,
  onProjectAnalyzed,
  onSendToChat,
}: {
  project?: UserProjectDetails | null;
  git?: GitRepositoryState | null;
  onRefresh: () => Promise<void>;
  onOpenFileAtLine?: (path: string, line: number) => Promise<void>;
  onProjectAnalyzed?: () => void;
  onSendToChat?: (msg: string) => void;
}) {
  const { t } = useI18n();
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();
  const projectId = project?.id;
  const projectIsGitRepo = project?.isGitRepo ?? false;
  const runtimeIsGitRepo = projectIsGitRepo || (git?.isGitRepo ?? false);
  const [commitMessage, setCommitMessage] = useState("");
  const [branches, setBranches] = useState<GitBranchInfo[]>([]);
  const [logEntries, setLogEntries] = useState<GitLogEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [githubAccount, setGitHubAccount] = useState<GitHubAccountStatus | null>(null);
  const [githubStatus, setGitHubStatus] = useState<GitHubRemoteStatus | null>(null);
  const [githubBusy, setGitHubBusy] = useState(false);
  const [githubLoading, setGitHubLoading] = useState(false);
  const [githubError, setGitHubError] = useState<string | null>(null);
  const [githubMessage, setGitHubMessage] = useState<string | null>(null);
  const [githubRepositories, setGitHubRepositories] = useState<GitHubRepositorySummary[]>([]);
  const [repoQuery, setRepoQuery] = useState("");
  const [selectedCloneUrl, setSelectedCloneUrl] = useState("");
  const [cloneTargetExists, setCloneTargetExists] = useState<boolean | null>(null);
  const [prTitle, setPrTitle] = useState("");
  const [prBody, setPrBody] = useState("");
  const [prBaseBranch, setPrBaseBranch] = useState("");
  // Fix M15: dialog inline "Crea repo GitHub"
  const [createRepoOpen, setCreateRepoOpen] = useState(false);
  const [createRepoName, setCreateRepoName] = useState("");
  const [createRepoPrivate, setCreateRepoPrivate] = useState(true);
  const [createRepoDesc, setCreateRepoDesc] = useState("");
  const [createRepoBusy, setCreateRepoBusy] = useState(false);
  const [analyzeBusy, setAnalyzeBusy] = useState(false);
  const [deepAnalysisPhase, setDeepAnalysisPhase] = useState<"idle" | "static" | "deep">("idle");
  const [insights, setInsights] = useState<DeepAnalysisInsights | null>(null);
  const [insightsModel, setInsightsModel] = useState<string | null>(null);
  const [insightsAt, setInsightsAt] = useState<string | null>(null);
  // Set degli indici delle issue/azioni gia' inviate alla chat tramite i
  // pulsanti "Risolvi con Nexus" / "▶ Esegui con Nexus". Servono per:
  //  1) Disabilitare il pulsante dopo il click → evita doppio invio.
  //  2) Mostrare visivamente che l'azione e' gia' partita (label diverso).
  // Il set viene resettato ogni volta che `insights` cambia (re-analisi),
  // perche' la corrispondenza per indice non e' piu' valida con un
  // report nuovo. Vedi useEffect piu' sotto.
  const [sentIssueIds, setSentIssueIds] = useState<Set<number>>(new Set());
  const [sentActionIds, setSentActionIds] = useState<Set<number>>(new Set());

  // Check if the target clone directory already exists whenever the selected repo changes
  useEffect(() => {
    if (!selectedCloneUrl) {
      setCloneTargetExists(null);
      return;
    }
    const repoName = selectedCloneUrl
      .replace(/\.git$/, "")
      .split("/")
      .pop() ?? "";
    if (!repoName) {
      setCloneTargetExists(null);
      return;
    }
    let cancelled = false;
    checkCloneTargetExists(repoName).then((res) => {
      if (!cancelled) setCloneTargetExists(res.exists);
    }).catch(() => {
      if (!cancelled) setCloneTargetExists(null);
    });
    return () => { cancelled = true; };
  }, [selectedCloneUrl]);

  const loadGitContext = useCallback(async () => {
    if (!projectId || !runtimeIsGitRepo) {
      setBranches([]);
      setLogEntries([]);
      return;
    }

    try {
      const [branchRes, logRes] = await Promise.all([
        getGitBranches(projectId),
        getGitLog(projectId),
      ]);
      setBranches(branchRes.branches);
      setLogEntries(logRes.entries);
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "Errore Git");
    }
  }, [projectId, runtimeIsGitRepo]);

  const loadGitHubState = useCallback(
    async (resetForm: boolean) => {
      if (!projectId) {
        setGitHubAccount(null);
        setGitHubStatus(null);
        setGitHubError(null);
        setGitHubMessage(null);
        setGitHubRepositories([]);
        setSelectedCloneUrl("");
        setRepoQuery("");
        return;
      }

      try {
        setGitHubLoading(true);
        setGitHubError(null);
        const accountRes = await getGitHubAccount();
        setGitHubAccount(accountRes.account);
        if (runtimeIsGitRepo) {
          const statusRes = await getProjectGitHubStatus(projectId);
          setGitHubStatus(statusRes.github);
          if (resetForm) {
            setPrTitle(statusRes.github.suggestedPrTitle ?? "");
            setPrBody("");
            setPrBaseBranch(statusRes.github.defaultBranch ?? "");
          }
        } else {
          setGitHubStatus(null);
          if (resetForm) {
            setPrTitle("");
            setPrBody("");
            setPrBaseBranch("");
          }
        }
      } catch (nextError) {
        setGitHubError(
          nextError instanceof Error ? nextError.message : "Errore nel caricamento di GitHub",
        );
      } finally {
        setGitHubLoading(false);
      }
    },
    [projectId, runtimeIsGitRepo],
  );

  const loadGitHubRepositories = useCallback(async () => {
    if (!projectId || !githubAccount?.connected) {
      setGitHubRepositories([]);
      setSelectedCloneUrl("");
      return;
    }
    try {
      const response = await listProjectGitHubRepositories(projectId);
      setGitHubRepositories(response.repositories);
      if (response.repositories.length === 0) {
        setSelectedCloneUrl("");
        return;
      }
      setSelectedCloneUrl((current) => {
        if (current && response.repositories.some((repo) => repo.cloneUrl === current)) {
          return current;
        }
        return response.repositories[0].cloneUrl;
      });
    } catch (nextError) {
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile caricare i repository GitHub",
      );
    }
  }, [githubAccount?.connected, projectId]);

  useEffect(() => {
    void loadGitContext();
    void loadGitHubState(true);
  }, [loadGitContext, loadGitHubState]);

  useEffect(() => {
    void loadGitHubRepositories();
  }, [loadGitHubRepositories]);

  const allChangedPaths = useMemo(
    () => [
      ...(git?.staged ?? []).map((item) => item.path),
      ...(git?.unstaged ?? []).map((item) => item.path),
      ...(git?.untracked ?? []).map((item) => item.path),
    ],
    [git],
  );
  const filteredRepositories = useMemo(() => {
    const query = repoQuery.trim().toLowerCase();
    if (!query) return githubRepositories;
    return githubRepositories.filter((repo) => {
      const haystack = `${repo.fullName} ${repo.name} ${repo.ownerLogin}`.toLowerCase();
      return haystack.includes(query);
    });
  }, [githubRepositories, repoQuery]);

  const runAction = async (action: () => Promise<unknown>) => {
    try {
      setBusy(true);
      setError(null);
      await action();
      await onRefresh();
      await loadGitContext();
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "Operazione Git fallita");
    } finally {
      setBusy(false);
    }
  };

  const runActionWithGitHubRefresh = async (action: () => Promise<unknown>) => {
    await runAction(action);
    await loadGitHubState(false);
  };

  if (!project) {
    return <div style={{ color: tc.textMuted }}>{t("git.apriUnProgettoPer")}</div>;
  }

  const isGitRepo = runtimeIsGitRepo;
  const remoteName = githubStatus?.remoteName;
  const branchName = githubStatus?.branch;
  const canManageGit = project.canManageGit;
  const canPushPull =
    canManageGit &&
    Boolean(remoteName) &&
    (githubStatus?.reason !== "github_https" || githubAccount?.connected === true);
  const canPublishBranch =
    canManageGit &&
    githubStatus?.reason === "github_https" &&
    githubAccount?.connected === true &&
    !githubStatus.published &&
    Boolean(branchName);
  const canCreatePr =
    canManageGit &&
    githubStatus?.reason === "github_https" &&
    githubAccount?.connected === true &&
    githubStatus.published &&
    !githubStatus.pullRequest;
  const connectLabel =
    githubAccount?.status === "upgrade_required"
      ? "Aggiorna permessi"
      : githubAccount?.status === "reconnect_required"
        ? "Riconnetti GitHub"
        : "Connetti GitHub";
  const selectedRepository = githubRepositories.find((repo) => repo.cloneUrl === selectedCloneUrl);
  const canCloneSelected =
    !isGitRepo &&
    project.canManageGit &&
    githubAccount?.connected === true &&
    !!selectedCloneUrl &&
    cloneTargetExists !== true &&
    !busy &&
    !githubBusy;
  const isNexusReady = Boolean(project.nexusReady);

  const handleGitHubConnect = async () => {
    try {
      setGitHubBusy(true);
      setGitHubError(null);
      const returnTo =
        typeof window === "undefined"
          ? "/"
          : `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const response = await connectGitHub(returnTo);
      window.location.href = response.url;
    } catch (nextError) {
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile avviare l'autorizzazione GitHub",
      );
      setGitHubBusy(false);
    }
  };

  const handleGitHubDisconnect = async () => {
    try {
      setGitHubBusy(true);
      setGitHubError(null);
      setGitHubMessage(null);
      await disconnectGitHub();
      await loadGitHubState(true);
    } catch (nextError) {
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile scollegare GitHub",
      );
    } finally {
      setGitHubBusy(false);
    }
  };

  const handleCreatePullRequest = async () => {
    if (!project?.id) return;
    try {
      setGitHubBusy(true);
      setGitHubError(null);
      setGitHubMessage(null);
      const response = await createGitHubPullRequest(project.id, {
        title: prTitle.trim(),
        body: prBody.trim() || undefined,
        baseBranch: prBaseBranch.trim() || undefined,
      });
      setGitHubMessage(
        response.created
          ? `Pull request #${response.pullRequest.number} creata con successo`
          : `Pull request #${response.pullRequest.number} gia esistente`,
      );
      await loadGitHubState(false);
      if (typeof window !== "undefined") {
        window.open(response.pullRequest.htmlUrl, "_blank", "noopener,noreferrer");
      }
    } catch (nextError) {
      if (nextError instanceof Error && nextError.name === "AbortError") return;
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile creare la pull request",
      );
    } finally {
      setGitHubBusy(false);
    }
  };

  // Pubblica progetto su GitHub: orchestra init + commit + crea repo + push.
  // Endpoint backend: github::github_publish_project (idempotente: salta i
  // passi gia' fatti). Vedere github.rs.
  const handleCreateGithubRepo = async () => {
    if (!project?.id) return;
    const name = createRepoName.trim();
    if (!name) {
      setGitHubError("Specifica il nome del repository");
      return;
    }
    try {
      setCreateRepoBusy(true);
      setGitHubError(null);
      setGitHubMessage(null);
      const data = await publishProjectToGitHub(project.id, {
        name,
        description: createRepoDesc.trim() || undefined,
        private: createRepoPrivate,
      });
      const pushedMsg = data.pushed ? " e push completato" : " (push fallito — verifica manualmente)";
      setGitHubMessage(`Repo ${data.full_name ?? name} creato${pushedMsg}`);
      setCreateRepoOpen(false);
      setCreateRepoName("");
      setCreateRepoDesc("");
      await loadGitHubState(false);
      if (data.html_url && typeof window !== "undefined") {
        window.open(data.html_url, "_blank", "noopener,noreferrer");
      }
    } catch (nextError) {
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile pubblicare il progetto",
      );
    } finally {
      setCreateRepoBusy(false);
    }
  };

  const handleCloneSelectedRepository = async () => {
    if (!project?.id || !selectedCloneUrl) return;
    try {
      setGitHubBusy(true);
      setGitHubError(null);
      setGitHubMessage(null);
      const response = await cloneProjectGitHubRepository(project.id, {
        cloneUrl: selectedCloneUrl,
      });

      // If the backend auto-created a new project (current dir was not empty),
      // navigate to the new project.
      if ("project" in response && (response as { project?: { id?: string } }).project?.id) {
        const newId = (response as { project: { id: string } }).project.id;
        window.location.href = `/?project=${newId}`;
        return;
      }

      setGitHubMessage(
        `Repository ${response.repository.owner}/${response.repository.repo} clonato nel progetto corrente`,
      );
      await onRefresh();
      await loadGitContext();
      await loadGitHubState(true);
      // Update clone target check
      setCloneTargetExists(true);
      const wantsAnalyzeNow = await confirmDialog(
        "Repository clonato con successo. Vuoi analizzare ora il progetto per renderlo pronto in Nexus?",
        "Analizza progetto",
      );
      if (wantsAnalyzeNow) {
        await handleAnalyzeProject();
      } else {
        setGitHubMessage(
          "Repository clonato. Analisi rimandata: puoi avviarla in qualsiasi momento dal pannello Git.",
        );
      }
    } catch (nextError) {
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile clonare il repository selezionato",
      );
    } finally {
      setGitHubBusy(false);
    }
  };

  const handleAnalyzeProject = async () => {
    if (!project?.id) return;
    try {
      setAnalyzeBusy(true);
      setGitHubError(null);
      setGitHubMessage(null);

      // Fase 1: analisi statica (linguaggi, framework, deps)
      setDeepAnalysisPhase("static");
      const analysis = await analyzeProject(project.id);
      await onRefresh();
      await loadGitContext();
      await loadGitHubState(false);

      // Fase 2: analisi profonda con agente AI (legge config, individua incoerenze)
      // Cancella subito la vecchia analisi: la card sparisce e viene rimpiazzata
      // dal placeholder "Analisi AI in corso..." finche' non arriva la nuova.
      // Evita che l'utente veda dati stantii mentre sta lavorando il backend.
      setInsights(null);
      setInsightsModel(null);
      setInsightsAt(null);
      // Re-analisi: azzera lo storico dei click sui pulsanti — gli indici
      // del nuovo report non corrispondono piu' a quelli precedenti.
      setSentIssueIds(new Set());
      setSentActionIds(new Set());
      setDeepAnalysisPhase("deep");
      let deepStatus: "completed" | "partial" | "failed" | "skipped" = "skipped";
      let deepIssuesCount = 0;
      try {
        // Refactor 0102: deep-analyze ora e' asincrono.
        // POST ritorna 202 immediato con run_id; serve poi pollare GET /insights.
        const startResp = await analyzeProjectDeep(project.id);
        console.info(`Deep analysis started: run_id=${startResp.run_id}`);
        // Polling: GET /insights ogni 3s, max 6 minuti
        const final = await pollProjectInsightsUntilDone(project.id, 3000, 120);
        // Mappa lo status del DB ai 4 valori attesi: completed | partial | failed | skipped
        if (final.status === "completed") {
          deepStatus = "completed";
        } else if (final.status === "failed") {
          deepStatus = "failed";
        } else {
          // 'running' (timeout polling) o altro
          deepStatus = "skipped";
        }
        if (final.insights) {
          setInsights(final.insights);
          setInsightsModel(final.model_used ?? null);
          setInsightsAt(final.created_at ?? new Date().toISOString());
          deepIssuesCount = final.insights.config_issues?.length ?? 0;
        }
      } catch (deepErr) {
        console.warn("Deep analysis failed:", deepErr);
        deepStatus = "failed";
      }

      const vector = analysis.vectorIndex;
      const issuesCount = deepIssuesCount;
      let baseMsg: string;
      if (vector?.status === "indexed") {
        baseMsg = `Analisi statica completata (${vector.indexedPoints ?? 0} punti vector).`;
      } else if (vector?.status === "partial") {
        baseMsg = `Analisi statica completata. Vector parziale (${vector.indexedPoints ?? 0} ok).`;
      } else {
        baseMsg = "Analisi statica completata.";
      }
      if (deepStatus === "completed") {
        const issMsg = issuesCount > 0 ? ` ${issuesCount} incoerenze rilevate.` : " nessuna incoerenza rilevata.";
        setGitHubMessage(`${baseMsg} Analisi profonda AI ok.${issMsg}`);
      } else if (deepStatus === "failed") {
        setGitHubMessage(`${baseMsg} Analisi profonda AI fallita (provider non disponibili).`);
      } else {
        setGitHubMessage(baseMsg);
      }
      onProjectAnalyzed?.();
    } catch (nextError) {
      setGitHubError(
        nextError instanceof Error ? nextError.message : "Impossibile analizzare il progetto",
      );
    } finally {
      setAnalyzeBusy(false);
      setDeepAnalysisPhase("idle");
    }
  };

  // Carica insights esistenti al mount/cambio progetto
  useEffect(() => {
    if (!project?.id) return;
    let cancelled = false;
    void (async () => {
      try {
        const r = await getProjectInsights(project.id);
        if (cancelled) return;
        if (r.exists && r.insights) {
          setInsights(r.insights);
          setInsightsModel(r.model_used ?? null);
          setInsightsAt(r.created_at ?? null);
        } else {
          setInsights(null);
          setInsightsModel(null);
          setInsightsAt(null);
        }
      } catch { /* ignora */ }
    })();
    return () => { cancelled = true; };
  }, [project?.id]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8, fontSize: 12, width: "100%", minWidth: 0 }}>
      <NexusStatusCard
        project={project}
        isNexusReady={isNexusReady}
        busy={busy}
        githubBusy={githubBusy}
        analyzeBusy={analyzeBusy}
        deepAnalysisPhase={deepAnalysisPhase}
        onAnalyzeProject={handleAnalyzeProject}
      />

      <AnalysisInsightsCard
        insights={insights}
        insightsModel={insightsModel}
        insightsAt={insightsAt}
        analyzeBusy={analyzeBusy}
        deepAnalysisPhase={deepAnalysisPhase}
        onSendToChat={onSendToChat}
        sentIssueIds={sentIssueIds}
        sentActionIds={sentActionIds}
        setSentIssueIds={setSentIssueIds}
        setSentActionIds={setSentActionIds}
      />

      <GitHubAccountCard
        githubAccount={githubAccount}
        busy={busy}
        githubBusy={githubBusy}
        connectLabel={connectLabel}
        onConnect={handleGitHubConnect}
        onDisconnect={handleGitHubDisconnect}
      />

      {!isGitRepo ? (
        <GitHubImportSection
          project={project}
          githubAccount={githubAccount}
          githubBusy={githubBusy}
          busy={busy}
          githubLoading={githubLoading}
          githubMessage={githubMessage}
          githubError={githubError}
          createRepoOpen={createRepoOpen}
          setCreateRepoOpen={setCreateRepoOpen}
          createRepoName={createRepoName}
          setCreateRepoName={setCreateRepoName}
          createRepoDesc={createRepoDesc}
          setCreateRepoDesc={setCreateRepoDesc}
          createRepoPrivate={createRepoPrivate}
          setCreateRepoPrivate={setCreateRepoPrivate}
          createRepoBusy={createRepoBusy}
          onCreateGithubRepo={handleCreateGithubRepo}
          repoQuery={repoQuery}
          setRepoQuery={setRepoQuery}
          selectedCloneUrl={selectedCloneUrl}
          setSelectedCloneUrl={setSelectedCloneUrl}
          filteredRepositories={filteredRepositories}
          selectedRepository={selectedRepository}
          cloneTargetExists={cloneTargetExists}
          canCloneSelected={canCloneSelected}
          onLoadRepositories={loadGitHubRepositories}
          onCloneSelectedRepository={handleCloneSelectedRepository}
        />
      ) : (
        <>
          <RemoteCard
            project={project}
            githubStatus={githubStatus}
            githubAccount={githubAccount}
            busy={busy}
            githubBusy={githubBusy}
            githubLoading={githubLoading}
            githubMessage={githubMessage}
            githubError={githubError}
            remoteName={remoteName}
            branchName={branchName}
            canManageGit={canManageGit}
            canPushPull={canPushPull}
            canPublishBranch={canPublishBranch}
            canCreatePr={canCreatePr}
            runActionWithGitHubRefresh={runActionWithGitHubRefresh}
            createRepoOpen={createRepoOpen}
            setCreateRepoOpen={setCreateRepoOpen}
            createRepoName={createRepoName}
            setCreateRepoName={setCreateRepoName}
            createRepoDesc={createRepoDesc}
            setCreateRepoDesc={setCreateRepoDesc}
            createRepoPrivate={createRepoPrivate}
            setCreateRepoPrivate={setCreateRepoPrivate}
            createRepoBusy={createRepoBusy}
            onCreateGithubRepo={handleCreateGithubRepo}
            prTitle={prTitle}
            setPrTitle={setPrTitle}
            prBody={prBody}
            setPrBody={setPrBody}
            prBaseBranch={prBaseBranch}
            setPrBaseBranch={setPrBaseBranch}
            onCreatePullRequest={handleCreatePullRequest}
          />

          <RepositoryActionsCard
            project={project}
            git={git}
            busy={busy}
            allChangedPaths={allChangedPaths}
            commitMessage={commitMessage}
            setCommitMessage={setCommitMessage}
            runAction={runAction}
          />

          <div style={cardStyle(tc)}>
            <div style={sectionTitleStyle(tc)}>{t("git.branch")}</div>
            <div style={{ color: tc.textMuted, fontSize: 12 }}>
              {t("git.creaUnBranchNuovo")}
            </div>
            <BranchManager
              project={project}
              branches={branches}
              busy={busy}
              runAction={runActionWithGitHubRefresh}
            />
          </div>

          <div style={cardStyle(tc)}>
            <div style={sectionTitleStyle(tc)}>{t("git.staging")}</div>
            <div style={{ color: tc.textMuted, fontSize: 12 }}>
              {t("git.controllaFileStagedUnstaged")}
            </div>
            <StagingArea
              project={project}
              staged={git?.staged ?? []}
              unstaged={git?.unstaged ?? []}
              untracked={git?.untracked ?? []}
              busy={busy}
              runAction={runAction}
              onOpenFileAtLine={onOpenFileAtLine}
            />
          </div>

          <div style={cardStyle(tc)}>
            <div style={sectionTitleStyle(tc)}>{t("git.cronologiaCommit")}</div>
            <CommitLog logEntries={logEntries} />
          </div>
        </>
      )}

      {error && <div style={{ color: tc.error }}>{error}</div>}
    </div>
  );
}
