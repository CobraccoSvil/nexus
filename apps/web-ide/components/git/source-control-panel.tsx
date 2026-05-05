"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  analyzeProject,
  analyzeProjectDeep,
  pollProjectInsightsUntilDone,
  getProjectInsights,
  type DeepAnalysisInsights,
  commitGit,
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
  pullGit,
  publishGitHubBranch,
  pushGit,
  stageGitPaths,
  type UserProjectDetails,
  unstageGitPaths,
} from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import { StagingArea } from "./staging-area";
import { CommitLog } from "./commit-log";
import { BranchManager } from "./branch-manager";

function buttonStyle(tc: ReturnType<typeof useThemeColors>, disabled: boolean) {
  return {
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: disabled ? tc.bgCard : tc.accentBg,
    color: tc.text,
    cursor: disabled ? "not-allowed" : "pointer",
    flexShrink: 0,
    whiteSpace: "nowrap" as const,
  };
}

function inputStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    flex: 1,
    minWidth: 0,
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    boxSizing: "border-box" as const,
    width: "100%",
  };
}

function sectionTitleStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    color: tc.text,
    fontSize: 12,
    fontWeight: 700,
    textTransform: "uppercase" as const,
    letterSpacing: "0.04em",
  };
}

function cardStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    background: tc.bgCard,
    padding: "8px 10px",
    display: "flex",
    flexDirection: "column",
    gap: 6,
    minWidth: 0,
    width: "100%",
    overflow: "hidden",
    boxSizing: "border-box",
  } as const;
}

function smallButtonStyle(tc: ReturnType<typeof useThemeColors>, disabled: boolean) {
  return {
    ...buttonStyle(tc, disabled),
    padding: "6px 10px",
    fontSize: 12,
    fontWeight: 600,
  } as const;
}

function statusBadgeStyle(
  tc: ReturnType<typeof useThemeColors>,
  tone: "neutral" | "success" | "warning" | "error",
) {
  const colors = {
    neutral: { color: tc.textSecondary, background: tc.bgInput },
    success: { color: tc.success, background: tc.accentBg },
    warning: { color: tc.warning, background: tc.bgInput },
    error: { color: tc.error, background: tc.bgInput },
  }[tone];

  return {
    display: "inline-flex",
    alignItems: "center",
    gap: 6,
    borderRadius: 999,
    padding: "4px 8px",
    fontSize: 11,
    fontWeight: 700,
    background: colors.background,
    color: colors.color,
    border: `1px solid ${tc.border}`,
  } as const;
}

function linkButtonStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    ...buttonStyle(tc, false),
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    textDecoration: "none",
    fontSize: 12,
    fontWeight: 600,
  } as const;
}

function accountTone(status?: GitHubAccountStatus["status"]) {
  if (status === "connected") return "success";
  if (status === "upgrade_required") return "warning";
  if (status === "reconnect_required") return "error";
  return "neutral";
}

function accountLabel(account?: GitHubAccountStatus | null) {
  if (!account) return "Caricamento stato GitHub...";
  if (account.status === "connected") {
    return `Connesso a GitHub come ${account.username ?? "account GitHub"}`;
  }
  if (account.status === "upgrade_required") {
    return `Permessi GitHub da aggiornare${account.username ? ` per ${account.username}` : ""}`;
  }
  if (account.status === "reconnect_required") {
    return `Connessione GitHub da riconfermare${account.username ? ` per ${account.username}` : ""}`;
  }
  return "Connetti GitHub per usare publish branch e pull request";
}

function remoteReasonLabel(status?: GitHubRemoteStatus | null) {
  if (!status) return "Caricamento stato remote...";
  if (status.reason === "github_https") return "Remote GitHub HTTPS rilevato";
  if (status.reason === "non_github_remote") return "Remote non GitHub: restano disponibili solo operazioni Git locali";
  if (status.reason === "ssh_remote_unsupported") {
    return "Remote SSH rilevato: publish branch e pull request GitHub non sono supportati in v1";
  }
  if (status.reason === "missing_origin_remote") return "Nessun remote origin configurato";
  if (status.reason === "not_git_repo") return "Il progetto non e' un repository Git";
  return "Stato remote non disponibile";
}

function readinessTone(isReady: boolean) {
  return isReady ? "success" : "warning";
}

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
    return <div style={{ color: tc.textMuted }}>Apri un progetto per usare Source Control.</div>;
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
      <div style={cardStyle(tc)}>
        <div style={{ display: "flex", justifyContent: "space-between", gap: 12, flexWrap: "wrap" }}>
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <div style={{ color: tc.text, fontWeight: 700 }}>Stato Progetto Nexus</div>
            <div style={{ color: tc.textSecondary }}>
              {isNexusReady
                ? "Nexus Ready: progetto analizzato e pronto alla gestione AI."
                : "Analisi richiesta: progetto non inizializzato per Nexus."}
            </div>
            <div style={{ color: tc.textMuted, fontSize: 12 }}>
              Puoi inizializzare il progetto in qualsiasi momento.
            </div>
            {project.analyzedAt ? (
              <div style={{ color: tc.textMuted, fontSize: 11 }}>
                Ultima analisi: {new Date(project.analyzedAt).toLocaleString()}
              </div>
            ) : null}
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <span style={statusBadgeStyle(tc, readinessTone(isNexusReady))}>
              {isNexusReady ? "Nexus Ready" : "Analisi richiesta"}
            </span>
            <button
              disabled={busy || githubBusy || analyzeBusy}
              onClick={() => void handleAnalyzeProject()}
              style={smallButtonStyle(tc, busy || githubBusy || analyzeBusy)}
            >
              {analyzeBusy
                ? (deepAnalysisPhase === "deep" ? "Analisi AI..." : "Analisi statica...")
                : isNexusReady ? "Rianalizza progetto" : "Analizza progetto"}
            </button>
          </div>
        </div>
      </div>

      {/* Placeholder mentre l'analisi profonda e' in corso: la vecchia card e'
          gia' stata svuotata da handleAnalyzeProject; mostriamo uno stato
          esplicito cosi' l'utente capisce che il sistema sta lavorando. */}
      {!insights && analyzeBusy && deepAnalysisPhase === "deep" && (
        <div style={{ ...cardStyle(tc), minWidth: 0, overflow: "hidden" }}>
          {/* keyframes inline: nessun foglio CSS globale modificato */}
          <style>{`@keyframes nx-pulse-dot {
            0%, 100% { opacity: 1; transform: scale(1); }
            50%      { opacity: 0.35; transform: scale(0.7); }
          }`}</style>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span style={{
              width: 10, height: 10, borderRadius: "50%",
              background: "#60a5fa",
              animation: "nx-pulse-dot 1.4s ease-in-out infinite",
              flexShrink: 0,
            }} />
            <div style={{ color: tc.text, fontWeight: 600, fontSize: 12 }}>
              Analisi AI in corso...
            </div>
          </div>
          <div style={{ color: tc.textMuted, fontSize: 10, marginTop: 4, lineHeight: 1.4 }}>
            L&apos;agente sta leggendo i file di configurazione del progetto e
            valutando incoerenze, servizi rilevati e azioni consigliate.
            Tempo tipico: 30-60 secondi.
          </div>
        </div>
      )}

      {/* Card insights dell'agente agent.project.analyzer */}
      {insights && (
        <div style={{ ...cardStyle(tc), minWidth: 0, overflow: "hidden" }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 6, marginBottom: 6, flexWrap: "wrap" }}>
            <div style={{ color: tc.text, fontWeight: 700 }}>Analisi AI del progetto</div>
            <div style={{ color: tc.textMuted, fontSize: 10, wordBreak: "break-all" }}>
              {insightsModel ? `${insightsModel}` : ""}{insightsAt ? ` · ${new Date(insightsAt).toLocaleString()}` : ""}
            </div>
          </div>
          {insights.project_summary && (
            <div style={{
              color: tc.textSecondary, fontSize: 11, marginBottom: 8, lineHeight: 1.5,
              wordBreak: "break-word", overflowWrap: "anywhere",
            }}>
              {insights.project_summary}
            </div>
          )}
          {insights.architecture && (
            <div style={{
              fontSize: 10, color: tc.textMuted, marginBottom: 8,
              wordBreak: "break-word", overflowWrap: "anywhere",
            }}>
              <span style={{ fontWeight: 600 }}>Architettura:</span> {insights.architecture.pattern}
              {insights.architecture.primary_languages && insights.architecture.primary_languages.length > 0 &&
                ` · ${insights.architecture.primary_languages.join(", ")}`}
            </div>
          )}

          {/* Servizi rilevati con modalita' di esecuzione consigliata */}
          {insights.services && insights.services.length > 0 && insights.services.some(s => s.recommended_run_mode) && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, marginBottom: 4 }}>
                Servizi e modalita' di esecuzione
              </div>
              {insights.services.map((svc, idx) => {
                if (!svc.recommended_run_mode) return null;
                const modeColor = svc.recommended_run_mode === "native" ? "#22c55e"
                                : svc.recommended_run_mode === "docker" ? "#60a5fa"
                                : "#94a3b8";
                const modeLabel = svc.recommended_run_mode === "native" ? "nativo"
                                : svc.recommended_run_mode === "docker" ? "Docker"
                                : "scelta libera";
                return (
                  <div key={idx} style={{
                    border: `1px solid ${tc.border}`,
                    borderRadius: 3, padding: "5px 8px", marginBottom: 4, background: tc.bgCard,
                    minWidth: 0, maxWidth: "100%", overflow: "hidden",
                  }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
                      <span style={{
                        fontSize: 11, fontWeight: 600, color: tc.text,
                        wordBreak: "break-word", overflowWrap: "anywhere",
                      }}>{svc.name}</span>
                      {svc.port && (
                        <span style={{ fontSize: 9, color: tc.textMuted }}>:{svc.port}</span>
                      )}
                      <span style={{
                        fontSize: 9, color: modeColor,
                        background: `${modeColor}1c`,
                        border: `1px solid ${modeColor}55`,
                        borderRadius: 3, padding: "1px 5px",
                        fontFamily: '"JetBrains Mono", monospace',
                        whiteSpace: "nowrap",
                      }}>
                        consiglio: {modeLabel}
                      </span>
                    </div>
                    {svc.run_mode_rationale && (
                      <div style={{
                        fontSize: 10, color: tc.textSecondary, marginTop: 2, lineHeight: 1.4,
                        wordBreak: "break-word", overflowWrap: "anywhere",
                      }}>
                        {svc.run_mode_rationale}
                      </div>
                    )}
                    {svc.start_command && (
                      <code style={{
                        display: "block", marginTop: 3,
                        fontSize: 9, color: "#60a5fa", background: "rgba(96,165,250,0.08)",
                        padding: "2px 4px", borderRadius: 2, fontFamily: '"JetBrains Mono", monospace',
                        wordBreak: "break-all", overflowWrap: "anywhere",
                      }}>
                        {svc.start_command}
                      </code>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {/* Incoerenze di configurazione rilevate */}
          {insights.config_issues && insights.config_issues.length > 0 && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, marginBottom: 4 }}>
                Incoerenze di configurazione ({insights.config_issues.length})
              </div>
              {insights.config_issues.map((iss, idx) => {
                const sevColor = iss.severity === "high" ? "#ef4444" : iss.severity === "medium" ? "#f59e0b" : "#94a3b8";
                const alreadySent = sentIssueIds.has(idx);
                const handleResolveWithNexus = () => {
                  if (!onSendToChat || alreadySent) return;
                  const filesList = (iss.files ?? []).map(f => `- \`${f}\``).join("\n");
                  // Niente istruzioni di autonomia (gia' nel dropdown chat),
                  // ma SI contesto di sistema: l'agente deve vedere TUTTE le
                  // incoerenze rilevate e i servizi consigliati, altrimenti
                  // risolve il punto in modo isolato e ignora la direzione
                  // generale del fix. Vedi caso reale: l'utente ha chiesto
                  // di eliminare Docker; senza contesto di sistema l'agente
                  // su una singola issue ricostruisce il setup Docker.
                  const otherIssues = insights.config_issues
                    .filter((_, i) => i !== idx)
                    .map(o => `  - [${o.severity.toUpperCase()}] ${o.title}${o.suggested_fix ? ` → ${o.suggested_fix}` : ""}`)
                    .join("\n");
                  const servicesContext = (insights.services ?? [])
                    .filter(s => s.recommended_run_mode)
                    .map(s => `  - ${s.name} (${s.type}${s.port ? `:${s.port}` : ""}) → modalita' consigliata: ${s.recommended_run_mode}${s.run_mode_rationale ? ` — ${s.run_mode_rationale}` : ""}`)
                    .join("\n");
                  const prompt = [
                    `Risolvi questo problema di configurazione del progetto rilevato dall'analisi AI.`,
                    ``,
                    `## Problema da risolvere`,
                    `**Severità**: ${iss.severity.toUpperCase()}`,
                    `**Titolo**: ${iss.title}`,
                    iss.description ? `**Descrizione**: ${iss.description}` : "",
                    filesList ? `**File coinvolti**:\n${filesList}` : "",
                    iss.suggested_fix ? `**Fix suggerito dall'analizzatore**: ${iss.suggested_fix}` : "",
                    ``,
                    otherIssues ? `## Contesto: altre incoerenze rilevate nello stesso report\n${otherIssues}\n\nRagiona in modo coerente con queste: applica un fix che vada nella stessa direzione del piano d'insieme, non un fix isolato che potrebbe contraddirle.` : "",
                    servicesContext ? `## Modalita' di esecuzione consigliate dall'analizzatore\n${servicesContext}\n\nSe il problema riguarda un servizio elencato sopra, rispetta la modalita' consigliata (native vs docker).` : "",
                    ``,
                    `Valida che il fix proposto sia corretto nel contesto complessivo e applicalo, segnalando alternative migliori se le rilevi.`,
                  ].filter(Boolean).join("\n");
                  onSendToChat(prompt);
                  // Memorizza l'invio per disabilitare il pulsante e mostrare
                  // visivamente che l'azione e' partita. Reset alla prossima analisi.
                  setSentIssueIds(prev => {
                    const next = new Set(prev);
                    next.add(idx);
                    return next;
                  });
                };
                // Stili condivisi per i blocchi testuali — gestiscono overflow e wrapping
                const wrapStyle: React.CSSProperties = {
                  wordBreak: "break-word",
                  overflowWrap: "anywhere",
                  whiteSpace: "pre-wrap",
                  minWidth: 0,
                };
                return (
                  <div key={idx} style={{
                    border: `1px solid ${tc.border}`, borderLeft: `3px solid ${sevColor}`,
                    borderRadius: 3, padding: "5px 8px", marginBottom: 4, background: tc.bgCard,
                    minWidth: 0, maxWidth: "100%", overflow: "hidden",
                  }}>
                    <div style={{ ...wrapStyle, fontSize: 11, fontWeight: 600, color: tc.text }}>
                      <span style={{ color: sevColor, fontSize: 9, marginRight: 4 }}>
                        [{iss.severity.toUpperCase()}]
                      </span>
                      {iss.title}
                    </div>
                    {iss.description && (
                      <div style={{ ...wrapStyle, fontSize: 10, color: tc.textSecondary, marginTop: 2, lineHeight: 1.4 }}>
                        {iss.description}
                      </div>
                    )}
                    {iss.suggested_fix && (
                      <div style={{
                        ...wrapStyle,
                        fontSize: 10, color: "#22c55e", marginTop: 3,
                        fontFamily: '"JetBrains Mono", monospace',
                      }}>
                        → {iss.suggested_fix}
                      </div>
                    )}
                    {onSendToChat && (
                      <div style={{ marginTop: 5, display: "flex", justifyContent: "flex-end" }}>
                        <button
                          onClick={handleResolveWithNexus}
                          disabled={alreadySent}
                          title={alreadySent
                            ? "Gia' inviato a Nexus — la chat sta processando o ha gia' completato. Rianalizza il progetto per ricaricare lo stato."
                            : "Invia il problema alla chat per farlo risolvere a Nexus"}
                          style={{
                            background: alreadySent ? "rgba(148,163,184,0.10)" : "rgba(96,165,250,0.12)",
                            border: alreadySent
                              ? "1px solid rgba(148,163,184,0.30)"
                              : "1px solid rgba(96,165,250,0.45)",
                            borderRadius: 3,
                            color: alreadySent ? tc.textMuted : "#60a5fa",
                            cursor: alreadySent ? "not-allowed" : "pointer",
                            padding: "2px 8px",
                            fontSize: 10,
                            fontWeight: 600,
                            whiteSpace: "nowrap",
                            opacity: alreadySent ? 0.7 : 1,
                          }}
                        >
                          {alreadySent ? "✓ inviato a Nexus" : "Risolvi con Nexus"}
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {/* Azioni suggerite */}
          {insights.suggested_actions && insights.suggested_actions.length > 0 && (
            <div>
              <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, marginBottom: 4 }}>
                Azioni suggerite
              </div>
              {insights.suggested_actions.slice(0, 5).map((act, idx) => {
                const alreadyRun = sentActionIds.has(idx);
                const handleRunWithNexus = () => {
                  if (!onSendToChat || alreadyRun) return;
                  // Stesso principio del pulsante "Risolvi con Nexus":
                  // passare contesto di sistema (altre azioni + servizi)
                  // affinche' l'agente non agisca in isolamento.
                  const otherActions = insights.suggested_actions
                    .filter((_, i) => i !== idx)
                    .slice(0, 4)
                    .map(a => `  ${a.priority}. ${a.title}${a.command ? ` (\`${a.command}\`)` : ""}`)
                    .join("\n");
                  const issuesContext = (insights.config_issues ?? [])
                    .map(o => `  - [${o.severity.toUpperCase()}] ${o.title}`)
                    .join("\n");
                  const servicesContext = (insights.services ?? [])
                    .filter(s => s.recommended_run_mode)
                    .map(s => `  - ${s.name} (${s.type}${s.port ? `:${s.port}` : ""}) → ${s.recommended_run_mode}`)
                    .join("\n");
                  const prompt = [
                    `Esegui questa azione suggerita dall'analisi AI del progetto.`,
                    ``,
                    `## Azione da eseguire`,
                    `**Titolo**: ${act.title}`,
                    act.command ? `**Comando proposto**: \`${act.command}\`` : "",
                    act.rationale ? `**Motivazione**: ${act.rationale}` : "",
                    ``,
                    issuesContext ? `## Contesto: incoerenze di config rilevate nel progetto\n${issuesContext}` : "",
                    otherActions ? `## Contesto: altre azioni nel piano d'insieme\n${otherActions}\n\nL'azione che esegui ora deve essere coerente con questo piano: non contraddire le altre azioni, non rifare lavoro inutile.` : "",
                    servicesContext ? `## Modalita' di esecuzione consigliate\n${servicesContext}` : "",
                    ``,
                    `Valida che il comando sia sicuro nel contesto del progetto attivo, eseguilo o adattalo se necessario, e riporta l'esito.`,
                  ].filter(Boolean).join("\n");
                  onSendToChat(prompt);
                  setSentActionIds(prev => {
                    const next = new Set(prev);
                    next.add(idx);
                    return next;
                  });
                };
                return (
                  <div key={idx} style={{
                    border: `1px solid ${tc.border}`,
                    borderRadius: 3, padding: "5px 8px", marginBottom: 4, background: tc.bgCard,
                    minWidth: 0, maxWidth: "100%", overflow: "hidden",
                  }}>
                    <div style={{ display: "flex", gap: 6, minWidth: 0 }}>
                      <span style={{ color: tc.textMuted, fontSize: 10, minWidth: 14, flexShrink: 0 }}>
                        {act.priority}.
                      </span>
                      <div style={{ flex: 1, minWidth: 0, overflow: "hidden" }}>
                        <div style={{
                          fontSize: 11, color: tc.text, fontWeight: 600,
                          wordBreak: "break-word", overflowWrap: "anywhere",
                        }}>{act.title}</div>
                        {act.command && (
                          <code style={{
                            display: "block",
                            fontSize: 9, color: "#60a5fa", background: "rgba(96,165,250,0.08)",
                            padding: "2px 4px", borderRadius: 2, fontFamily: '"JetBrains Mono", monospace',
                            wordBreak: "break-all", overflowWrap: "anywhere",
                            whiteSpace: "pre-wrap",
                            marginTop: 2,
                          }}>
                            {act.command}
                          </code>
                        )}
                        {act.rationale && (
                          <div style={{
                            fontSize: 9, color: tc.textMuted, marginTop: 2,
                            wordBreak: "break-word", overflowWrap: "anywhere",
                          }}>
                            {act.rationale}
                          </div>
                        )}
                      </div>
                    </div>
                    {onSendToChat && (
                      <div style={{ marginTop: 5, display: "flex", justifyContent: "flex-end" }}>
                        <button
                          onClick={handleRunWithNexus}
                          disabled={alreadyRun}
                          title={alreadyRun
                            ? "Gia' inviata a Nexus — la chat sta processando o ha gia' completato. Rianalizza il progetto per ricaricare lo stato."
                            : "Invia l'azione alla chat per farla eseguire da Nexus (con i tool del progetto)"}
                          style={{
                            background: alreadyRun ? "rgba(148,163,184,0.10)" : "rgba(34,197,94,0.12)",
                            border: alreadyRun
                              ? "1px solid rgba(148,163,184,0.30)"
                              : "1px solid rgba(34,197,94,0.45)",
                            borderRadius: 3,
                            color: alreadyRun ? tc.textMuted : "#22c55e",
                            cursor: alreadyRun ? "not-allowed" : "pointer",
                            padding: "2px 8px",
                            fontSize: 10,
                            fontWeight: 600,
                            whiteSpace: "nowrap",
                            opacity: alreadyRun ? 0.7 : 1,
                          }}
                        >
                          {alreadyRun ? "✓ inviata a Nexus" : "▶ Esegui con Nexus"}
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      <div style={cardStyle(tc)}>
        <div style={{ display: "flex", justifyContent: "space-between", gap: 12, flexWrap: "wrap" }}>
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <div style={{ color: tc.text, fontWeight: 700 }}>GitHub</div>
            <div style={{ color: tc.textSecondary }}>{accountLabel(githubAccount)}</div>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <span style={statusBadgeStyle(tc, accountTone(githubAccount?.status))}>
              {githubAccount?.status ?? "loading"}
            </span>
            {githubAccount?.connected ? (
              <button
                disabled={busy || githubBusy}
                onClick={() => void handleGitHubDisconnect()}
                style={smallButtonStyle(tc, busy || githubBusy)}
              >
                Scollega
              </button>
            ) : (
              <button
                disabled={busy || githubBusy}
                onClick={() => void handleGitHubConnect()}
                style={smallButtonStyle(tc, busy || githubBusy)}
              >
                {connectLabel}
              </button>
            )}
          </div>
        </div>
        {githubAccount?.expiresAt ? (
          <div style={{ color: tc.textMuted, fontSize: 11 }}>
            Token valido fino a {new Date(githubAccount.expiresAt).toLocaleString()}
          </div>
        ) : null}
      </div>

      {!isGitRepo ? (
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
                  onClick={() => void loadGitHubRepositories()}
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
                  onClick={() => void handleCloneSelectedRepository()}
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
      ) : (
        <>
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
          <div style={{ color: tc.textSecondary }}>
            Repo
            <div style={{ color: tc.text, marginTop: 4 }}>
              {githubStatus?.repoFullName ?? githubStatus?.remoteUrl ?? "Non disponibile"}
            </div>
          </div>
          <div style={{ color: tc.textSecondary }}>
            Branch
            <div style={{ color: tc.text, marginTop: 4 }}>{branchName ?? "Non disponibile"}</div>
          </div>
          <div style={{ color: tc.textSecondary }}>
            Upstream
            <div style={{ color: tc.text, marginTop: 4 }}>{githubStatus?.upstream ?? "Non configurato"}</div>
          </div>
          <div style={{ color: tc.textSecondary }}>
            Sync
            <div style={{ color: tc.text, marginTop: 4 }}>
              ahead {githubStatus?.ahead ?? 0} · behind {githubStatus?.behind ?? 0}
            </div>
          </div>
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
        </div>

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
                onClick={() => void handleCreatePullRequest()}
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

      <div style={cardStyle(tc)}>
        <div style={sectionTitleStyle(tc)}>Gestione Repository</div>
        <div style={{ color: tc.textMuted, fontSize: 12 }}>
          Comandi rapidi per stage, commit e sincronizzazione del repository.
        </div>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            disabled={busy || !project.canManageGit || allChangedPaths.length === 0}
            onClick={() => runAction(() => stageGitPaths(project.id, allChangedPaths))}
            title="Stage all — aggiungi tutte le modifiche"
            style={buttonStyle(tc, busy || !project.canManageGit || allChangedPaths.length === 0)}
          >
            Stage tutto
          </button>
          <button
            disabled={busy || !project.canManageGit || (git?.staged.length ?? 0) === 0}
            onClick={() => runAction(() => unstageGitPaths(project.id, git?.staged.map((item) => item.path) ?? []))}
            title="Unstage all — rimuovi tutte le modifiche dallo stage"
            style={buttonStyle(tc, busy || !project.canManageGit || (git?.staged.length ?? 0) === 0)}
          >
            Rimuovi stage
          </button>
        </div>

        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <input
            value={commitMessage}
            onChange={(event) => setCommitMessage(event.target.value)}
            placeholder="Messaggio commit"
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
            title="Commit — salva le modifiche staged"
            style={buttonStyle(tc, busy || !project.canManageGit || !commitMessage.trim())}
          >
            Commit
          </button>
        </div>
      </div>

      <div style={cardStyle(tc)}>
        <div style={sectionTitleStyle(tc)}>Branch</div>
        <div style={{ color: tc.textMuted, fontSize: 12 }}>
          Crea un branch nuovo oppure cambia branch corrente.
        </div>
        <BranchManager
          project={project}
          branches={branches}
          busy={busy}
          runAction={runAction}
        />
      </div>

      <div style={cardStyle(tc)}>
        <div style={sectionTitleStyle(tc)}>Staging</div>
        <div style={{ color: tc.textMuted, fontSize: 12 }}>
          Controlla file staged, unstaged e non tracciati.
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
        <div style={sectionTitleStyle(tc)}>Cronologia Commit</div>
        <CommitLog logEntries={logEntries} />
      </div>
      </>
      )}

      {error && <div style={{ color: tc.error }}>{error}</div>}
    </div>
  );
}
