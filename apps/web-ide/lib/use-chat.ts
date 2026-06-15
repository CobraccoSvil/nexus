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
  type SavedChatAttachment,
} from "./api-client";
import {
  selectChatLastCompact,
  selectChatLastMessage,
  useProjectStore,
} from "./project-dispatcher";
import { UUID_RE, type BusyAction, type MetaStepEntry } from "./use-chat/types";
import { upsertSyntheticAssistantMessage, isStatusTerminal } from "./use-chat/helpers";
import { createTerminalMessage } from "./use-chat/run-summary";
import { formatChatError } from "./use-chat/errors";

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
    Map<string, MetaStepEntry[]>
  >(new Map());
  const [tokenUsage, setTokenUsage] = useState({ totalTokens: 0, totalCostUsd: 0 });
  const [traces, setTraces] = useState<AITraceEvent[]>([]);
  const [streamingToken, setStreamingToken] = useState<string>("");
  const streamingTokenRef = useRef<string>("");
  // Ragionamento intermedio del modello (testo che accompagna le tool calls
  // durante le iterazioni dell'agent loop). Sostituito ad ogni iterazione,
  // svuotato quando il run termina. Mostrato nel chat-panel come blocco
  // collassabile per dare feedback visivo durante l'elaborazione.
  const [thinkingText, setThinkingText] = useState<string>("");

  // ── Proposta di indicizzazione allegati nella Knowledge Base ──
  // Dopo l'invio di un messaggio con allegati persistiti, il backend
  // restituisce `savedAttachments`. Questo state segnala al chat-panel di
  // mostrare un modale che chiede all'utente quali file indicizzare in KB.
  // null = nessuna proposta in coda; impostato/cancellato esplicitamente.
  const [attachmentIndexProposal, setAttachmentIndexProposal] = useState<{
    messageId: string;
    attachments: SavedChatAttachment[];
  } | null>(null);

  // ── Auto-continuazione per modalita' "Automatico" ──
  // Quando un run primario completa con status "completed" e automationMode "automatic",
  // invia automaticamente "Continua" per far proseguire l'agente senza intervento utente.
  // Limite: max 10 continuazioni automatiche consecutive per evitare loop infiniti.
  const autoContinueCountRef = useRef(0);
  const [autoContinuePending, setAutoContinuePending] = useState(false);

  // ── Coda messaggi ──
  // Messaggi inviati dall'utente MENTRE un agent run e' in corso. Vengono
  // accodati qui e inviati automaticamente a fine run dall'effect di drain (sotto
  // a `send`). Evita run concorrenti sulla stessa sessione — il backend ne
  // rifiuta uno con 409 — e la perdita silenziosa del messaggio osservata quando
  // si inviava un secondo messaggio durante un run ancora attivo.
  const [pendingQueue, setPendingQueue] = useState<
    Array<{ content: string; options: SendChatMessageOptions }>
  >([]);

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
    // Ricarica i messaggi dal DB cosi' i messaggi pre-compact (ora con
    // deleted_at valorizzato dal backend) vengano filtrati e il calcolo
    // di ratio/ctx% sul bottone Compatta usi solo i messaggi vivi.
    // Senza questo refresh, lastAssistantWithTokens resta legato al
    // vecchio messaggio gigante e ctx% rimane bloccato su valori >100%.
    (async () => {
      try {
        const history = await getChatMessages(sessionId);
        if (history.messages) setMessages(history.messages);
      } catch {
        // ignore: il TokenUsage e' gia' aggiornato; il refresh dei messaggi
        // riavverra' al prossimo turno.
      }
    })();
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
          // Tool `nexus_open_file_in_editor`: il backend ritorna un JSON con
          // `_ui_action: "open_file"` + path. Intercettiamo qui per dispatchare
          // l'evento globale che apre il file nell'editor del web-ide.
          if (event.step.toolName === "nexus_open_file_in_editor" && event.step.toolResult) {
            try {
              const parsed = JSON.parse(event.step.toolResult);
              if (parsed && parsed.ok && parsed._ui_action === "open_file" && typeof parsed.path === "string") {
                if (typeof window !== "undefined") {
                  window.dispatchEvent(new CustomEvent("nexus:editor:open-file", {
                    detail: { path: parsed.path, line: parsed.line ?? undefined },
                  }));
                }
              }
            } catch {
              // toolResult non e' JSON parseabile, skip silenzioso
            }
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
                  if (usage.totalTokens > 0 && syntheticMsg) {
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
            setThinkingText("");
          }
        },
        (trace) => {
          // Accumula trace tra le run (max 100, FIFO). Non cancellare mai automaticamente.
          setTraces((prev) => {
            const next = [...prev, trace];
            return next.length > 100 ? next.slice(next.length - 100) : next;
          });
          // L'AITraceEvent porta provider/model dell'iterazione = modello REALE
          // corrente del run (riflette cascade/escalation e il fallback gateway).
          // Allineiamo agentRun cosi' l'indicatore "run: X/Y" nel composer segue
          // il modello che sta girando ORA, non quello iniziale del routing
          // (fix etichetta stantia: mostrava openai/gpt-4o-mini mentre girava
          // google/gemini-2.5-pro). Aggiorna solo se cambia (evita re-render).
          if (trace.provider && trace.model) {
            const tp = trace.provider;
            const tm = trace.model;
            setAgentRuns((prevMap) => {
              const cur = prevMap.get(trace.runId);
              if (!cur || (cur.provider === tp && cur.model === tm)) return prevMap;
              return new Map(prevMap).set(trace.runId, { ...cur, provider: tp, model: tm });
            });
            if (isPrimary) {
              setAgentRun((prev) =>
                prev && prev.runId === trace.runId && (prev.provider !== tp || prev.model !== tm)
                  ? { ...prev, provider: tp, model: tm }
                  : prev,
              );
            }
          }
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
          // Quando arriva un meta_step di fallback / routing context-aware /
          // executor_call, il provider/model effettivo cambia: aggiorniamo
          // anche agentRun cosi' l'indicatore "run: X/Y" nel composer
          // riflette il provider che sta effettivamente girando ORA.
          const m = meta.metaStep;
          const payload = (m.payload ?? {}) as Record<string, unknown>;
          let newProvider: string | null = null;
          let newModel: string | null = null;
          if (m.kind === "fallback") {
            newProvider = (payload.to_provider as string | undefined) ?? null;
            newModel = (payload.to_model as string | undefined) ?? null;
          } else if (m.kind === "executor_call") {
            newProvider = (payload.provider as string | undefined) ?? null;
            newModel = (payload.model as string | undefined) ?? null;
          }
          if (newProvider && newModel) {
            setAgentRun((prev) =>
              prev && prev.runId === runId
                ? { ...prev, provider: newProvider!, model: newModel! }
                : prev,
            );
            setAgentRuns((prevMap) => {
              const cur = prevMap.get(runId);
              if (!cur) return prevMap;
              const next = new Map(prevMap);
              next.set(runId, { ...cur, provider: newProvider!, model: newModel! });
              return next;
            });
          }
        },
        isPrimary ? (text: string) => {
          // Modalita append: accumula tutte le righe di ragionamento (router,
          // executor, tool decisions) in un buffer multi-linea. Il ThinkingBlock
          // le mostra in tempo reale con scroll automatico. Reset su onDone.
          setThinkingText((prev) => {
            const trimmed = text.trim();
            if (!trimmed) return prev;
            return prev ? prev + "\n" + trimmed : trimmed;
          });
        } : undefined,
        (usage) => {
          // Token live (evento agent_usage): aggiorna la barra context in tempo
          // reale durante il run. ctxRatio usa usage.totalPromptTokens = prompt
          // dell'ultima chiamata (riempimento context). Patcha sia la map (per i
          // pannelli) sia agentRun (per il composer) se primary.
          const applyUsage = (run: AgentRunInfo): AgentRunInfo => ({
            ...run,
            usage: {
              ...run.usage,
              totalPromptTokens:
                usage.lastPromptTokens || usage.promptTokens || run.usage?.totalPromptTokens,
              totalCompletionTokens: usage.completionTokens ?? run.usage?.totalCompletionTokens,
              totalTokens: usage.totalTokens ?? run.usage?.totalTokens,
            },
            totalCostUsd: usage.totalCostUsd ?? run.totalCostUsd,
          });
          setAgentRuns((prevMap) => {
            const cur = prevMap.get(runId);
            if (!cur) return prevMap;
            return new Map(prevMap).set(runId, applyUsage(cur));
          });
          if (isPrimary) {
            setAgentRun((prev) => (prev && prev.runId === runId ? applyUsage(prev) : prev));
            // Propaga l'usage live anche al contatore globale (TokenUsageBar nel
            // composer): senza questo, costo/token restano congelati al valore
            // pre-run fino a fine run / refresh manuale. usage.* sono totali
            // cumulativi di sessione (no doppi conteggi).
            setTokenUsage((prev) => ({
              totalTokens: usage.totalTokens ?? prev.totalTokens,
              totalCostUsd: usage.totalCostUsd ?? prev.totalCostUsd,
            }));
          }
        },
      );
    },
    [projectId],
  );

  // Riaggancia un run attivo del backend non ancora noto al client (post-refresh,
  // race, o finestra reflection/generation con generation_ended_at NULL). Usato
  // dal recovery di bootstrap e dal gestore del 409 in send(): senza l'aggancio,
  // agentRun resta null e il drain della coda re-invierebbe subito, ribeccando il
  // 409 in loop. Ritorna true se ha agganciato un run attivo. Punto unico (regola L).
  const reattachActiveRun = useCallback(
    async (sid: string): Promise<boolean> => {
      try {
        const { activeRun } = await getActiveRunForSession(sid);
        if (
          !activeRun ||
          (activeRun.status !== "running" &&
            activeRun.status !== "awaiting_confirmation")
        ) {
          return false;
        }
        const runId = activeRun.runId;
        setAgentRun(activeRun);
        setAgentSteps(activeRun.steps ?? []);
        setAgentRuns((prev) => new Map(prev).set(runId, activeRun));
        setAgentStepsMap((prev) => new Map(prev).set(runId, activeRun.steps ?? []));
        setIsLoading(true);
        subscribeToRun(sid, runId, true);
        return true;
      } catch {
        return false;
      }
    },
    [subscribeToRun],
  );

  const send = useCallback(
    async (content: string, options: SendChatMessageOptions = {}) => {
      if (!hasProject || !isReady || !sessionId || !content.trim()) {
        return;
      }
      // Coda: se un agent run e' in corso (agentRun != null) o una POST e' in volo
      // (isLoading), accoda il messaggio invece di inviarlo subito. L'effect di
      // drain lo inviera' a fine run. Cosi' non si avviano run concorrenti sulla
      // stessa sessione e nessun messaggio viene perso. Gli invii sintetici di
      // auto-continuazione passano da un altro path (non da send), quindi non
      // interferiscono con la coda.
      if (agentRun !== null || isLoading) {
        const trimmed = content.trim();
        setPendingQueue((q) => [...q, { content: trimmed, options }]);
        return;
      }
      // Reset contatore auto-continuazione: ogni messaggio manuale dell'utente
      // azzera il conteggio per permettere nuove sequenze di auto-continuazione.
      autoContinueCountRef.current = 0;
      setAutoContinuePending(false);

      setIsLoading(true);
      setError(null);
      // Reset thinking buffer: il prossimo run partira pulito
      setThinkingText("");
      let isAgentMode = false;
      try {
        const response = await sendChatMessage(sessionId, content.trim(), {
          ...options,
          profileId: options.profileId ?? profileId,
        });

        // Inietta gli allegati salvati (se presenti) nel messaggio utente,
        // cosi' i chip vengono renderizzati subito dal MessageList senza
        // aspettare un refresh manuale.
        const savedAttachments = response.savedAttachments ?? [];
        const enrichedUserMessage = response.userMessage
          ? { ...response.userMessage, attachments: savedAttachments }
          : response.userMessage;

        if (response.agentRun) {
          isAgentMode = true;
          // Modalita' agente: aggiungi solo il messaggio utente, poi ascolta lo stream
          setMessages((current) => [
            ...current,
            ...(enrichedUserMessage ? [enrichedUserMessage] : []),
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
            ...(enrichedUserMessage ? [enrichedUserMessage] : []),
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

        // Propone l'indicizzazione KB SE almeno un allegato e' stato salvato.
        // Il chat-panel mostra un dialog con multi-select; saltabile dall'utente.
        if (savedAttachments.length > 0 && response.userMessage?.id) {
          setAttachmentIndexProposal({
            messageId: response.userMessage.id,
            attachments: savedAttachments,
          });
        }
      } catch (e) {
        // 409 = un run e' gia' attivo sulla sessione ma il client non lo sapeva
        // (race / post-refresh / finestra reflection). Non e' un errore per
        // l'utente: accodiamo il messaggio e ci riagganciamo al run in corso, cosi'
        // il drain lo invia a fine run invece di mostrare "Invio fallito". L'isLoading
        // viene gestito da reattachActiveRun; non lo resettiamo qui.
        const is409 = e instanceof Error && e.message.includes("409");
        if (is409) {
          setPendingQueue((q) => [...q, { content: content.trim(), options }]);
          const attached = await reattachActiveRun(sessionId);
          // attached=true: reattach ha messo isLoading=true (c'e' un run da
          // attendere) e il finally NON deve resettarlo -> isAgentMode=true.
          // attached=false: il run e' gia' finito tra la POST e il riaggancio ->
          // isAgentMode=false fa resettare isLoading nel finally e il drain
          // invia subito il messaggio appena accodato.
          isAgentMode = attached;
          return;
        }
        setError(formatChatError(e, "Invio messaggio fallito."));
        isAgentMode = false;
      } finally {
        if (!isAgentMode) setIsLoading(false);
      }
    },
    [
      hasProject,
      isLoading,
      isReady,
      profileId,
      sessionId,
      subscribeToRun,
      agentRun,
      reattachActiveRun,
    ],
  );

  // Drain della coda messaggi: quando non c'e' piu' un run attivo (agentRun null)
  // ne una POST in volo (!isLoading), invia il primo messaggio accodato. send()
  // setta isLoading in modo sincrono prima del primo await, quindi un solo
  // messaggio per volta viene drenato (niente doppio invio nello stesso tick).
  useEffect(() => {
    if (isLoading || agentRun !== null || pendingQueue.length === 0) return;
    const next = pendingQueue[0];
    setPendingQueue((q) => q.slice(1));
    void send(next.content, next.options);
  }, [isLoading, agentRun, pendingQueue, send]);

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
                if (isStatusTerminal(finalRun.status)) {
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
            undefined,
            (usage) => {
              // Token live anche per i run ripresi dopo conferma (stesso patch
              // della sottoscrizione primaria).
              const applyUsage = (run: AgentRunInfo): AgentRunInfo => ({
                ...run,
                usage: {
                  ...run.usage,
                  totalPromptTokens:
                    usage.lastPromptTokens || usage.promptTokens || run.usage?.totalPromptTokens,
                  totalCompletionTokens: usage.completionTokens ?? run.usage?.totalCompletionTokens,
                  totalTokens: usage.totalTokens ?? run.usage?.totalTokens,
                },
                totalCostUsd: usage.totalCostUsd ?? run.totalCostUsd,
              });
              setAgentRun((prev) => (prev && prev.runId === runId ? applyUsage(prev) : prev));
              setAgentRuns((prevMap) => {
                const cur = prevMap.get(runId);
                if (!cur) return prevMap;
                return new Map(prevMap).set(runId, applyUsage(cur));
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

  /** Chiude la modal di indicizzazione KB senza eseguire alcuna richiesta.
   *  Usato sia dal pulsante "Salta tutto" sia dopo il completamento di una
   *  indicizzazione confermata. */
  const clearAttachmentIndexProposal = useCallback(() => {
    setAttachmentIndexProposal(null);
  }, []);

  /** Marca come indicizzati gli allegati appena confermati: aggiorna sia
   *  `messages` (chip → "verde con icona KB") sia la proposta corrente. */
  const applyAttachmentsIndexed = useCallback(
    (messageId: string, indexed: Array<{ attachmentId: string; kbNoteId: string }>) => {
      if (indexed.length === 0) return;
      const indexedAt = new Date().toISOString();
      const byId = new Map(indexed.map((row) => [row.attachmentId, row.kbNoteId]));
      setMessages((current) =>
        current.map((msg) => {
          if (msg.id !== messageId || !msg.attachments) return msg;
          return {
            ...msg,
            attachments: msg.attachments.map((att) => {
              const kbNoteId = byId.get(att.id);
              if (!kbNoteId) return att;
              return { ...att, kbNoteId, indexedAt };
            }),
          };
        }),
      );
    },
    [],
  );

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
    thinkingText,
    attachmentIndexProposal,
    clearAttachmentIndexProposal,
    applyAttachmentsIndexed,
    // Numero di messaggi accodati in attesa che il run corrente finisca (per
    // mostrare un indicatore "N in coda" nell'input).
    pendingCount: pendingQueue.length,
    send,
    resend,
    remove,
    feedbackError,
    feedbackPositive,
    positiveFeedback,
    confirmAgent,
    cancelRun: useCallback(async (runId?: string) => {
      // Resetta stato UI SUBITO per sbloccare l'input (prima delle chiamate async)
      setAgentRun(null);
      setAgentSteps([]);
      setIsLoading(false);
      // Risoluzione del runId target. Il pulsante "Stop" puo' essere premuto in
      // una finestra in cui `agentRun` lato client e' gia' null (SSE is_final
      // emesso, oppure auto-continuazione in modalita' Continuo) mentre il
      // backend ha ancora un run 'running'. In quel caso `runId` arriva
      // undefined: senza fallback lo Stop sarebbe un no-op e il run resterebbe
      // 'running' nel DB, bloccando la sessione col 409 per 15 min. Chiediamo
      // quindi al server qual e' il run attivo della sessione e cancelliamo
      // quello (il cancel backend e' cascade per sessione: chiude tutti i run
      // attivi residui in un colpo).
      let targetRunId = runId;
      if (!targetRunId && sessionId) {
        try {
          const { activeRun } = await getActiveRunForSession(sessionId);
          targetRunId = activeRun?.runId;
        } catch { /* ignore — niente run attivo da risolvere */ }
      }
      if (!targetRunId) return;
      try { await cancelAgentRun(targetRunId); } catch { /* ignore */ }
      // Recupera il run finale e mostra il messaggio di interruzione nel chat
      try {
        const finalRun = await getAgentRun(targetRunId);
        if (finalRun) {
          const syntheticMsg = createTerminalMessage(finalRun, projectId);
          setMessages((current) => upsertSyntheticAssistantMessage(current, syntheticMsg));
        }
      } catch { /* ignore */ }
    }, [projectId, sessionId]),
    clear,
    clearTraces: () => setTraces([]),
    refresh: bootstrap,
  };
}
