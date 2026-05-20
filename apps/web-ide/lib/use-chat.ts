"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  cancelAgentRun,
  confirmAgentRun,
  createChatSession,
  deleteChatMessage,
  feedbackChatMessageError,
  feedbackChatMessagePositive,
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
import {
  selectChatLastCompact,
  selectChatLastMessage,
  useProjectStore,
} from "./project-dispatcher";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

type BusyAction = "resend" | "delete" | "feedback" | "feedback-positive";

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
  if (run.status === "loop_aborted") {
    return completed > 0
      ? `Operazione interrotta: il modello era entrato in un ciclo ripetitivo dopo ${completed} step. Al prossimo invio verrà usato automaticamente un modello più capace.`
      : "Operazione interrotta: il modello era entrato in un ciclo ripetitivo. Al prossimo invio verrà usato automaticamente un modello più capace.";
  }
  if (run.status === "provider_unavailable") {
    return "Operazione interrotta: tutti i provider AI configurati sono temporaneamente non disponibili (quota esaurita o rate limit). Riprova tra qualche minuto.";
  }
  if (run.status === "awaiting_confirmation") {
    return awaiting > 0
      ? `In attesa di conferma per ${awaiting} azion${awaiting === 1 ? "e" : "i"}.`
      : "In attesa di conferma per proseguire.";
  }
  return "Operazione conclusa.";
}

/** Costruisce un riepilogo dettagliato delle azioni eseguite dall'agente (P2). */
function buildSemanticDetail(run: AgentRunInfo): string {
  const WRITE_TOOLS = new Set(["write_file", "edit_file", "create_file", "patch_file"]);
  const CMD_TOOLS = new Set(["run_in_terminal", "run_command"]);
  const READ_TOOLS = new Set(["read_file", "search_in_files", "search_files"]);
  const IGNORE_TOOLS = new Set(["supervisor_check"]);

  const modifiedFiles: string[] = [];
  const commands: string[] = [];
  let analysisCount = 0;
  let errorCount = 0;

  for (const step of run.steps) {
    if (IGNORE_TOOLS.has(step.toolName)) continue;
    if (step.status === "failed") errorCount++;

    if (WRITE_TOOLS.has(step.toolName)) {
      const path = (step.toolInput?.path || step.toolInput?.file_path || step.toolInput?.filename) as string | undefined;
      if (path && !modifiedFiles.includes(path)) {
        modifiedFiles.push(path);
      }
    } else if (CMD_TOOLS.has(step.toolName)) {
      const cmd = (step.toolInput?.command || step.toolInput?.cmd || step.toolInput?.text) as string | undefined;
      if (cmd) {
        // Tronca comandi molto lunghi
        const short = cmd.length > 80 ? cmd.slice(0, 77) + "..." : cmd;
        if (!commands.includes(short)) commands.push(short);
      }
    } else if (READ_TOOLS.has(step.toolName)) {
      analysisCount++;
    }
  }

  // Se non ci sono azioni significative, non generare dettagli
  if (modifiedFiles.length === 0 && commands.length === 0 && analysisCount === 0) {
    return "";
  }

  const lines: string[] = [];
  if (modifiedFiles.length > 0) {
    const MAX_FILES = 5;
    const shown = modifiedFiles.slice(0, MAX_FILES).map((f) => {
      // Mostra solo il nome del file (senza path lungo)
      const parts = f.replace(/\\/g, "/").split("/");
      return `\`${parts.length > 2 ? ".../" + parts.slice(-2).join("/") : f}\``;
    });
    const extra = modifiedFiles.length > MAX_FILES ? ` e altri ${modifiedFiles.length - MAX_FILES} file` : "";
    lines.push(`- Modificati ${modifiedFiles.length} file: ${shown.join(", ")}${extra}`);
  }
  if (commands.length > 0) {
    const MAX_CMDS = 3;
    const shown = commands.slice(0, MAX_CMDS).map((c) => `\`${c}\``);
    const extra = commands.length > MAX_CMDS ? ` e altri ${commands.length - MAX_CMDS}` : "";
    lines.push(`- Eseguiti ${commands.length} comandi: ${shown.join(", ")}${extra}`);
  }
  if (analysisCount > 0) {
    lines.push(`- Analizzati ${analysisCount} file`);
  }

  const completed = run.steps.filter((s) => s.status === "completed").length;
  lines.push(`- Risultato: ${completed} step completati${errorCount > 0 ? `, ${errorCount} errori` : ""}`);

  return `\n\n**Riepilogo:**\n${lines.join("\n")}`;
}

function createTerminalMessage(run: AgentRunInfo, pid: string, lastStreamingText?: string): ChatMessage {
  const statusSummary = buildTerminalRunSummary(run);
  const semanticDetail = buildSemanticDetail(run);

  let baseContent: string;
  if (run.finalAnswer?.trim() && run.finalAnswer.trim().length > 0) {
    // La risposta finale del modello e' presente: usala, appendi il dettaglio semantico
    baseContent = run.finalAnswer + semanticDetail;
  } else if (lastStreamingText?.trim() && lastStreamingText.trim().length > 0) {
    // Testo streaming parziale: usalo, appendi il dettaglio semantico
    baseContent = lastStreamingText + semanticDetail;
  } else {
    // Nessuna risposta dal modello: usa status + dettaglio semantico
    baseContent = statusSummary + semanticDetail;
  }

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
    automationMode: "agent" as const,
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
  // Set di messageId per cui l'utente ha gia' votato positivamente in questa sessione.
  // Persistito in sessionStorage per sopravvivere a reload pagina.
  const [positiveFeedback, setPositiveFeedback] = useState<Set<string>>(() => {
    if (typeof window === "undefined") return new Set();
    try {
      const raw = window.sessionStorage.getItem("nexus-positive-feedback");
      return raw ? new Set(JSON.parse(raw)) : new Set();
    } catch {
      return new Set();
    }
  });
  const [agentRun, setAgentRun] = useState<AgentRunInfo | null>(null);
  const [agentSteps, setAgentSteps] = useState<AgentStep[]>([]);
  const [isReconnecting, setIsReconnecting] = useState(false);
  // Mappa run_id → info per agenti paralleli (include anche il run primario)
  const [agentRuns, setAgentRuns] = useState<Map<string, AgentRunInfo>>(new Map());
  const [agentStepsMap, setAgentStepsMap] = useState<Map<string, AgentStep[]>>(new Map());
  // Meta-step pubblicati in chat (plan/routing/clarify/fallback/reflection).
  // Per ogni runId conserviamo la timeline ordinata di entry semantiche che
  // il backend emette via SSE `agent_meta_step`. Sono rese visibili come
  // card collassabili sopra/dentro il messaggio assistant del run.
  const [metaStepsMap, setMetaStepsMap] = useState<
    Map<string, Array<{
      kind: string;
      title: string;
      payload: Record<string, unknown>;
      correlationId?: string | null;
      createdAt: string;
    }>>
  >(new Map());
  const [tokenUsage, setTokenUsage] = useState({ totalTokens: 0, totalCostUsd: 0 });
  const [traces, setTraces] = useState<AITraceEvent[]>([]);
  const [streamingToken, setStreamingToken] = useState<string>("");
  const streamingTokenRef = useRef<string>("");

  // ── Auto-continuazione per modalita' "Automatico" ──
  // Quando un run primario completa con status "completed" e automationMode "automatic",
  // invia automaticamente "Continua" per far proseguire l'agente senza intervento utente.
  // Limite: max 10 continuazioni automatiche consecutive per evitare loop infiniti.
  const autoContinueCountRef = useRef(0);
  const [autoContinuePending, setAutoContinuePending] = useState(false);

  // ── Binding dispatcher: TokenUsageBar e tokenUsage si aggiornano in
  // ── real-time SENZA refresh browser quando il backend emette eventi chat.
  //
  // - ChatSessionCompacted: il backend invia totali freschi dopo compact;
  //   riallineiamo subito la barra (caso bug "percentuale solo dopo F5").
  // - ChatMessageAdded: il backend invia totali assoluti aggiornati ad ogni
  //   INSERT messaggio (TODO: cablare emit lato chat_messages.rs); per ora
  //   resta inattivo finche' il cablaggio backend e' completo.
  const lastCompact = useProjectStore(selectChatLastCompact(sessionId ?? null));
  const lastMessage = useProjectStore(selectChatLastMessage(sessionId ?? null));

  useEffect(() => {
    if (!lastCompact || !sessionId) return;
    setTokenUsage({
      totalTokens: lastCompact.totalTokens,
      totalCostUsd: lastCompact.totalCostUsd,
    });
    // Trigger solo sul timestamp di lastCompact (lo stesso oggetto cambia identita').
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastCompact?.ts, sessionId]);

  useEffect(() => {
    if (!lastMessage || !sessionId) return;
    // Totali assoluti dal backend (idempotente, non incrementale)
    if (lastMessage.totalTokens !== undefined && lastMessage.totalCostUsd !== undefined) {
      setTokenUsage({
        totalTokens: lastMessage.totalTokens,
        totalCostUsd: lastMessage.totalCostUsd,
      });
    }
    // Trigger sul messageId; lastMessage stesso e' read solo per i totalTokens/cost
    // del messaggio corrente.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastMessage?.messageId, sessionId]);

  // ── Auto-continuazione: quando il run completa in modalita' "automatic" e
  // autoContinuePending e' true, invia "Continua" dopo 2s (attende che isLoading
  // sia false). Il contatore limita a max 10 continuazioni consecutive.
  // Reset del contatore: ogni messaggio manuale dell'utente lo azzera.
  const autoContinueSendRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (!autoContinuePending || isLoading || !sessionId) return;
    // Piccolo delay per dare tempo alla UI di aggiornarsi (mostra il messaggio sintetico)
    autoContinueSendRef.current = setTimeout(() => {
      setAutoContinuePending(false);
      // Usa sendChatMessage direttamente per evitare dipendenze circolari con send()
      void (async () => {
        try {
          setIsLoading(true);
          const response = await sendChatMessage(sessionId, "Continua", {
            automationMode: "automatic",
            // synthetic: messaggio auto-generato dal sistema, la UI lo nasconde
            // (l'utente non lo ha digitato; vedere chat_messages.rs::synthetic).
            synthetic: true,
          });
          if (response.agentRun) {
            setMessages((current) => [
              ...current,
              ...(response.userMessage ? [response.userMessage] : []),
            ]);
            setAgentSteps([]);
            const initialRun: AgentRunInfo = {
              runId: response.agentRun.runId,
              sessionId: sessionId,
              status: "running",
              automationMode: "automatic",
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
            setIsLoading(false);
          }
        } catch {
          setIsLoading(false);
          setAutoContinuePending(false);
        }
      })();
    }, 2000);
    return () => {
      if (autoContinueSendRef.current) clearTimeout(autoContinueSendRef.current);
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoContinuePending, isLoading, sessionId]);

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
      // Accumulate token usage from history — esclude i messaggi soft-deleted
      // (es. assistant compattati). Senza il filtro, dopo un compact la
      // TokenUsageBar mostrerebbe ancora i token dei messaggi pre-compact.
      let histTokens = 0;
      let histCost = 0;
      for (const msg of history.messages ?? []) {
        if (msg.role === "assistant" && !msg.deletedAt) {
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
                provider: "auto",  // placeholder: aggiornato da getAgentRun()
                model: "auto",     // placeholder: aggiornato da getAgentRun()
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
            // Helper per valutare lo stato come terminale
            const isStatusTerminal = (status: string) =>
              status === "completed" ||
              status === "failed" ||
              status === "timed_out" ||
              status === "cancelled" ||
              status === "interrupted" ||
              status === "loop_aborted" ||
              status === "provider_unavailable";

            // Polling con retry: se l'evento agent_final SSE e' arrivato MA
            // il DB risponde ancora "running", potrebbe esserci una race
            // condition (UPDATE finale in corso) oppure un bug backend
            // (panic dopo emit ma prima del UPDATE). Riproviamo 3 volte ×
            // 2s. Se dopo 6 secondi il DB resta non-terminale, FORZIAMO lo
            // stato terminale lato UI (l'SSE diceva is_final=true,
            // dobbiamo fidarci e sbloccare l'utente).
            let finalRun = await getAgentRun(runId);
            let attempts = 0;
            const MAX_RETRIES = 3;
            while (!isStatusTerminal(finalRun.status) && attempts < MAX_RETRIES) {
              await new Promise((r) => setTimeout(r, 2000));
              attempts += 1;
              try {
                finalRun = await getAgentRun(runId);
              } catch {
                break;
              }
            }
            // Sintetizza terminale forzato se il DB non si e' aggiornato:
            // l'SSE ci ha gia' detto is_final=true, quindi consideriamo
            // il run concluso (failed) e sblocchiamo la UI. Senza questo
            // l'utente vede il pulsante Stop rosso indefinitamente.
            const dbIsTerminal = isStatusTerminal(finalRun.status);
            if (!dbIsTerminal) {
              console.warn(
                `[use-chat] agent_final ricevuto ma DB resta status=${finalRun.status} dopo ${MAX_RETRIES} retry. Forzo terminale (failed) lato UI.`,
              );
              finalRun = {
                ...finalRun,
                status: "failed",
                finalAnswer:
                  finalRun.finalAnswer ||
                  "⚠ Il backend ha chiuso lo stream ma il database non si e' aggiornato. La risposta potrebbe non essere stata salvata correttamente.",
              };
            }
            setAgentRuns((prev) => new Map(prev).set(runId, finalRun));
            setAgentStepsMap((prev) => new Map(prev).set(runId, finalRun.steps));
            if (isPrimary) {
              // Dopo il retry, il run e' sempre considerato terminale
              // (vero terminale dal DB, oppure forzato fail dopo timeout)
              const isTerminal = isStatusTerminal(finalRun.status);
              if (isTerminal) {
                // Run completato: ripulisci lo stato dell'agente attivo
                // cosi' la UI smette di mostrare "Agente in esecuzione" e
                // il pulsante "Stop" rosso. Senza questo reset l'utente
                // vedeva la chat bloccata in "running" anche dopo che il
                // backend aveva chiuso lo stream SSE (done event).
                setAgentRun(null);
                setAgentSteps([]);
              } else {
                setAgentRun(finalRun);
                setAgentSteps(finalRun.steps);
              }
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
                // Refresh dei messaggi dal DB con RETRY: il backend salva il
                // messaggio assistant ASYNC dopo l'emissione di `agent_final`,
                // quindi al primo getChatMessages potrebbe non essere ancora
                // presente (race condition tra SSE done e DB INSERT). Riproviamo
                // fino a 5 volte con backoff: 0ms → 500ms → 1s → 1.5s → 2s.
                // Stop appena troviamo un assistant message creato DOPO l'inizio
                // del run corrente (= il vero messaggio appena persistito).
                const runStartedTs = finalRun.createdAt
                  ? new Date(finalRun.createdAt).getTime()
                  : 0;
                const tryRefresh = async (attempt: number): Promise<boolean> => {
                  try {
                    const history = await getChatMessages(sid);
                    if (!history.messages || history.messages.length === 0) return false;
                    // Cerca un assistant message persistito DOPO l'inizio del run
                    const hasRealAssistant = history.messages.some((m) =>
                      m.role === "assistant" &&
                      new Date(m.createdAt).getTime() >= runStartedTs - 1000,
                    );
                    if (!hasRealAssistant && attempt < 5) return false;
                    setMessages((current) => {
                      const syntheticIdForThisRun = `agent-${runId}`;
                      const dbAssistantIds = new Set(
                        history.messages.filter((m) => m.role === "assistant").map((m) => m.id),
                      );
                      const otherSynthetics = current.filter(
                        (m) =>
                          m.id.startsWith("agent-") &&
                          m.id !== syntheticIdForThisRun &&
                          !dbAssistantIds.has(m.id),
                      );
                      return [...history.messages, ...otherSynthetics];
                    });
                    return hasRealAssistant;
                  } catch {
                    return false;
                  }
                };
                // Sequenza di retry: 0ms, 500ms, 1000ms, 1500ms, 2000ms
                void (async () => {
                  for (let i = 0; i < 5; i++) {
                    if (i > 0) await new Promise((r) => setTimeout(r, 500));
                    const found = await tryRefresh(i);
                    if (found) return; // assistant message vero trovato → stop
                  }
                })();
                // Auto-continuazione: DISATTIVATA per default in modalita'
                // "automatic". L'utente sceglie "Automatico" per evitare conferme,
                // NON per far girare l'agente in loop dopo aver completato il task.
                // Il loop precedente bruciava 1.8M+ token per task gia' completati
                // (vedi issue utente: "nexus ha risolto ma poi la chat e' ripartita
                // da sola"). Auto-continue resta possibile SOLO se il run e' in
                // stato `awaiting_confirmation`, cioe' l'agente sta esplicitamente
                // aspettando un input — caso in cui "automatic" significa
                // "rispondi automaticamente per confermare".
                if (
                  finalRun.status === "awaiting_confirmation" &&
                  finalRun.automationMode === "automatic" &&
                  autoContinueCountRef.current < 3
                ) {
                  autoContinueCountRef.current += 1;
                  setAutoContinuePending(true);
                } else {
                  // Run completato/failed/cancelled: STOP. L'utente puo' digitare
                  // "Continua" manualmente se vuole proseguire.
                  autoContinueCountRef.current = 0;
                }
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
        (meta) => {
          // Accoda il meta_step alla timeline del run. Dedup per
          // (kind, createdAt) per resistere a duplicati SSE (replay).
          setMetaStepsMap((prev) => {
            const current = prev.get(runId) ?? [];
            const isDup = current.some(
              (m) => m.kind === meta.metaStep.kind && m.createdAt === meta.metaStep.createdAt,
            );
            if (isDup) return prev;
            const next = [...current, meta.metaStep];
            return new Map(prev).set(runId, next);
          });
        },
      );
    },
    [projectId],
  );

  const send = useCallback(
    async (content: string, options: SendChatMessageOptions = {}) => {
      if (!hasProject || !isReady || !sessionId || !content.trim() || isLoading) {
        return;
      }
      // Reset contatore auto-continuazione: ogni messaggio manuale dell'utente
      // azzera il conteggio per permettere nuove sequenze di auto-continuazione.
      autoContinueCountRef.current = 0;
      setAutoContinuePending(false);

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

  const feedbackPositive = useCallback(async (messageId: string, comment?: string) => {
    if (!messageId) return;
    if (positiveFeedback.has(messageId)) return; // idempotente lato client
    setBusyByMessage((current) => ({ ...current, [messageId]: "feedback-positive" }));
    setError(null);
    try {
      await feedbackChatMessagePositive(messageId, comment?.trim() || undefined);
      setPositiveFeedback((prev) => {
        const next = new Set(prev);
        next.add(messageId);
        if (typeof window !== "undefined") {
          try {
            window.sessionStorage.setItem(
              "nexus-positive-feedback",
              JSON.stringify(Array.from(next)),
            );
          } catch { /* quota / disabled — ignora */ }
        }
        return next;
      });
    } catch (e) {
      setError(formatChatError(e, "Invio feedback positivo fallito."));
    } finally {
      setBusyByMessage((current) => {
        const next = { ...current };
        delete next[messageId];
        return next;
      });
    }
  }, [positiveFeedback]);

  const clear = useCallback(() => {
    setMessages([]);
    setError(null);
    setSessionId(null);
    setIsReady(false);
    setAgentRun(null);
    setAgentSteps([]);
    setAgentRuns(new Map());
    setAgentStepsMap(new Map());
    setMetaStepsMap(new Map());
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
                  finalRun.status === "interrupted" ||
                  finalRun.status === "loop_aborted" ||
                  finalRun.status === "provider_unavailable";
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
            (meta) => {
              setMetaStepsMap((prev) => {
                const current = prev.get(runId) ?? [];
                const isDup = current.some(
                  (m) => m.kind === meta.metaStep.kind && m.createdAt === meta.metaStep.createdAt,
                );
                if (isDup) return prev;
                return new Map(prev).set(runId, [...current, meta.metaStep]);
              });
            },
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
    metaStepsMap,
    tokenUsage,
    traces,
    streamingToken,
    send,
    resend,
    remove,
    feedbackError,
    feedbackPositive,
    positiveFeedback,
    confirmAgent,
    cancelRun: useCallback(async (runId: string) => {
      // Resetta stato UI SUBITO per sbloccare l'input (prima delle chiamate async)
      setAgentRun(null);
      setAgentSteps([]);
      setIsLoading(false);
      try { await cancelAgentRun(runId); } catch { /* ignore */ }
      // Recupera il run finale e mostra il messaggio di interruzione nel chat
      try {
        const finalRun = await getAgentRun(runId);
        if (finalRun) {
          const syntheticMsg = createTerminalMessage(finalRun, projectId);
          setMessages((current) => upsertSyntheticAssistantMessage(current, syntheticMsg));
        }
      } catch { /* ignore */ }
    }, [projectId]),
    clear,
    clearTraces: () => setTraces([]),
    refresh: bootstrap,
  };
}
