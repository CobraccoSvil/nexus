"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  cancelAgentRun,
  confirmAgentRun,
  createChatSession,
  deleteChatMessage,
  feedbackChatMessageError,
  getActiveRunForSession,
  getAgentRun,
  getChatMessages,
  getChatSessions,
  getSessionUsage,
  resendChatMessage,
  sendChatMessage,
  subscribeAgentStream,
  type AgentRunInfo,
  type AgentStep,
  type AITraceEvent,
  type SendChatMessageOptions,
  type ChatMessage,
} from "./api-client";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

type BusyAction = "resend" | "delete" | "feedback";

function upsertSyntheticAssistantMessage(current: ChatMessage[], message: ChatMessage): ChatMessage[] {
  const index = current.findIndex((item) => item.id === message.id);
  if (index >= 0) {
    const next = [...current];
    next[index] = message;
    return next;
  }
  return [...current, message];
}

function buildTerminalRunSummary(run: AgentRunInfo): string {
  const completed = run.steps.filter((step) => step.status === "completed").length;
  const failed = run.steps.filter((step) => step.status === "failed").length;
  const awaiting = run.pendingActions.length;

  if (run.status === "completed") {
    return completed > 0
      ? `Operazione completata. Ho eseguito ${completed} step.`
      : "Operazione completata.";
  }
  if (run.status === "failed") {
    if (completed > 0) {
      return `Operazione terminata con errore dopo ${completed} step completati${failed > 0 ? ` e ${failed} falliti` : ""}.`;
    }
    return "Operazione terminata con errore.";
  }
  if (run.status === "timed_out") {
    return completed > 0
      ? `Operazione interrotta per timeout dopo ${completed} step completati.`
      : "Operazione interrotta per timeout prima della risposta finale.";
  }
  if (run.status === "cancelled") {
    return "Operazione annullata.";
  }
  if (run.status === "interrupted") {
    return completed > 0
      ? `Elaborazione interrotta dal riavvio del server dopo ${completed} step. Puoi ripetere la richiesta.`
      : "Elaborazione interrotta dal riavvio del server. Puoi ripetere la richiesta.";
  }
  if (run.status === "awaiting_confirmation") {
    return awaiting > 0
      ? `In attesa di conferma per ${awaiting} azion${awaiting === 1 ? "e" : "i"}.`
      : "In attesa di conferma per proseguire.";
  }
  return "Operazione conclusa.";
}

function createTerminalMessage(run: AgentRunInfo, pid: string, lastStreamingText?: string): ChatMessage {
  const baseContent =
    run.finalAnswer?.trim() && run.finalAnswer.trim().length > 0
      ? run.finalAnswer
      : lastStreamingText?.trim() && lastStreamingText.trim().length > 0
        ? lastStreamingText
        : buildTerminalRunSummary(run);

  // Prependi l'avviso privacy se il provider non e' EU/locale
  const content = run.providerPrivacyNotice
    ? `${run.providerPrivacyNotice}\n\n---\n\n${baseContent}`
    : baseContent;

  return {
    id: `agent-${run.runId}`,
    sessionId: run.sessionId,
    projectId: pid,
    role: "assistant",
    content,
    runId: run.runId,
    provider: run.provider,
    model: run.model,
    promptTokens: run.usage?.totalPromptTokens,
    completionTokens: run.usage?.totalCompletionTokens,
    totalTokens: run.usage?.totalTokens,
    totalCost: run.totalCostUsd,
    createdAt: run.completedAt ?? new Date().toISOString(),
  };
}

function formatChatError(error: unknown, fallback: string): string {
  if (error instanceof DOMException && error.name === "AbortError") {
    return "La richiesta e' stata interrotta (timeout di rete o navigazione). Riprova.";
  }
  const raw = error instanceof Error ? error.message : fallback;
  const normalized = raw.trim();
  const lower = normalized.toLowerCase();

  if (lower.includes("aborted") || lower.includes("abort")) {
    return "La richiesta e' stata interrotta. Riprova tra qualche secondo.";
  }
  if (
    lower.includes("429") ||
    lower.includes("rate limit") ||
    lower.includes("rate_limit") ||
    lower.includes("quota")
  ) {
    return "Il provider AI e' temporaneamente in rate limit. Riprovo in fallback automatico; se persiste, attendi qualche secondo e ripeti.";
  }
  if (
    (lower.includes("not_found_error") || lower.includes("not found")) &&
    lower.includes("model")
  ) {
    return "Il modello selezionato non e' disponibile presso il provider corrente. Prova un modello diverso o lascia la selezione automatica.";
  }
  if (lower.includes("connection error")) {
    return "Connessione al provider interrotta durante l'esecuzione. Ho mantenuto lo stato del run; puoi riprovare subito.";
  }
  if (
    lower.includes("transport error") ||
    lower.includes("status: unavailable") ||
    lower.includes("connection refused")
  ) {
    return "Connessione interna ai servizi AI temporaneamente non disponibile. Riprova tra pochi secondi.";
  }
  if (lower.includes("timeout")) {
    return "La richiesta e' andata in timeout. Riprova tra poco o con un prompt piu' breve.";
  }
  const compact = normalized.replace(/\s+/g, " ");
  if (compact.startsWith("{") || compact.startsWith("[")) {
    return fallback;
  }
  if (compact.length > 220) {
    return `${compact.slice(0, 220)}...`;
  }
  return compact || fallback;
}

export function useChat(
  projectId = "default",
  profileId = "default",
  opts: { sessionId?: string } = {},
) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(opts.sessionId ?? null);
  const [isLoading, setIsLoading] = useState(false);
  const [isReady, setIsReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busyByMessage, setBusyByMessage] = useState<Record<string, BusyAction | undefined>>({});
  const [agentRun, setAgentRun] = useState<AgentRunInfo | null>(null);
  const [agentSteps, setAgentSteps] = useState<AgentStep[]>([]);
  const [isReconnecting, setIsReconnecting] = useState(false);
  // Mappa run_id → info per agenti paralleli (include anche il run primario)
  const [agentRuns, setAgentRuns] = useState<Map<string, AgentRunInfo>>(new Map());
  const [agentStepsMap, setAgentStepsMap] = useState<Map<string, AgentStep[]>>(new Map());
  const [tokenUsage, setTokenUsage] = useState({ totalTokens: 0, totalCostUsd: 0 });
  const [traces, setTraces] = useState<AITraceEvent[]>([]);
  const [streamingToken, setStreamingToken] = useState<string>("");
  const streamingTokenRef = useRef<string>("");

  // Persist traces to sessionStorage when they change
  useEffect(() => {
    if (!sessionId) return;
    try {
      sessionStorage.setItem(`nexus:traces:${sessionId}`, JSON.stringify(traces));
    } catch { /* ignore */ }
  }, [traces, sessionId]);

  const hasProject = useMemo(() => UUID_RE.test(projectId), [projectId]);

  const bootstrap = useCallback(async () => {
    if (!hasProject) {
      setMessages([]);
      setSessionId(null);
      setIsReady(false);
      setError(null);
      return;
    }

    setIsLoading(true);
    setError(null);
    try {
      let activeSessionId: string | null = opts.sessionId ?? null;

      if (!activeSessionId) {
        // Legacy: auto-select first session or create new
        const sessions = await getChatSessions(projectId);
        const activeSession =
          sessions.sessions[0] ??
          (await createChatSession(projectId, "Nuova sessione")).session;
        activeSessionId = activeSession.id;
      }

      setSessionId(activeSessionId);
      // Restore persisted traces for this session
      try {
        const saved = sessionStorage.getItem(`nexus:traces:${activeSessionId}`);
        if (saved) {
          const parsed = JSON.parse(saved) as AITraceEvent[];
          if (Array.isArray(parsed) && parsed.length > 0) {
            setTraces(parsed);
          }
        }
      } catch { /* ignore */ }
      const history = await getChatMessages(activeSessionId);
      setMessages(history.messages ?? []);
      // Accumulate token usage from history
      let histTokens = 0;
      let histCost = 0;
      for (const msg of history.messages ?? []) {
        if (msg.role === "assistant") {
          histTokens += msg.totalTokens ?? 0;
          histCost += msg.totalCost ?? 0;
        }
      }
      if (histTokens > 0 || histCost > 0) {
        setTokenUsage({ totalTokens: histTokens, totalCostUsd: histCost });
      }
      setIsReady(true);
    } catch (e) {
      setError(formatChatError(e, "Impossibile inizializzare la chat."));
      setMessages([]);
      setSessionId(null);
      setIsReady(false);
    } finally {
      setIsLoading(false);
    }
  }, [hasProject, projectId, opts.sessionId]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  // Sottoscrive a un run (primario o figlio) e aggiorna le map di stato
  const subscribeToRun = useCallback(
    (sid: string, runId: string, isPrimary: boolean) => {
      subscribeAgentStream(
        sid,
        runId,
        (event) => {
          if (!event.step) return; // eventi trace non hanno step
          setAgentStepsMap((prev) => {
            const current = prev.get(runId) ?? [];
            const existing = current.findIndex((s) => s.stepIndex === event.step!.stepIndex);
            const next = existing >= 0
              ? current.map((s, i) => (i === existing ? event.step! : s))
              : [...current, event.step!];
            return new Map(prev).set(runId, next);
          });
          if (isPrimary) {
            setAgentSteps((prev) => {
              const existing = prev.findIndex((s) => s.stepIndex === event.step!.stepIndex);
              if (existing >= 0) {
                const next = [...prev];
                next[existing] = event.step!;
                return next;
              }
              return [...prev, event.step!];
            });
          }
          // Se lo step contiene un sub-run lanciato da dispatch_subtask, sottoscriviti
          if (event.step.toolName === "dispatch_subtask" && event.step.toolResult) {
            const match = event.step.toolResult.match(/ID:\s*([0-9a-f-]{36})/i);
            if (match) {
              const childRunId = match[1];
              const childRun: AgentRunInfo = {
                runId: childRunId,
                sessionId: sid,
                status: "running",
                automationMode: "automatic",
                provider: "anthropic",
                model: "claude-haiku-4-5",
                iterationCount: 0,
                pendingActions: [],
                steps: [],
                createdAt: new Date().toISOString(),
              };
              setAgentRuns((prev) => new Map(prev).set(childRunId, childRun));
              setAgentStepsMap((prev) => new Map(prev).set(childRunId, []));
              subscribeToRun(sid, childRunId, false);
            }
          }
        },
        async () => {
          try {
            const finalRun = await getAgentRun(runId);
            setAgentRuns((prev) => new Map(prev).set(runId, finalRun));
            setAgentStepsMap((prev) => new Map(prev).set(runId, finalRun.steps));
            if (isPrimary) {
              setAgentRun(finalRun);
              setAgentSteps(finalRun.steps);
              const isTerminal =
                finalRun.status === "completed" ||
                finalRun.status === "failed" ||
                finalRun.status === "timed_out" ||
                finalRun.status === "cancelled" ||
                finalRun.status === "interrupted";
              if (isTerminal && sid) {
                const syntheticMsg = createTerminalMessage(finalRun, projectId, streamingTokenRef.current);
                setMessages((current) => upsertSyntheticAssistantMessage(current, syntheticMsg));
                try {
                  const usage = await getSessionUsage(sid);
                  setTokenUsage({ totalTokens: usage.totalTokens, totalCostUsd: usage.totalCostUsd });
                  if (usage.totalTokens > 0) {
                    setMessages((current) =>
                      current.map((m) =>
                        m.id === syntheticMsg.id
                          ? { ...m, totalTokens: usage.totalTokens, totalCost: usage.totalCostUsd }
                          : m,
                      ),
                    );
                  }
                } catch {}
              }
            }
            // Rimuove i run completati dalla map dopo 30s
            setTimeout(() => {
              setAgentRuns((prev) => {
                const next = new Map(prev);
                next.delete(runId);
                return next;
              });
            }, 30_000);
          } catch {}
          if (isPrimary) {
            setIsLoading(false);
            setStreamingToken("");
            streamingTokenRef.current = "";
          }
        },
        (trace) => {
          // Accumula trace tra le run (max 100, FIFO). Non cancellare mai automaticamente.
          setTraces((prev) => {
            const next = [...prev, trace];
            return next.length > 100 ? next.slice(next.length - 100) : next;
          });
        },
        isPrimary ? setIsReconnecting : undefined,
        isPrimary ? (delta: string) => {
          setStreamingToken((prev) => {
            const next = prev + delta;
            streamingTokenRef.current = next;
            return next;
          });
        } : undefined,
      );
    },
    [projectId],
  );

  const send = useCallback(
    async (content: string, options: SendChatMessageOptions = {}) => {
      if (!hasProject || !isReady || !sessionId || !content.trim() || isLoading) {
        return;
      }

      setIsLoading(true);
      setError(null);
      let isAgentMode = false;
      try {
        const response = await sendChatMessage(sessionId, content.trim(), {
          ...options,
          profileId: options.profileId ?? profileId,
        });

        if (response.agentRun) {
          isAgentMode = true;
          // Modalita' agente: aggiungi solo il messaggio utente, poi ascolta lo stream
          setMessages((current) => [
            ...current,
            ...(response.userMessage ? [response.userMessage] : []),
          ]);
          setAgentSteps([]);
          // NON cancellare le trace: accumularle tra le run così il pannello
          // Trace AI mostra la storia completa della sessione (max 100 per non appesantire).
          // L'utente può pulire manualmente con il pulsante "Pulisci" nel pannello.
          const initialRun: AgentRunInfo = {
            runId: response.agentRun.runId,
            sessionId: sessionId,
            status: "running",
            automationMode: options.automationMode ?? "confirm",
            provider: response.agentRun.provider,
            model: response.agentRun.model,
            iterationCount: 0,
            pendingActions: [],
            steps: [],
            createdAt: new Date().toISOString(),
          };
          setAgentRun(initialRun);
          setAgentRuns((prev) => new Map(prev).set(initialRun.runId, initialRun));
          setAgentStepsMap((prev) => new Map(prev).set(initialRun.runId, []));

          subscribeToRun(sessionId, response.agentRun.runId, true);
        } else {
          // Modalita' normale (Study o fallback)
          setMessages((current) => [
            ...current,
            ...(response.userMessage ? [response.userMessage] : []),
            ...(response.assistantMessage ? [response.assistantMessage] : []),
          ]);
          if (response.assistantMessage) {
            const tokens = response.assistantMessage.totalTokens ?? 0;
            const cost = response.assistantMessage.totalCost ?? 0;
            if (tokens > 0 || cost > 0) {
              setTokenUsage((prev) => ({
                totalTokens: prev.totalTokens + tokens,
                totalCostUsd: prev.totalCostUsd + cost,
              }));
            }
          }
        }
      } catch (e) {
        setError(formatChatError(e, "Invio messaggio fallito."));
        isAgentMode = false;
      } finally {
        if (!isAgentMode) setIsLoading(false);
      }
    },
    [hasProject, isLoading, isReady, profileId, sessionId, subscribeToRun],
  );

  const resend = useCallback(
    async (messageId: string, options: SendChatMessageOptions = {}) => {
      if (!messageId || isLoading) return;
      setBusyByMessage((current) => ({ ...current, [messageId]: "resend" }));
      setError(null);
      try {
        const response = await resendChatMessage(messageId, {
          ...options,
          profileId: options.profileId ?? profileId,
        });
        setSessionId(response.sessionId);

        if (response.agentRun) {
          // Modalita' agente: aggiungi solo il messaggio utente, poi ascolta lo stream
          setMessages((current) => [
            ...current,
            ...(response.userMessage ? [response.userMessage] : []),
          ]);
          setAgentSteps([]);
          const initialRun: AgentRunInfo = {
            runId: response.agentRun.runId,
            sessionId: response.sessionId,
            status: "running",
            automationMode: options.automationMode ?? "confirm",
            provider: response.agentRun.provider,
            model: response.agentRun.model,
            iterationCount: 0,
            pendingActions: [],
            steps: [],
            createdAt: new Date().toISOString(),
          };
          setAgentRun(initialRun);
          setAgentRuns((prev) => new Map(prev).set(initialRun.runId, initialRun));
          setAgentStepsMap((prev) => new Map(prev).set(initialRun.runId, []));
          subscribeToRun(response.sessionId, response.agentRun.runId, true);
        } else {
          setMessages((current) => [
            ...current,
            ...(response.userMessage ? [response.userMessage] : []),
            ...(response.assistantMessage ? [response.assistantMessage] : []),
          ]);
        }
      } catch (e) {
        setError(formatChatError(e, "Reinvio fallito."));
      } finally {
        setBusyByMessage((current) => {
          const next = { ...current };
          delete next[messageId];
          return next;
        });
      }
    },
    [isLoading, profileId, subscribeToRun],
  );

  const remove = useCallback(async (messageId: string) => {
    if (!messageId) return;
    setBusyByMessage((current) => ({ ...current, [messageId]: "delete" }));
    setError(null);
    try {
      await deleteChatMessage(messageId);
      setMessages((current) =>
        current.map((message) =>
          message.id === messageId
            ? { ...message, deletedAt: new Date().toISOString(), content: "[messaggio eliminato]" }
            : message,
        ),
      );
    } catch (e) {
      setError(formatChatError(e, "Cancellazione fallita."));
    } finally {
      setBusyByMessage((current) => {
        const next = { ...current };
        delete next[messageId];
        return next;
      });
    }
  }, []);

  const feedbackError = useCallback(async (messageId: string, comment: string) => {
    if (!messageId || !comment.trim()) return;
    setBusyByMessage((current) => ({ ...current, [messageId]: "feedback" }));
    setError(null);
    try {
      await feedbackChatMessageError(messageId, comment.trim());
    } catch (e) {
      setError(formatChatError(e, "Invio feedback fallito."));
    } finally {
      setBusyByMessage((current) => {
        const next = { ...current };
        delete next[messageId];
        return next;
      });
    }
  }, []);

  const clear = useCallback(() => {
    setMessages([]);
    setError(null);
    setSessionId(null);
    setIsReady(false);
    setAgentRun(null);
    setAgentSteps([]);
    setAgentRuns(new Map());
    setAgentStepsMap(new Map());
    setTokenUsage({ totalTokens: 0, totalCostUsd: 0 });
    setTraces([]);
  }, []);

  const confirmAgent = useCallback(
    async (runId: string, approved: boolean) => {
      setError(null);
      try {
        await confirmAgentRun(runId, approved);
        if (!approved) {
          setAgentRun((prev) => (prev ? { ...prev, status: "cancelled" } : null));
          setIsLoading(false);
          return;
        }
        setAgentRun((prev) => (prev ? { ...prev, status: "running" } : null));
        setAgentRuns((prev) => {
          const run = prev.get(runId);
          if (!run) return prev;
          return new Map(prev).set(runId, { ...run, status: "running" });
        });
        // Riavvia l'ascolto dello stream
        if (sessionId) {
          subscribeAgentStream(
            sessionId,
            runId,
            (event) => {
              if (!event.step) return; // eventi trace non hanno step
              setAgentSteps((prev) => {
                const existing = prev.findIndex((s) => s.stepIndex === event.step!.stepIndex);
                if (existing >= 0) {
                  const next = [...prev];
                  next[existing] = event.step!;
                  return next;
                }
                return [...prev, event.step!];
              });
            },
            async () => {
              try {
                const finalRun = await getAgentRun(runId);
                setAgentRun(finalRun);
                setAgentSteps(finalRun.steps);
                const isTerminal =
                  finalRun.status === "completed" ||
                  finalRun.status === "failed" ||
                  finalRun.status === "timed_out" ||
                  finalRun.status === "cancelled" ||
                  finalRun.status === "interrupted";
                if (isTerminal) {
                  const syntheticMsg = createTerminalMessage(finalRun, projectId);
                  setMessages((current) => upsertSyntheticAssistantMessage(current, syntheticMsg));
                }
              } catch {}
              setIsLoading(false);
              setIsReconnecting(false);
            },
            undefined,
            setIsReconnecting,
            (delta: string) => setStreamingToken((prev) => prev + delta),
          );
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : "Conferma fallita.");
        setIsLoading(false);
      }
    },
    [projectId, sessionId],
  );

  // Dopo bootstrap: riconnetti all'agente in corso (se il browser è stato refreshato mentre girava)
  useEffect(() => {
    if (!isReady || !sessionId) return;
    // Solo al primo mount (agentRun è ancora null): cerca un run attivo nel DB
    if (agentRun !== null) return;
    let cancelled = false;
    getActiveRunForSession(sessionId).then(({ activeRun }) => {
      if (cancelled || !activeRun) return;
      if (activeRun.status !== "running" && activeRun.status !== "awaiting_confirmation") return;
      const runId = activeRun.runId;
      setAgentRun(activeRun);
      setAgentSteps(activeRun.steps ?? []);
      setAgentRuns((prev) => new Map(prev).set(runId, activeRun));
      setAgentStepsMap((prev) => new Map(prev).set(runId, activeRun.steps ?? []));
      setIsLoading(true);
      subscribeToRun(sessionId, runId, true);
    }).catch(() => { /* ignora — non critico */ });
    return () => { cancelled = true; };
  // dipendenze escluse intenzionalmente: agentRun letto solo per early-exit al mount (includerlo causerebbe loop); subscribeToRun è stabile (useCallback su [projectId])
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isReady, sessionId]);

  return {
    messages,
    sessionId,
    isLoading,
    isReady,
    isReconnecting,
    error,
    busyByMessage,
    agentRun,
    agentSteps,
    agentRuns,
    agentStepsMap,
    tokenUsage,
    traces,
    streamingToken,
    send,
    resend,
    remove,
    feedbackError,
    confirmAgent,
    cancelRun: useCallback(async (runId: string) => {
      try { await cancelAgentRun(runId); } catch { /* ignore */ }
      // Recupera il run finale e mostra il messaggio di interruzione nel chat
      try {
        const finalRun = await getAgentRun(runId);
        if (finalRun) {
          setAgentRun(finalRun);
          setAgentSteps(finalRun.steps);
          const syntheticMsg = createTerminalMessage(finalRun, projectId);
          setMessages((current) => upsertSyntheticAssistantMessage(current, syntheticMsg));
        }
      } catch { /* ignore */ }
      // Resetta stato loading
      setAgentRun(null);
      setAgentSteps([]);
      setIsLoading(false);
    }, [projectId]),
    clear,
    clearTraces: () => setTraces([]),
    refresh: bootstrap,
  };
}
