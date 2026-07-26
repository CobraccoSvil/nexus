"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ApiError,
  onBackendRecovered,
  cancelAgentRun,
  confirmAgentRun,
  createChatSession,
  deleteChatMessage,
  feedbackChatMessageError,
  feedbackChatMessagePositive,
  getActiveRunForSession,
  getAgentRun,
  getChatMessages,
  getSessionMetaSteps,
  getSessionTraces,
  getChatSessions,
  getSessionUsage,
  resendChatMessage,
  sendChatMessage,
  subscribeAgentStream,
  isAgentRunLiveOrWaiting,
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
import { upsertSyntheticAssistantMessage, isStatusTerminal, mergeIncomingStep } from "./use-chat/helpers";
import { createTerminalMessage } from "./use-chat/run-summary";
import { formatChatError } from "./use-chat/errors";
import { childRunIdsFromToolResult } from "./use-chat/subagent-runs";

export function useChat(
  projectId = "default",
  profileId = "default",
  opts: { sessionId?: string } = {},
) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(opts.sessionId ?? null);
  const [isLoading, setIsLoading] = useState(false);
  const isLoadingRef = useRef(false);
  useEffect(() => {
    isLoadingRef.current = isLoading;
  }, [isLoading]);
  // POST /messages in volo, NON ancora confermata dal server. Distinta da
  // isLoading (che copre anche il run agentico gia' avviato): finche'
  // isSending=true il messaggio NON e' persistito e la UI deve dirlo
  // ("invio in corso"), non fingere un'elaborazione gia' accettata.
  const [isSending, setIsSending] = useState(false);
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

  // ── Auto-conferma HITL per modalita' "Automatico" ──
  // Se un run in automatic dovesse comunque sospendersi su awaiting_confirmation,
  // approviamo via API (resume checkpoint), MAI con un nuovo messaggio "Continua".
  const autoConfirmCountRef = useRef(0);
  const [autoConfirmRunId, setAutoConfirmRunId] = useState<string | null>(null);
  const confirmingRunIdRef = useRef<string | null>(null);
  const [confirmingRunId, setConfirmingRunId] = useState<string | null>(null);

  const pendingQueueStorageKey = useCallback(
    (sid: string) => `nexus:chat-pending-queue:${sid}`,
    [],
  );

  // ── Coda messaggi ──
  // Messaggi inviati dall'utente MENTRE un agent run e' in corso. Vengono
  // accodati qui e inviati automaticamente a fine run dall'effect di drain (sotto
  // a `send`). Evita run concorrenti sulla stessa sessione — il backend ne
  // rifiuta uno con 409 — e la perdita silenziosa del messaggio osservata quando
  // si inviava un secondo messaggio durante un run ancora attivo.
  const [pendingQueue, setPendingQueue] = useState<
    Array<{ content: string; options: SendChatMessageOptions }>
  >([]);

  // Ripristina la coda messaggi da sessionStorage al cambio sessione (sopravvive
  // al refresh della pagina finche' la sessione e' la stessa).
  useEffect(() => {
    if (!sessionId || typeof window === "undefined") return;
    try {
      const raw = window.sessionStorage.getItem(pendingQueueStorageKey(sessionId));
      if (!raw) return;
      const parsed = JSON.parse(raw) as unknown;
      if (Array.isArray(parsed) && parsed.length > 0) {
        setPendingQueue(parsed as Array<{ content: string; options: SendChatMessageOptions }>);
      }
    } catch {
      // best-effort
    }
  }, [sessionId, pendingQueueStorageKey]);

  // Persiste la coda in sessionStorage ad ogni modifica.
  useEffect(() => {
    if (!sessionId || typeof window === "undefined") return;
    try {
      const key = pendingQueueStorageKey(sessionId);
      if (pendingQueue.length === 0) {
        window.sessionStorage.removeItem(key);
      } else {
        window.sessionStorage.setItem(key, JSON.stringify(pendingQueue));
      }
    } catch {
      // best-effort
    }
  }, [sessionId, pendingQueue, pendingQueueStorageKey]);

  // ── Punto unico contabilita' di sessione (regola L) ────────────────────────
  // La TokenUsageBar (token totali + costo) DEVE leggere sempre dalla stessa
  // fonte autoritativa: l'endpoint backend getSessionUsage
  // (GET /api/billing/session-usage -> billing.rs::get_session_usage), che
  // aggrega i metadata per-messaggio in DB con la semantica corretta:
  //   - total_tokens: solo messaggi VIVI (deleted_at IS NULL) -> il context %
  //     scende dopo un compact.
  //   - total_cost: TUTTI i messaggi, inclusi i soft-deleted dalla compattazione
  //     -> il costo e' CUMULATIVO (gia' speso) e non si azzera compattando.
  // Prima del fix il reload ricalcolava il costo lato client filtrando i
  // soft-deleted (e l'invio sincrono lo accumulava in modo incrementale),
  // ri-introducendo il "bug storico" che il backend gia' risolve: live e reload
  // divergevano (costo che SCENDE mentre i token SALGONO sulla stessa sessione).
  // Ora reload, fine-run e send sincrono delegano tutti a questo punto unico.
  // Ritorna i totali per i chiamanti che devono riusarli (es. patch del
  // messaggio sintetico terminale), oppure null se la lettura fallisce.
  const refreshSessionUsage = useCallback(
    async (sid: string): Promise<{ totalTokens: number; totalCostUsd: number } | null> => {
      try {
        const usage = await getSessionUsage(sid);
        setTokenUsage({
          totalTokens: usage.totalTokens,
          totalCostUsd: usage.totalCostUsd,
        });
        return { totalTokens: usage.totalTokens, totalCostUsd: usage.totalCostUsd };
      } catch {
        // best-effort: la barra resta sull'ultimo valore noto; il prossimo turno
        // (o un reload) la riallinea. Mai bloccare il flusso chat per un
        // fallimento di lettura della contabilita'.
        return null;
      }
    },
    [],
  );

  // ── Binding dispatcher: TokenUsageBar e tokenUsage si aggiornano in
  // ── real-time SENZA refresh browser quando il backend emette eventi chat.
  //
  // - ChatSessionCompacted: il backend invia totali freschi dopo compact;
  //   riallineiamo subito la barra (caso bug "percentuale solo dopo F5").
  // - ChatMessageAdded: il backend invia totali assoluti aggiornati ad ogni
  //   INSERT messaggio (emesso da chat_messages/run.rs, con i totali del
  //   payload). Sui messaggi senza contabilita' i totali sono null e la barra
  //   resta sull'ultimo valore noto.
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
        // Lista NON vuota richiesta: un array vuoto (es. lettura instradata su
        // un pool sbagliato durante il fallback di routing) NON deve
        // sovrascrivere la storia visibile — "la chat sparisce" (2026-07-02).
        if (history.messages && history.messages.length > 0) {
          setMessages(history.messages);
        }
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
    // Totali assoluti dal backend (idempotente, non incrementale).
    // Il test e' sul TIPO, non su `!== undefined`: il backend manda `null` per i
    // messaggi senza contabilita' (disambiguazione), e un null passava il vecchio
    // controllo azzerando la barra a meta' conversazione.
    if (typeof lastMessage.totalTokens === "number" && typeof lastMessage.totalCostUsd === "number") {
      setTokenUsage({
        totalTokens: lastMessage.totalTokens,
        totalCostUsd: lastMessage.totalCostUsd,
      });
    }
    // Trigger sul messageId; lastMessage stesso e' read solo per i totalTokens/cost
    // del messaggio corrente.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastMessage?.messageId, sessionId]);


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
      // Ripristino tracce gateway LLM (AITraceEvent) a due livelli, convergenti
      // sul DB (regola L). sessionStorage resta cache opportunistica (veloce,
      // per-dispositivo, volatile): lo usiamo come primo riempimento per evitare
      // un pannello vuoto durante la fetch. Subito dopo il DB
      // (nexus_agent_traces, mig 0485) e' autoritativo e SOVRASCRIVE: cosi' un
      // reload in un altro tab/dispositivo o dopo pulizia storage converge col
      // rendering live invece di divergere.
      try {
        const saved = sessionStorage.getItem(`nexus:traces:${activeSessionId}`);
        if (saved) {
          const parsed = JSON.parse(saved) as AITraceEvent[];
          if (Array.isArray(parsed) && parsed.length > 0) {
            setTraces(parsed);
          }
        }
      } catch { /* ignore */ }
      try {
        const { runs } = await getSessionTraces(activeSessionId);
        const runEntries = runs ? Object.values(runs) : [];
        if (runEntries.length > 0) {
          // Le tracce arrivano raggruppate per runId (ordine HashMap non
          // garantito tra run). Appiattiamo in un'unica timeline ordinata per
          // timestamp dell'AITraceEvent: e' la stessa forma di `traces` prodotta
          // live dall'accumulo SSE. Cap a 100 (FIFO) come il path live.
          const flat = runEntries
            .flat()
            .filter((tr): tr is AITraceEvent => Boolean(tr && tr.runId))
            .sort(
              (a, b) =>
                new Date(a.timestamp ?? 0).getTime() - new Date(b.timestamp ?? 0).getTime(),
            );
          const capped = flat.length > 100 ? flat.slice(flat.length - 100) : flat;
          setTraces(capped);
        }
      } catch {
        // best-effort: il pannello resta sull'eventuale cache sessionStorage
      }
      const history = await getChatMessages(activeSessionId);
      setMessages(history.messages ?? []);
      // Ripristina la timeline meta_step persistita (plan/routing/clarify/
      // fallback/reflection/next_actions): gli eventi SSE vivono solo in memoria
      // e si perdono al refresh, qui li rileggiamo dal DB (nexus_agent_meta_steps)
      // cosi' la presentazione della chat resta identica prima e dopo un reload.
      try {
        const meta = await getSessionMetaSteps(activeSessionId);
        const entries = meta.runs ? Object.entries(meta.runs) : [];
        if (entries.length > 0) setMetaStepsMap(new Map(entries));
      } catch {
        // best-effort: la chat funziona comunque, solo senza timeline storica
      }
      // Contabilita' di sessione dalla fonte autoritativa (punto unico, regola
      // L). NON ri-sommare msg.totalCost lato client: lo faceva con un filtro
      // deletedAt che azzerava il costo dei turni compattati, divergendo dal
      // valore live. getSessionUsage applica gia' la semantica corretta
      // (token solo vivi per il ctx%, costo cumulativo incluso il soft-deleted).
      await refreshSessionUsage(activeSessionId);
      setIsReady(true);
    } catch (e) {
      setError(formatChatError(e, "Impossibile inizializzare la chat."));
      // NON svuotare una storia gia' caricata: un refresh fallito (backend
      // occupato, fetch transitoria KO) faceva "sparire" la chat pur avendo i
      // messaggi in memoria (incidente 2026-07-02). Si azzera solo se non
      // avevamo ancora nulla (primo bootstrap fallito).
      setMessages((current) => (current.length > 0 ? current : []));
      setSessionId((current) => current ?? null);
      setIsReady(false);
    } finally {
      setIsLoading(false);
    }
  }, [hasProject, projectId, opts.sessionId, refreshSessionUsage]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  // Cleanup della subscription SSE PRIMARIA corrente. subscribeAgentStream
  // ritorna una funzione che chiude l'EventSource e ferma il loop di reconnect;
  // la conserviamo qui per poterla chiudere PRIMA di aprirne un'altra sullo
  // stesso canale primario. Senza questo, un riaggancio (recovery del backend,
  // reattach post-409, reattach in onDone) apriva un secondo EventSource mentre
  // il primo era ancora in reconnect-loop -> due stream concorrenti sullo stesso
  // run, doppioni di step e usage. Le subscription FIGLIE (sub-agenti) non sono
  // tracciate qui: vivono legate al ciclo del run padre.
  const primarySubCleanupRef = useRef<(() => void) | null>(null);

  /** Cleanup delle subscription FIGLIE (una per sub-agente), per runId.
   *
   *  Prima venivano semplicemente scartate (`if (isPrimary)`), quindi ogni
   *  EventSource figlio sopravviveva al componente col proprio loop di reconnect.
   *  Un batch di sub-agenti ne apre 8 di default (fino a 32), e ogni riaggancio
   *  del padre li ri-sottoscriveva TUTTI senza chiudere i precedenti: su un
   *  budget di 6 connessioni per origine (HTTP/1.1) questo affamava le fetch
   *  normali, e il bootstrap della chat non completava (pulsante Invia muto).
   *  Tenerli qui permette di chiuderli e di non riaprirli due volte. */
  const childSubCleanupsRef = useRef<Map<string, () => void>>(new Map());

  /** Chiude TUTTE le subscription figlie ancora aperte. */
  const stopChildAgentStreams = useCallback(() => {
    for (const cleanup of childSubCleanupsRef.current.values()) {
      try {
        cleanup();
      } catch {
        // best-effort: un cleanup che fallisce non deve impedire gli altri
      }
    }
    childSubCleanupsRef.current.clear();
  }, []);

  /** Chiude la subscription SSE primaria e azzera il banner "Connessione persa".
   *  Punto unico (regola L): ogni chiusura del canale run (watchdog, reattach,
   *  cancel, cambio run) deve passare da qui, altrimenti il loop di reconnect
   *  resta attivo con isReconnecting=true anche senza agentRun in UI.
   *  Chiude anche le figlie: vivono legate al ciclo del run padre, quindi quando
   *  il padre se ne va non hanno piu' motivo di restare aperte. */
  const stopPrimaryAgentStream = useCallback(() => {
    if (primarySubCleanupRef.current) {
      primarySubCleanupRef.current();
      primarySubCleanupRef.current = null;
    }
    stopChildAgentStreams();
    setIsReconnecting(false);
  }, [stopChildAgentStreams]);

  // Sottoscrive a un run (primario o figlio) e aggiorna le map di stato
  const subscribeToRun = useCallback(
    (sid: string, runId: string, isPrimary: boolean) => {
      // Chiudi la subscription primaria precedente prima di aprirne una nuova
      // (idempotenza del riaggancio: mai due EventSource primari attivi).
      if (isPrimary) {
        stopPrimaryAgentStream();
      } else if (childSubCleanupsRef.current.has(runId)) {
        // Stesso principio per i figli: a ogni riaggancio del padre arrivano di
        // nuovo gli stessi child_run_ids, e senza questo controllo si apriva un
        // secondo EventSource sullo stesso sub-run lasciando aperto il primo --
        // il moltiplicatore che portava le connessioni ben oltre il budget.
        return;
      }
      const cleanup = subscribeAgentStream(
        sid,
        runId,
        (event) => {
          if (!event.step) return; // eventi trace non hanno step
          // Difesa FIX 4: piu' step possono arrivare con lo stesso stepIndex
          // (es. indice 0 ripetuto). mergeIncomingStep (punto unico, regola L)
          // non collassa step distinti: correla per toolId quando presente,
          // altrimenti per stepIndex senza sovrascrivere uno step gia' terminato
          // con uno nuovo `running`.
          setAgentStepsMap((prev) => {
            const current = prev.get(runId) ?? [];
            return new Map(prev).set(runId, mergeIncomingStep(current, event.step!));
          });
          if (isPrimary) {
            setAgentSteps((prev) => mergeIncomingStep(prev, event.step!));
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
          // Sub-run avviati da questo step: gli id arrivano dal campo
          // strutturato `subagent_run_id` del tool_result (punto unico
          // childRunIdsFromToolResult), mai dal testo del messaggio.
          for (const childRunId of childRunIdsFromToolResult(
            event.step.toolName,
            event.step.toolResult,
          )) {
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
        },
        async () => {
          // Un sub-run CONCLUSO non ha piu' nulla da trasmettere: il suo stream
          // resta pero' agganciato fino alla fine del padre, e con un batch di
          // sub-agenti quegli slot pesano sul budget di 6 connessioni per origine
          // proprio mentre il lavoro e' al massimo. I sub-agenti finiscono a
          // scaglioni, quindi liberare qui abbassa il PICCO, che e' la grandezza
          // che conta. Gli step gia' ricevuti restano in agentStepsMap: il tab del
          // sub-agente continua a mostrarli. Il primario NON si tocca (il suo
          // ciclo di vita e' governato da stopPrimaryAgentStream).
          if (!isPrimary) {
            const chiudi = childSubCleanupsRef.current.get(runId);
            if (chiudi) {
              childSubCleanupsRef.current.delete(runId);
              try {
                chiudi();
              } catch {
                // best-effort: la chiusura non deve impedire la finalizzazione
              }
            }
          }
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
            if (!dbIsTerminal && isAgentRunLiveOrWaiting(finalRun.status)) {
              // Stream chiuso ma il run e' ancora vivo (HITL, fan-in, running):
              // NON forzare failed — mantieni lo stato reale dal DB.
              setAgentRuns((prev) => new Map(prev).set(runId, finalRun));
              setAgentStepsMap((prev) => new Map(prev).set(runId, finalRun.steps));
              if (isPrimary) {
                setAgentRun(finalRun);
                setAgentSteps(finalRun.steps);
                setIsLoading(finalRun.status === "running");
              }
              return;
            }
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
                  const usage = await refreshSessionUsage(sid);
                  if (usage && usage.totalTokens > 0 && syntheticMsg) {
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
                  autoConfirmCountRef.current < 3
                ) {
                  autoConfirmCountRef.current += 1;
                  setAutoConfirmRunId(runId);
                } else {
                  // Run completato/failed/cancelled: STOP. L'utente puo' digitare
                  // "Continua" manualmente se vuole proseguire.
                  autoConfirmCountRef.current = 0;
                }
                // REATTACH automatico al run attivo della sessione. Se il backend
                // ha GIA' un ALTRO run in corso (l'agente si e' rilanciato come
                // Debugger, oppure questo run e' stato superato da un nuovo run dello
                // stesso turno), agganciamolo cosi' la chat continua a mostrarne il
                // lavoro SENZA che l'utente debba fare un refresh manuale (causa
                // radice del bug "si ferma e riparte solo dopo refresh" quando i run
                // si succedono). NB: NON e' auto-continue (quello CREA un run e
                // brucia token, vedi sopra): qui ci agganciamo solo a un run GIA'
                // attivo nel backend — esattamente cio' che farebbe un refresh.
                try {
                  const { activeRun } = await getActiveRunForSession(sid);
                  if (
                    activeRun &&
                    activeRun.runId !== runId &&
                    !isStatusTerminal(activeRun.status)
                  ) {
                    setAgentRun(activeRun);
                    setAgentRuns((prev) => new Map(prev).set(activeRun.runId, activeRun));
                    setAgentStepsMap((prev) =>
                      prev.has(activeRun.runId) ? prev : new Map(prev).set(activeRun.runId, []),
                    );
                    setIsLoading(true);
                    subscribeToRun(sid, activeRun.runId, true);
                  }
                } catch { /* nessun run attivo o backend giu': stop normale */ }
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
          if (m.kind === "awaiting_confirmation") {
            const pendingRaw = payload.pending_actions;
            const pendingActions = Array.isArray(pendingRaw) ? pendingRaw : [];
            const patchRun = (run: AgentRunInfo): AgentRunInfo => ({
              ...run,
              status: "awaiting_confirmation",
              pendingActions: pendingActions as AgentRunInfo["pendingActions"],
            });
            setAgentRuns((prevMap) => {
              const cur = prevMap.get(runId);
              if (!cur) return prevMap;
              const next = new Map(prevMap).set(runId, patchRun(cur));
              if (
                isPrimary &&
                cur.automationMode === "automatic" &&
                autoConfirmCountRef.current < 3
              ) {
                autoConfirmCountRef.current += 1;
                setAutoConfirmRunId(runId);
              }
              return next;
            });
            if (isPrimary) {
              setAgentRun((prev) => (prev && prev.runId === runId ? patchRun(prev) : prev));
              setIsLoading(false);
            }
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
              // Riempimento contesto (ratio ctx%): il motore nativo emette
              // promptTokens PER-TURNO nell'evento agent_usage, il ponte brain
              // emette lastPromptTokens esplicito. Campo dedicato, mai il
              // cumulativo di billing.
              lastPromptTokens:
                usage.lastPromptTokens ?? usage.promptTokens ?? run.usage?.lastPromptTokens,
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
      // Conserva il cleanup: quello primario nel suo ref, quelli figli nella
      // mappa per runId. Scartare i cleanup figli (com'era) lasciava un
      // EventSource orfano per ogni sub-agente, col suo loop di reconnect.
      if (isPrimary) {
        primarySubCleanupRef.current = cleanup;
      } else {
        childSubCleanupsRef.current.set(runId, cleanup);
      }
    },
    [projectId, refreshSessionUsage, stopPrimaryAgentStream],
  );

  // Riaggancia un run attivo del backend non ancora noto al client (post-refresh,
  // race, o finestra reflection/generation con generation_ended_at NULL). Usato
  // dal recovery di bootstrap e dal gestore del 409 in send(): senza l'aggancio,
  // agentRun resta null e il drain della coda re-invierebbe subito, ribeccando il
  // 409 in loop. Ritorna true se ha agganciato un run attivo. Punto unico (regola L).
  const reattachActiveRun = useCallback(
    async (sid: string): Promise<boolean> => {
      // Ferma subito il reconnect-loop corrente (non aspettare getActiveRun):
      // evita stream orfani e banner "Connessione persa" infinito.
      stopPrimaryAgentStream();
      try {
        const { activeRun } = await getActiveRunForSession(sid);
        if (!activeRun || !isAgentRunLiveOrWaiting(activeRun.status)) {
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
    [subscribeToRun, stopPrimaryAgentStream],
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
      // Reset contatore auto-conferma HITL: ogni messaggio manuale dell'utente
      // azzera il conteggio per permettere nuove sequenze di auto-conferma.
      autoConfirmCountRef.current = 0;
      setAutoConfirmRunId(null);

      setIsLoading(true);
      setIsSending(true);
      setError(null);
      // Reset thinking buffer: il prossimo run partira pulito
      setThinkingText("");
      let isAgentMode = false;
      try {
        // sendChatMessage e' idempotente (clientMessageId) e ritenta da solo
        // sugli errori di trasporto/5xx: quando risolve, il messaggio E'
        // persistito lato server (response.userMessage.id); quando rigetta,
        // l'invio e' definitivamente fallito e l'errore diventa visibile.
        const response = await sendChatMessage(sessionId, content.trim(), {
          ...options,
          profileId: options.profileId ?? profileId,
        });
        setIsSending(false);

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
            // Riallinea sulla fonte autoritativa invece di accumulare lato
            // client: l'incrementale `prev + cost` partiva dal valore corrente
            // (eventualmente gia' divergente) e si scollava dal totale di
            // sessione dopo un reload. getSessionUsage e' il punto unico.
            await refreshSessionUsage(sessionId);
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
        // Status dallo stato strutturato di ApiError (regola M), mai dal testo.
        const is409 = e instanceof ApiError && e.status === 409;
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
        setIsSending(false);
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
      refreshSessionUsage,
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
      if (!messageId) return;
      // Un run gia' in corso (POST in volo O agentRun attivo) rendeva il click
      // "Reinvia" un NO-OP MUTO: sembrava non fare nulla e la richiesta non
      // compariva in testa. Il `send` normale accoda invece di perdere il
      // messaggio; il resend avvia un nuovo flusso che confligge con un run
      // attivo, quindi diamo feedback ESPLICITO (niente perdita silenziosa).
      if (isLoading || agentRun) {
        setError(
          "Un run è già in corso: fermalo con Stop prima di reinviare la richiesta.",
        );
        return;
      }
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
    [isLoading, agentRun, profileId, subscribeToRun],
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
    stopPrimaryAgentStream();
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
  }, [stopPrimaryAgentStream]);

  const confirmAgent = useCallback(
    async (runId: string, approved: boolean) => {
      if (confirmingRunIdRef.current === runId) return;
      confirmingRunIdRef.current = runId;
      setConfirmingRunId(runId);
      setError(null);
      try {
        const result = await confirmAgentRun(runId, approved);
        if (!approved) {
          stopPrimaryAgentStream();
          setAgentRun((prev) => (prev ? { ...prev, status: "cancelled" } : null));
          setIsLoading(false);
          return;
        }
        const nextStatus = result.status;
        if (nextStatus === "failed" || nextStatus === "failed_diagnosed") {
          stopPrimaryAgentStream();
          try {
            const finalRun = await getAgentRun(runId);
            setAgentRuns((prev) => new Map(prev).set(runId, finalRun));
            setAgentRun(null);
            setAgentSteps([]);
            setIsLoading(false);
            // `finalAnswer` NON e' un errore JS: e' l'esito che il run ha
            // dichiarato, gia' prosa. Farlo passare da formatChatError significava
            // fabbricare un Error attorno a un testo per poi troncarlo — e da
            // quando la frase si legge da un CAMPO (mai dal testo), quel testo
            // verrebbe scartato del tutto e l'utente perderebbe il motivo vero.
            setError(finalRun.finalAnswer?.trim() || "Conferma fallita: il run e' terminato con errore.");
          } catch {
            setAgentRun(null);
            setAgentSteps([]);
            setIsLoading(false);
            setError("Conferma fallita: il run non e' riuscito a riprendere.");
          }
          return;
        }
        setAgentRun((prev) =>
          prev && prev.runId === runId
            ? { ...prev, status: nextStatus === "awaiting_confirmation" ? "awaiting_confirmation" : "running", pendingActions: [] }
            : prev,
        );
        setAgentRuns((prev) => {
          const run = prev.get(runId);
          if (!run) return prev;
          return new Map(prev).set(runId, {
            ...run,
            status: nextStatus === "awaiting_confirmation" ? "awaiting_confirmation" : "running",
            pendingActions: [],
          });
        });
        setIsLoading(true);
        if (sessionId) {
          subscribeToRun(sessionId, runId, true);
        }
      } catch (e) {
        if (e instanceof ApiError && e.status === 409 && sessionId) {
          try {
            const current = await getAgentRun(runId);
            setAgentRuns((prev) => new Map(prev).set(runId, current));
            if (isAgentRunLiveOrWaiting(current.status)) {
              setAgentRun({ ...current, pendingActions: [] });
              setAgentSteps(current.steps ?? []);
              setIsLoading(current.status === "running" || current.status === "awaiting_subagents");
              if (current.status === "running" || current.status === "awaiting_subagents") {
                subscribeToRun(sessionId, runId, true);
              }
              return;
            }
            stopPrimaryAgentStream();
            setAgentRun(null);
            setAgentSteps([]);
            setIsLoading(false);
            return;
          } catch {
            // fallback al messaggio originale
          }
        }
        setError(formatChatError(e, "Conferma fallita."));
        setIsLoading(false);
      } finally {
        if (confirmingRunIdRef.current === runId) {
          confirmingRunIdRef.current = null;
          setConfirmingRunId(null);
        }
      }
    },
    [sessionId, subscribeToRun, stopPrimaryAgentStream],
  );

  // Auto-conferma HITL per run in modalita' "automatic" (punto unico: resume API).
  useEffect(() => {
    if (!autoConfirmRunId || !sessionId) return;
    const runId = autoConfirmRunId;
    setAutoConfirmRunId(null);
    void confirmAgent(runId, true);
  }, [autoConfirmRunId, confirmAgent, sessionId]);

  // Dopo bootstrap: riconnetti all'agente in corso (se il browser è stato refreshato mentre girava)
  useEffect(() => {
    if (!isReady || !sessionId) return;
    // Solo al primo mount (agentRun è ancora null): cerca un run attivo nel DB
    if (agentRun !== null) return;
    let cancelled = false;
    getActiveRunForSession(sessionId).then(({ activeRun }) => {
      if (cancelled || !activeRun) return;
      if (!isAgentRunLiveOrWaiting(activeRun.status)) return;
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

  // Watchdog di liveness (robustezza, regola H). subscribeAgentStream ha gia'
  // reconnect+poll+replay, MA tutti reagiscono a `onerror` dell'EventSource. Se
  // lo stream muore in modo SILENZIOSO — tab in background sospeso dal browser,
  // connessione zombie che non emette onerror — onDone/handleDisconnect non
  // scattano mai e la UI resta su "Sto interrogando" all'infinito anche se il run
  // e' gia' terminato nel DB (incidente Beauty-Book: run completed/failed, ma la
  // chat congelata). Questo watchdog e' INDIPENDENTE dallo stream: mentre c'e' un
  // run attivo, riconcilia con lo stato reale del DB (a intervalli regolari e al
  // ritorno sul tab) e, se terminale, sblocca la UI come farebbe onDone. Le
  // chiamate di chiusura sono idempotenti: una corsa con onDone e' innocua.
  //
  // Si arma su agentRun (NON su isLoading): l'onDone dello stream, se la
  // getAgentRun finale fallisce per rete, resetta isLoading MA lascia agentRun
  // valorizzato. In quello stato "fantasma" (agentRun!=null, isLoading=false)
  // la coda pendingQueue resta bloccata per sempre: il drain richiede
  // agentRun==null e col vecchio guard su isLoading il watchdog non girava piu'
  // (causa radice del messaggio accodato mai inviato dopo una riconnessione).
  useEffect(() => {
    if (!sessionId) return;
    const runId = agentRun?.runId;
    if (!runId) return;
    let cancelled = false;

    const reconcile = async () => {
      if (cancelled || (typeof document !== "undefined" && document.visibilityState === "hidden")) {
        return;
      }
      try {
        const finalRun = await getAgentRun(runId);
        if (cancelled) return;
        if (isStatusTerminal(finalRun.status)) {
          stopPrimaryAgentStream();
          setAgentRuns((prev) => new Map(prev).set(runId, finalRun));
          setAgentStepsMap((prev) => new Map(prev).set(runId, finalRun.steps));
          setAgentRun(null);
          setAgentSteps([]);
          setIsLoading(false);
          const syntheticMsg = createTerminalMessage(finalRun, projectId);
          setMessages((current) => upsertSyntheticAssistantMessage(current, syntheticMsg));
          return;
        }
        // Run ancora vivo nel DB ma stream/UI congelati (stato fantasma):
        // riaggancia SSE + step dal DB cosi' la coda puo' drenarsi a fine run.
        if (isAgentRunLiveOrWaiting(finalRun.status) && !isLoadingRef.current) {
          setAgentRuns((prev) => new Map(prev).set(runId, finalRun));
          setAgentStepsMap((prev) => new Map(prev).set(runId, finalRun.steps ?? []));
          setAgentRun(finalRun);
          setAgentSteps(finalRun.steps ?? []);
          setIsLoading(true);
          subscribeToRun(sessionId, runId, true);
        }
      } catch {
        // Backend irraggiungibile: riprova al tick successivo (non arrenderti).
      }
    };

    const interval = setInterval(reconcile, 20_000);
    const onVisible = () => {
      if (document.visibilityState === "visible") void reconcile();
    };
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", onVisible);
    }
    return () => {
      cancelled = true;
      clearInterval(interval);
      if (typeof document !== "undefined") {
        document.removeEventListener("visibilitychange", onVisible);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, agentRun?.runId, projectId, stopPrimaryAgentStream]);

  // Risync SSE al ritorno del backend (fix "chat cieca sul run dopo restart di
  // mcp-core"). Il canale agent-stream ha gia' reconnect con backoff, ma dopo un
  // restart puo' restare agganciato a un run_id ormai terminale/sostituito e non
  // sa qual e' il run attivo ORA. Il punto unico health-monitor emette un segnale
  // down->up: qui ri-verifichiamo il run attivo della sessione dal server e
  // riagganciamo la subscription (reattachActiveRun chiude quella precedente via
  // primarySubCleanupRef e ne apre una nuova sul run corrente, con lo stato
  // risincronizzato dal replay backend — non solo i delta futuri). Se non c'e'
  // piu' un run attivo, il watchdog sopra e l'agent_final del replay chiudono la
  // UI: nessuna azione qui.
  useEffect(() => {
    if (!sessionId) return;
    const unsub = onBackendRecovered(() => {
      void reattachActiveRun(sessionId);
    });
    return unsub;
  }, [sessionId, reattachActiveRun]);

  // Chiude la subscription SSE primaria all'unmount del componente: senza questo
  // un EventSource (con il suo loop di reconnect) resterebbe orfano dopo la
  // smontatura della chat. finish e' idempotente, quindi e' innocuo anche se lo
  // stream era gia' concluso.
  useEffect(() => {
    return () => {
      stopPrimaryAgentStream();
    };
  }, [stopPrimaryAgentStream]);

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
    isSending,
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
    confirmingRunId,
    send,
    resend,
    remove,
    feedbackError,
    feedbackPositive,
    positiveFeedback,
    confirmAgent,
    cancelRun: useCallback(async (runId?: string) => {
      // Resetta stato UI SUBITO per sbloccare l'input (prima delle chiamate async)
      stopPrimaryAgentStream();
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
    }, [projectId, sessionId, stopPrimaryAgentStream]),
    clear,
    clearTraces: () => setTraces([]),
    refresh: bootstrap,
  };
}
