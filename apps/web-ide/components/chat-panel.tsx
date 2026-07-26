"use client";

import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useCallback,
  type FormEvent,
} from "react";
import { useChat } from "../lib/use-chat";
import { listProjectMemories, getProjectDbConfig, listAdminSettings, getModels, indexAttachmentsToKb, type AITraceEvent, type ChatAttachment, type ModelCatalogEntry, type PrecheckResult, type ProjectDbConfig, type SimilarHit } from "../lib/api-client";
import { SimilarRequestBanner } from "./knowledge/similar-request-banner";
import { computeContextFill } from "../lib/context-fill";
import { isStatusTerminal } from "../lib/use-chat/helpers";
import { isAgentRunLiveOrWaiting } from "../lib/api/agent";
import { useThemeColors } from "../lib/theme";
import { useI18n } from "../lib/i18n";
import { useGlobalDialog } from "./global-dialog-provider";
import { FeedbackErrorDialog } from "./feedback-error-dialog";
import { IconButton } from "./icon-button";
import { MessageList } from "./chat/message-list";
import { useActivityStreamEnabled } from "../lib/use-chat/activity-stream-flag";
import { composeActivityStream, tracesForRun, latestAwaitingSubagentsCount, type FoldThreshold } from "../lib/use-chat/activity-stream";
import { RunNotifications } from "./chat/run-notifications";
import { SessionWorklogPanel } from "./chat/session-worklog-panel";
import { AgentStepsPanel } from "./chat/agent-steps-panel";
import { extractLatestNextActions } from "./chat/agent-meta-step-card";
import { InlineTracePanel } from "./chat/inline-trace-panel";
import { Composer } from "./chat/composer";
import { MemoryPanel } from "./chat/memory-panel";
import { TokenUsageBar } from "./chat/token-usage-bar";
import {
  AgentPreparingBubble,
  ThinkingBlock,
  AgentProgressInline,
} from "./chat/agent-status-bubbles";
import { AttachmentIndexDialog } from "./chat/attachment-index-dialog";
import { ProviderUnavailableBanner } from "./chat/provider-unavailable-banner";
import { ConnectionStatusBanner } from "./chat/connection-status-banner";
import { PrecheckSuggestion } from "./chat/precheck-suggestion";
import { AgentActivityBar } from "./chat/agent-activity-bar";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// Limite singolo allegato in chat. Aumentato da 2 MB a 25 MB perche' 2 MB
// erano scomodi per immagini di schermate, dump JSON, codice voluminoso.
// Il backend Axum ha DefaultBodyLimit::max(50 MB) (vedi mcp-core main.rs),
// che lascia margine per il base64 expansion (~33 MB) + resto del JSON.
const MAX_ATTACHMENT_BYTES = 25_000_000;

function extractRunningCommand(toolInput: Record<string, unknown> | undefined): string | null {
  if (!toolInput) return null;
  const direct = ["command", "cmd", "text", "script"]
    .map((key) => toolInput[key])
    .find((value) => typeof value === "string" && value.trim().length > 0) as
    | string
    | undefined;
  if (direct) return direct.trim();

  const args = toolInput.args;
  if (Array.isArray(args) && args.length > 0) {
    const joined = args
      .map((item) => (typeof item === "string" ? item : ""))
      .filter((item) => item.length > 0)
      .join(" ");
    return joined.trim() || null;
  }
  return null;
}

interface SpeechRecognitionResultLike {
  readonly isFinal: boolean;
  readonly 0: { readonly transcript: string };
}

interface SpeechRecognitionEventLike extends Event {
  readonly results: ArrayLike<SpeechRecognitionResultLike>;
}

interface SpeechRecognitionLike extends EventTarget {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: SpeechRecognitionEventLike) => void) | null;
  onerror: (() => void) | null;
  onend: (() => void) | null;
  start(): void;
  stop(): void;
}

declare global {
  interface Window {
    SpeechRecognition?: new () => SpeechRecognitionLike;
    webkitSpeechRecognition?: new () => SpeechRecognitionLike;
  }
}

/* ------------------------------------------------------------------ */
/* ChatPanel                                                            */
/* ------------------------------------------------------------------ */

export function ChatPanel({
  projectId = "default",
  profileId = "default",
  activeFiles = [],
  sessionId,
  onAgentActivityChange,
  onCtxRatioChange,
  selectedProvider,
  setSelectedProvider,
  selectedModel,
  setSelectedModel,
  providerModels,
  availableProviders,
  automationMode,
  setAutomationMode,
  supervisorMode = "none",
  setSupervisorMode,
  showMemory,
  setShowMemory,
  externalInput,
  externalAutoSend,
  externalProviderHint,
  externalAgentTypeHint,
  externalAutomationOverride,
  onExternalInputConsumed,
  onTracesChange,
  hasRunningServices = false,
  onRunEnd,
}: {
  projectId?: string;
  profileId?: string;
  activeFiles?: string[];
  sessionId?: string;
  onAgentActivityChange?: (active: boolean) => void;
  /** Notifica al parent il ratio (0..1+) di riempimento context_window
   *  dell'ultimo turno; null se non disponibile. Usato dall'ide-shell per
   *  mostrare la % sul bottone "Compatta chat". */
  onCtxRatioChange?: (ratio: number | null) => void;
  selectedProvider: string;
  setSelectedProvider: (v: string) => void;
  selectedModel: string;
  setSelectedModel: (v: string) => void;
  providerModels: string[];
  availableProviders: string[];
  automationMode: "study" | "confirm" | "automatic";
  setAutomationMode: (v: "study" | "confirm" | "automatic") => void;
  supervisorMode?: "none" | "anomaly" | "interleaved" | "continuous";
  setSupervisorMode?: (v: "none" | "anomaly" | "interleaved" | "continuous") => void;
  showMemory: boolean;
  setShowMemory: (v: boolean) => void;
  externalInput?: string;
  externalAutoSend?: boolean;
  externalProviderHint?: { provider?: string; model?: string };
  /** Hint strutturale sul tipo di agente per questo invio esterno (es. "debugger"
   *  dai pannelli error-fix). Propagato come `agentTypeHint` nel POST: bypassa la
   *  disambiguazione d'intent A/B lato backend. */
  externalAgentTypeHint?: string;
  /** Se impostato con input esterno, questo invio usa la modalità indicata (es. `confirm` da pannello debug). */
  externalAutomationOverride?: "study" | "confirm" | "automatic";
  onExternalInputConsumed?: () => void;
  onTracesChange?: (traces: AITraceEvent[]) => void;
  hasRunningServices?: boolean;
  onRunEnd?: (run: { provider: string; model: string; status: string }) => void;
}) {
  const tc = useThemeColors();
  const { t } = useI18n();
  const { confirmDialog } = useGlobalDialog();
  // ADR 0037: flag del nastro attivita' (default OFF -> rendering odierno).
  const activityStreamEnabled = useActivityStreamEnabled();
  // Soglia densita' del collasso tool, derivata dalla larghezza REALE del
  // pannello chat (stessa fonte delle @container query): compatto <=380 -> 2,
  // esteso >=600 -> 4, medio -> 3. Osservata via ResizeObserver sullo scrollRef.
  const [foldThreshold, setFoldThreshold] = useState<FoldThreshold>(3);
  const [feedbackDialog, setFeedbackDialog] = useState<{ messageId: string; content: string } | null>(null);
  const hasProject = UUID_RE.test(projectId);
  const [activeMemoryCount, setActiveMemoryCount] = useState(0);
  const [dbStatus, setDbStatus] = useState<ProjectDbConfig | null>(null);
  const [similarHits, setSimilarHits] = useState<SimilarHit[]>([]);
  useEffect(() => {
    if (!hasProject) return;
    listProjectMemories(projectId!).then(res => {
      setActiveMemoryCount(res.memories.filter(m => m.active).length);
    }).catch(() => {});
  }, [projectId, hasProject, showMemory]);
  useEffect(() => {
    if (!hasProject) return;
    getProjectDbConfig(projectId!).then(cfg => {
      if (cfg.configured) setDbStatus(cfg);
    }).catch(() => {});
  }, [projectId, hasProject]); // ricarica anche quando il pannello viene chiuso
  const [narrationWarnAfterMs, setNarrationWarnAfterMs] = useState<number | undefined>(undefined);
  const [narrationWarnAfterChars, setNarrationWarnAfterChars] = useState<number | undefined>(undefined);
  useEffect(() => {
    listAdminSettings().then(({ settings }) => {
      const ms = settings.find((s) => s.key === "agent_narration_warn_after_ms");
      const chars = settings.find((s) => s.key === "agent_narration_warn_after_chars");
      if (ms?.has_value) setNarrationWarnAfterMs(Number(ms.value));
      if (chars?.has_value) setNarrationWarnAfterChars(Number(chars.value));
    }).catch(() => {});
  }, []);
  // Catalogo modelli per risolvere context window del modello attivo
  const [modelCatalog, setModelCatalog] = useState<ModelCatalogEntry[]>([]);
  useEffect(() => {
    getModels().then(({ models }) => setModelCatalog(models)).catch(() => {});
  }, []);
  const {
    messages, isLoading, isSending, isReady, isReconnecting, error, busyByMessage,
    agentRun, agentSteps, agentRuns, agentStepsMap, metaStepsMap,
    tokenUsage, traces, streamingToken, thinkingText,
    attachmentIndexProposal, clearAttachmentIndexProposal, applyAttachmentsIndexed,
    pendingCount,
    confirmingRunId,
    send, resend, remove, feedbackError, feedbackPositive, positiveFeedback,
    confirmAgent, cancelRun,
  } = useChat(projectId, profileId, { sessionId });
  // Run realmente in esecuzione: agentRuns conserva i run terminati per ~30s
  // (grace per i pannelli step), quindi la size grezza non indica parallelismo.
  const activeParallelRuns = [...agentRuns.values()].filter(
    (r) => !isStatusTerminal(r.status),
  ).length;
  const prevAgentActiveRef = useRef(false);
  useEffect(() => {
    const isActive = agentRun?.status === "running";
    if (isActive !== prevAgentActiveRef.current) {
      prevAgentActiveRef.current = isActive;
      onAgentActivityChange?.(isActive);
    }
  }, [agentRun?.status, onAgentActivityChange]);

  // Notifica al parent (ide-shell) il ratio % di riempimento context_window
  // dell'ultimo turno: usato per mostrare il valore sul bottone "Compatta chat".
  // Punto unico computeContextFill, lo stesso della TokenUsageBar (regola L).
  useEffect(() => {
    if (!onCtxRatioChange) return;
    onCtxRatioChange(
      computeContextFill(messages, agentRun ?? null, selectedModel, modelCatalog).ratio,
    );
    // Trigger volutamente su model + usage.lastPromptTokens: sono le SOLE
    // proprieta' di agentRun lette da computeContextFill. L'oggetto intero
    // muta a ogni step del run e rilancerebbe l'effect senza cambiare il ratio.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    messages,
    agentRun?.model,
    agentRun?.usage?.lastPromptTokens,
    selectedModel,
    modelCatalog,
    onCtxRatioChange,
  ]);
  useEffect(() => {
    onTracesChange?.(traces);
  }, [traces, onTracesChange]);
  const prevRunStatusRef = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (!agentRun) return;
    // Punto unico (regola L): delega a isStatusTerminal invece di un array
    // hardcoded, che dimenticava gli esiti canonici (completed_verified/
    // completed_unverified/failed_diagnosed/blocked_needs_input) -> onRunEnd non
    // scattava e la telemetria di fine run si perdeva.
    const isTerminal = isStatusTerminal(agentRun.status);
    if (isTerminal && prevRunStatusRef.current === "running") {
      onRunEnd?.({ provider: agentRun.provider, model: agentRun.model, status: agentRun.status });
    }
    prevRunStatusRef.current = agentRun.status;
  }, [agentRun, onRunEnd]);

  // P6: Toast "Connessione ripristinata"
  const wasReconnectingRef = useRef(false);
  const [reconnectSuccess, setReconnectSuccess] = useState(false);
  useEffect(() => {
    if (wasReconnectingRef.current && !isReconnecting) {
      setReconnectSuccess(true);
      const timer = setTimeout(() => setReconnectSuccess(false), 3000);
      return () => clearTimeout(timer);
    }
    wasReconnectingRef.current = isReconnecting;
  }, [isReconnecting]);

  const [input, setInput] = useState("");
  const [forceProvider, setForceProvider] = useState(false);
  const autoSendPendingRef = useRef<string | null>(null);
  // Salva il provider hint in un ref per evitare che onExternalInputConsumed()
  // lo resetti prima che l'auto-send effect lo possa usare.
  const pendingProviderHintRef = useRef<{ provider?: string; model?: string } | undefined>(undefined);
  // Stesso meccanismo per l'agent type hint (es. "debugger" dai pannelli error-fix):
  // serve sopravvivere fino all'auto-send dopo che onExternalInputConsumed() ha
  // azzerato i pending nel parent.
  const pendingAgentTypeHintRef = useRef<string | undefined>(undefined);
  const automationOnceRef = useRef<"study" | "confirm" | "automatic" | null>(null);
  useEffect(() => {
    if (externalInput) {
      if (externalAutoSend) {
        autoSendPendingRef.current = externalInput;
        pendingProviderHintRef.current = externalProviderHint;
        pendingAgentTypeHintRef.current = externalAgentTypeHint;
      }
      if (externalAutomationOverride) {
        automationOnceRef.current = externalAutomationOverride;
      }
      setInput(externalInput);
      onExternalInputConsumed?.();
    }
  // eslint-disable-next-line -- intentional: only re-run when externalInput changes
  }, [externalInput]);
  const [attachments, setAttachments] = useState<ChatAttachment[]>([]);
  // Snapshot della richiesta in volo: testo + allegati. Se la send fallisce
  // (es. 500 dal backend, network, body limit), useEffect "restore-on-error"
  // riporta lo stato cosi' l'utente non perde quello che aveva scritto.
  // Azzerato al primo response success o quando l'utente ricomincia a scrivere
  // (per non sovrascrivere il nuovo input se l'errore arriva in ritardo).
  const pendingSendSnapshotRef = useRef<{ text: string; attachments: ChatAttachment[] } | null>(null);
  // Invio "in sospeso" quando la richiesta risulta GIA' COMPLETATA: l'agente NON
  // parte finche' l'utente non clicca "Rifai comunque" (onProceed del banner).
  const pendingProceedSendRef = useRef<(() => void) | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [precheckPending] = useState(false);
  const [precheckResult, setPrecheckResult] = useState<PrecheckResult & { originalText: string } | null>(null);
  const [micSupported, setMicSupported] = useState(false);
  const [isListening, setIsListening] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const lastUserRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const speechRef = useRef<SpeechRecognitionLike | null>(null);
  const [showScrollBtn, setShowScrollBtn] = useState(false);
  const [resendPreview, setResendPreview] = useState<{
    messageId: string;
    content: string;
  } | null>(null);
  const [agentStatusExpanded, setAgentStatusExpanded] = useState(false);
  const hasBusyMessageAction = Object.values(busyByMessage).some(
    (action) => action === "resend" || action === "delete" || action === "feedback",
  );
  const isAgentRunning =
    agentRun != null && isAgentRunLiveOrWaiting(agentRun.status);

  // ADR 0037: osserva la larghezza reale della lista messaggi per derivare la
  // soglia densita' del collasso, coerente con le @container query (che agiscono
  // sulle CLASSI CSS; la soglia numerica del folding e' logica, non CSS, quindi
  // la calcoliamo qui). Attivo solo col nastro abilitato.
  useEffect(() => {
    if (!activityStreamEnabled) return;
    const el = scrollRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const apply = (w: number) => {
      const next: FoldThreshold = w <= 380 ? 2 : w >= 600 ? 4 : 3;
      setFoldThreshold((prev) => (prev === next ? prev : next));
    };
    apply(el.clientWidth);
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) apply(e.contentRect.width);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [activityStreamEnabled]);

  const [nowTick, setNowTick] = useState(() => Date.now());
  useEffect(() => {
    if (!isAgentRunning) return;
    const id = setInterval(() => setNowTick(Date.now()), 1000);
    return () => clearInterval(id);
  }, [isAgentRunning]);
  // Tempo trascorso dall'inizio del run: misura quanto tempo l'agente sta
  // lavorando in totale. Prima usavamo "secondi dall'ultimo step" come fonte,
  // ma con agenti che fanno step ogni <1s il counter restava bloccato a 0s.
  // Il countdown ora avanza monotonicamente.
  const runStartedAt = agentRun?.createdAt
    ? new Date(agentRun.createdAt).getTime()
    : Date.now();
  // Nome allineato a cio' che misura: si chiamava `secondsSinceLastStep` anche
  // dopo essere stato spostato sull'avvio run, e la barra attivita' ci scriveva
  // sopra "Forza stop" facendo leggere una lunga elaborazione come un blocco.
  const runElapsedSeconds = Math.max(0, Math.floor((nowTick - runStartedAt) / 1000));
  // "Stuck" calcolato sul tempo dall'ULTIMO step (non dall'inizio run): se
  // l'agente non emette step da >60s e' in attesa di qualcosa (LLM lento).
  // Meta-step live del run corrente: arrivano via SSE in tempo reale, a
  // differenza di `agentSteps` che si popola solo a fine run / via polling.
  // Li usiamo per (a) far avanzare lastStepAt cosi' "Agente in attesa" non
  // scatta mentre l'agente sta lavorando (emette meta_step), e (b) mostrare
  // l'attivita' corrente (titolo dell'ultimo meta_step).
  const liveMetaSteps = (agentRun?.runId ? metaStepsMap.get(agentRun.runId) : undefined) ?? [];
  // Refinement P13: compone il nastro del run LIVE UNA sola volta (stesso
  // foldThreshold del pannello). La STESSA istanza alimenta sia la campanella
  // (deriva notifiche + anchorId) sia il renderer del nastro (AgentStepsPanel):
  // gli anchorId del deep-link coincidono per costruzione, non per determinismo.
  const liveRunId = agentRun?.runId;
  const liveActivityStream = useMemo(() => {
    if (!activityStreamEnabled || !liveRunId) return null;
    const meta = metaStepsMap.get(liveRunId) ?? [];
    return composeActivityStream(
      meta,
      agentStepsMap.get(liveRunId) ?? [],
      tracesForRun(traces, liveRunId, meta),
      foldThreshold,
    );
  }, [activityStreamEnabled, liveRunId, metaStepsMap, agentStepsMap, traces, foldThreshold]);
  const lastMetaStep = liveMetaSteps.length > 0 ? liveMetaSteps[liveMetaSteps.length - 1] : null;
  const lastMetaStepAt = lastMetaStep ? new Date(lastMetaStep.createdAt).getTime() : 0;
  const lastAgentStepAt = agentSteps.length > 0
    ? Math.max(...agentSteps.map((s) => new Date(s.createdAt ?? 0).getTime()))
    : runStartedAt;
  const lastStepAt = Math.max(lastAgentStepAt, lastMetaStepAt);
  const secondsSinceLastStep = Math.max(0, Math.floor((nowTick - lastStepAt) / 1000));
  const isAgentStuck = isAgentRunning && secondsSinceLastStep > 60;

  // Auto-abort rimosso: il client non puo' sapere se "nessun nuovo step da Xs"
  // significa "bloccato" o "legittimamente lento" (provider in fallback dopo
  // cooldown billing, LLM call su contesto grande, tool long-running).
  //
  // Veri guardrail (lato backend) gia' attivi:
  //   - mcp-core: sse_max_silence_secs (settings DB, default 120s) — se SSE
  //     non emette eventi (incluso ping heartbeat ogni 30s) per N secondi
  //     chiude lo stream e segna il run fallito
  //   - mcp-core: max_provider_fallbacks dinamico = N provider idonei
  //   - mcp-core: HOLLOW detection (EMPTY_ANSWER / NO_TOOLS / RESIGNED)
  //
  // Lato client conserviamo solo:
  //   - pulsante Stop rosso sempre visibile (cancellazione esplicita utente)
  //   - banner informativo nella timeline se isAgentStuck (gia' presente:
  //     calcolo `secondsSinceLastStep > 60` poco sopra)

  const timelineSteps = [...agentSteps]
    .sort((a, b) => a.stepIndex - b.stepIndex)
    .slice(-5);
  const runningAgentStep =
    [...agentSteps].reverse().find((step) => step.status === "running") ?? null;
  const runningCommand =
    runningAgentStep?.toolName === "run_in_terminal"
      ? extractRunningCommand(runningAgentStep.toolInput)
      : null;
  const latestStepWithOutput =
    [...agentSteps].reverse().find((step) => typeof step.toolResult === "string" && step.toolResult.trim().length > 0) ??
    null;
  const latestOutputSnippet = latestStepWithOutput?.toolResult
    ?.replace(/\s+/g, " ")
    .trim()
    .slice(0, 180);
  const runningSteps = agentSteps.filter((step) => step.status === "running").length;
  const completedSteps = agentSteps.filter((step) => step.status === "completed").length;
  const failedSteps = agentSteps.filter((step) => step.status === "failed").length;
  // Step di tipo "provider_unavailable": il routing Rust ha segnalato che
  // tutti i provider configurati sono in cooldown. Niente run, mostro banner.
  // Vedi crates/mcp-core/src/chat_messages.rs::spawn_agent_run check
  // `routing_result.no_capable_provider`.
  const providerUnavailableStep = [...agentSteps]
    .reverse()
    .find((step) => step.status === "provider_unavailable") ?? null;
  const providersInCooldown: string[] = (() => {
    if (!providerUnavailableStep) return [];
    const ti = providerUnavailableStep.toolInput;
    if (!ti || typeof ti !== "object") return [];
    const arr = (ti as Record<string, unknown>).providers_in_cooldown;
    if (!Array.isArray(arr)) return [];
    return arr.filter((x): x is string => typeof x === "string");
  })();
  const isChatBusy = isLoading || isAgentRunning || hasBusyMessageAction;
  // Fan-in async: il run PADRE e' SOSPESO in attesa dei sub-agent in background
  // (fonte primaria dello stato: agentRun.status, non il testo). Il contatore,
  // se disponibile, arriva dall'ULTIMO meta-step awaiting_subagents (segnale
  // strutturato, punto unico latestAwaitingSubagentsCount).
  const isAwaitingSubagents = agentRun?.status === "awaiting_subagents";
  const awaitingSubagentsCount = isAwaitingSubagents
    ? latestAwaitingSubagentsCount(liveMetaSteps)
    : undefined;
  // "Invio al server in corso" SOLO finche' la POST non e' confermata
  // (isSending): lo stato di elaborazione non deve essere ottimismo del client
  // ma riflettere la conferma del server (messaggio persistito / run avviato).
  const busyLabel = isAgentRunning
    ? "Agente AI in esecuzione"
    : isAwaitingSubagents
      ? typeof awaitingSubagentsCount === "number" && awaitingSubagentsCount > 0
        ? `In attesa di ${awaitingSubagentsCount} sub-agent in background…`
        : "In attesa dei sub-agent in background…"
      : hasBusyMessageAction
        ? "Operazione sui messaggi in corso"
        : isSending
          ? "Invio al server in corso…"
          : "Elaborazione richiesta in corso";

  /* ---- Scroll management ---- */

  const scrollKey = sessionId ? `nexus:scroll:${sessionId}` : null;
  const scrollRestoredRef = useRef(false);
  const justRestoredRef = useRef(false); // blocca auto-scroll subito dopo restore

  // "Near bottom" threshold: se distanza dal fondo < di questo, consideriamo
  // l'utente attaccato al fondo e l'auto-scroll resta attivo. Sopra: l'utente
  // sta leggendo storia precedente, l'auto-scroll si ferma. Coerente con la
  // soglia del bottone "scroll to bottom" (showScrollBtn) per non avere due
  // stati: o sei a fondo (auto attivo, bottone nascosto) o non lo sei.
  const NEAR_BOTTOM_PX = 80;
  // Stato (in ref per non causare re-render): true se l'ultimo onScroll
  // osservato era near-bottom. Inizializzato a true: all'apertura della chat
  // assumiamo che l'utente voglia vedere l'ultimo messaggio.
  const wasNearBottomRef = useRef(true);

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    wasNearBottomRef.current = distFromBottom < NEAR_BOTTOM_PX;
    setShowScrollBtn(distFromBottom > NEAR_BOTTOM_PX);
    if (scrollRestoredRef.current && scrollKey) {
      try { sessionStorage.setItem(scrollKey, String(el.scrollTop)); } catch {}
    }
  }, [scrollKey]);

  const scrollToBottom = useCallback(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, []);

  // Auto-scroll resilient: un singolo MutationObserver + ResizeObserver sul
  // container intercetta OGNI cambio di contenuto (streaming testo dei chunk
  // del messaggio assistant, fumetti agente, nuovi step, thinking, etc.) senza
  // dover incollare un useEffect per ogni variabile. Pattern standard delle
  // chat: se l'utente era near-bottom prima del resize, lo teniamo a fondo.
  // Se ha risalito (wasNearBottomRef=false), nessuno scroll. Se torna a fondo
  // (handleScroll riporta wasNearBottomRef=true), il prossimo update lo
  // ri-aggancia automaticamente.
  //
  // `behavior: "auto"` (non smooth) durante updates frequenti come lo streaming:
  // animare ogni chunk creerebbe code di scroll che si sovrappongono e la chat
  // saltellerebbe. L'utente percepisce comunque uno scroll fluido perche' gli
  // update arrivano a 30+ Hz.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onContentChange = () => {
      if (justRestoredRef.current) return;
      if (!wasNearBottomRef.current) return;
      // Usa rAF per coalescere bursts di update DOM in un singolo scroll.
      requestAnimationFrame(() => {
        const cur = scrollRef.current;
        if (cur) cur.scrollTop = cur.scrollHeight;
      });
    };
    const ro = new ResizeObserver(onContentChange);
    // Osserva il primo figlio (wrapper interno con il contenuto della lista):
    // quando i suoi children crescono, scattano sia ResizeObserver sia
    // MutationObserver. Il doppio observer e' ridondante per design — se uno
    // non scatta (es. il content cresce senza cambio di layout), l'altro
    // copre il caso.
    if (el.firstElementChild instanceof Element) {
      ro.observe(el.firstElementChild);
    } else {
      ro.observe(el);
    }
    const mo = new MutationObserver(onContentChange);
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
    };
  }, []);

  // Ripristina scroll dopo il caricamento iniziale dei messaggi
  useEffect(() => {
    if (scrollRestoredRef.current || messages.length === 0 || !scrollKey) return;
    const el = scrollRef.current;
    if (!el) return;
    try {
      const saved = sessionStorage.getItem(scrollKey);
      if (saved !== null) {
        const pos = Number(saved);
        // Aspetta che il DOM sia renderizzato completamente
        setTimeout(() => {
          if (scrollRef.current) {
            scrollRef.current.scrollTop = pos;
            justRestoredRef.current = true;
            // Dopo 300ms riabilita auto-scroll
            setTimeout(() => { justRestoredRef.current = false; }, 300);
          }
          scrollRestoredRef.current = true;
        }, 80);
      } else {
        scrollRestoredRef.current = true;
      }
    } catch {
      scrollRestoredRef.current = true;
    }
  }, [messages.length, scrollKey]);

  /* ---- Speech recognition setup ---- */

  useEffect(() => {
    const SpeechRecognitionCtor =
      typeof window === "undefined"
        ? undefined
        : window.SpeechRecognition || window.webkitSpeechRecognition;
    setMicSupported(Boolean(SpeechRecognitionCtor));
    return () => {
      speechRef.current?.stop();
      speechRef.current = null;
    };
  }, []);

  /* ---- Auto-scroll on new user message ---- */

  useEffect(() => {
    if (!scrollRestoredRef.current || justRestoredRef.current || messages.length === 0) return;
    const lastMsg = messages[messages.length - 1];
    if (lastMsg?.role === "user") {
      setTimeout(() => {
        lastUserRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
      }, 30);
    }
  }, [messages]);

  // Scroll to bottom when agent enters awaiting_confirmation so the Approva
  // button is visible: caso speciale che IGNORA wasNearBottomRef (vogliamo
  // SEMPRE che il pulsante di conferma sia visibile).
  useEffect(() => {
    if (agentRun?.status === "awaiting_confirmation") {
      setTimeout(scrollToBottom, 60);
    }
  }, [agentRun?.status, scrollToBottom]);

  // NB: gli auto-scroll su isLoading change e su agentSteps.length change sono
  // stati rimossi: il MutationObserver + ResizeObserver sopra li copre TUTTI
  // (e qualsiasi altro cambio di contenuto: streaming token, thinking, fumetti)
  // senza richiedere un useEffect per ogni variabile.

  // Restore-on-error: se la send fallisce, ripristina il testo+allegati che
  // l'utente aveva digitato cosi' non li perde. La logica funziona perche'
  // useChat::send fa setError(null) all'inizio e setError(formatted) nel catch
  // — quindi una transizione null -> non-null su `error` significa che la
  // richiesta in volo (snapshot in ref) e' fallita.
  //
  // Safeguard: ripristina SOLO se l'utente non ha gia' iniziato a digitare un
  // messaggio nuovo dopo il send (cosa che sovrascriverebbe). Se sta scrivendo,
  // mostra l'errore e mantiene il suo nuovo testo: lo snapshot vecchio si perde.
  useEffect(() => {
    if (!error) return;
    const snap = pendingSendSnapshotRef.current;
    if (!snap) return;
    pendingSendSnapshotRef.current = null;
    // L'utente sta scrivendo qualcos'altro? Allora non sovrascrivere.
    const userIsTyping = input.trim().length > 0 || attachments.length > 0;
    if (userIsTyping) return;
    setInput(snap.text);
    setAttachments(snap.attachments);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [error]);

  // Cleanup snapshot quando la send completa con successo (= isLoading torna
  // a false senza error). Cosi' un errore futuro su una send diversa non
  // ripristina un messaggio vecchio gia' inviato.
  useEffect(() => {
    if (!isLoading && !error) {
      pendingSendSnapshotRef.current = null;
    }
  }, [isLoading, error]);

  /* ---- Handlers ---- */

  const doSend = async (
    text: string,
    providerHintOverride?: { provider?: string; model?: string },
    agentTypeHintOverride?: string,
  ) => {
    // Se il provider e' "auto" e c'e' un hint esterno (es. generazione documenti),
    // usa il hint per forzare un provider/modello capace.
    const hint = providerHintOverride || externalProviderHint;
    // Agent type hint (es. "debugger" dai pannelli error-fix): override esplicito
    // dall'auto-send (via ref) oppure prop esterna. Strutturale, mai dedotto dal testo.
    const agentTypeHint = agentTypeHintOverride ?? externalAgentTypeHint;
    // ADR 0023: provider e modello sono override indipendenti.
    // Provider: forzato solo se selezionato esplicitamente (diverso da "auto");
    // altrimenti lascia decidere al routing (eventuale hint esterno).
    const effectiveProvider = selectedProvider !== "auto"
      ? selectedProvider
      : hint?.provider;
    // Modello: un modello scelto esplicitamente va SEMPRE inviato come override,
    // anche se il provider e' "auto". Un modello identifica univocamente il suo
    // provider (il backend lo ricava dal catalogo), quindi "auto" sul provider
    // non deve azzerare la scelta esplicita del modello.
    const effectiveModel = selectedModel !== "auto"
      ? selectedModel
      : (selectedProvider === "auto" ? hint?.model : undefined);
    const modeForSend = automationOnceRef.current ?? automationMode;
    automationOnceRef.current = null;

    const snapshotAttachments = [...attachments];
    const sendOpts = {
      profileId,
      activeFiles,
      providerOverride: effectiveProvider,
      modelOverride: effectiveModel,
      automationMode: modeForSend,
      supervisorMode: supervisorMode !== "none" ? supervisorMode : undefined,
      attachments: snapshotAttachments,
      agentTypeHint,
    };
    // Snapshot del messaggio in volo: se l'invio fallisce (es. 500 backend,
    // network, body limit), useEffect su `error` riporta input + attachments
    // nello stato cosi' l'utente non perde quello che aveva scritto e puo'
    // ritentare con un solo click.
    const fireSend = () => {
      pendingSendSnapshotRef.current = { text, attachments: snapshotAttachments };
      void send(text, sendOpts);
    };
    const clearComposer = () => {
      setInput("");
      setAttachments([]);
      setAttachmentError(null);
      setPrecheckResult(null);
      if (fileInputRef.current) fileInputRef.current.value = "";
    };

    // Knowledge "richieste simili" RIMOSSO: l'endpoint /api/projects/:id/
    // knowledge/similar e' stato dismesso SENZA sostituto (ADR 0017 v2 F6 ->
    // 410 Gone). La chiamata frontend falliva ad OGNI invio finendo sempre nel
    // catch (feature inerte) ma il dispatcher la registrava come "Operazione
    // progetto (POST) fallita: endpoint deprecated", con un toast fuorviante.
    // Si procede direttamente con l'invio; il dedup/anti-ripetizione semantico,
    // se serve, vive lato brain (RAG inline su /api/internal/knowledge/search).
    fireSend();
    clearComposer();
  };

  // Auto-send: quando l'input esterno richiede invio automatico (es. Auto Fix sequenziale)
  useEffect(() => {
    // Attende che sia isAgentRunning=false CHE isLoading=false per evitare il race
    // condition dove lo status del run è "completed" ma isLoading è ancora true.
    if (autoSendPendingRef.current && input === autoSendPendingRef.current && !isAgentRunning && !isLoading) {
      const text = autoSendPendingRef.current;
      const hint = pendingProviderHintRef.current;
      const agentTypeHint = pendingAgentTypeHintRef.current;
      autoSendPendingRef.current = null;
      pendingProviderHintRef.current = undefined;
      pendingAgentTypeHintRef.current = undefined;
      // Piccolo delay per assicurare che lo stato React sia stabile
      const timer = setTimeout(() => doSend(text, hint, agentTypeHint), 150);
      return () => clearTimeout(timer);
    }
  // isAgentRunning e isLoading inclusi intenzionalmente: se bloccati l'effect riprova
  // automaticamente quando entrambi diventano false.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [input, isAgentRunning, isLoading]);

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if ((!input.trim() && attachments.length === 0) || !hasProject) return;
    const messageText = input.trim() || (attachments.some(a => a.base64Content) ? "[immagine allegata]" : "[allegato]");
    // Precheck disabilitato: l'analisi di "messaggio troppo generico" e' ora
    // gestita dal classifier intent (campo is_ambiguous + candidates) che
    // gia' fa una chiamata LLM al brain. Evitiamo cosi' una seconda call
    // LLM per turno (~1-3s di latenza in piu' per ogni messaggio).
    doSend(messageText);
  };

  const copyMessage = async (_messageId: string, content: string): Promise<boolean> => {
    try {
      await navigator.clipboard.writeText(content);
      return true;
    } catch {
      return false;
    }
  };

  const readFileAsBase64 = (file: File): Promise<string> =>
    new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const result = reader.result as string;
        resolve(result.split(",")[1] ?? "");
      };
      reader.onerror = reject;
      reader.readAsDataURL(file);
    });

  const handlePickFiles = async (files: FileList | null) => {
    if (!files?.length) return;
    const next: ChatAttachment[] = [];
    for (const file of Array.from(files)) {
      if (file.size > MAX_ATTACHMENT_BYTES) {
        setAttachmentError(`Il file ${file.name} supera ${Math.round(MAX_ATTACHMENT_BYTES / 1_000_000)} MB.`);
        continue;
      }
      if (file.type.startsWith("image/")) {
        try {
          const base64Content = await readFileAsBase64(file);
          next.push({
            name: file.name,
            mimeType: file.type,
            sizeBytes: file.size,
            textContent: "",
            base64Content,
          });
        } catch {
          setAttachmentError(`Impossibile leggere l'immagine ${file.name}.`);
        }
        continue;
      }
      // Classifica binario: mime-type "binary" tipico (zip/pdf/exe/audio/video)
      // O file.text() che contiene NULL bytes (U+0000) — incompatibili con
      // jsonb di Postgres che li rifiuta con "unsupported Unicode escape
      // sequence". I file binari vanno inviati come base64; testo come UTF-8.
      const mt = (file.type || "").toLowerCase();
      const binaryByMime =
        mt.startsWith("application/zip") ||
        mt.startsWith("application/x-zip") ||
        mt.startsWith("application/pdf") ||
        mt.startsWith("application/octet-stream") ||
        mt.startsWith("application/x-tar") ||
        mt.startsWith("application/gzip") ||
        mt.startsWith("application/x-7z-compressed") ||
        mt.startsWith("application/x-rar-compressed") ||
        mt.startsWith("application/x-executable") ||
        mt.startsWith("application/x-msdownload") ||
        mt.startsWith("audio/") ||
        mt.startsWith("video/") ||
        mt.startsWith("font/");
      let textContent = "";
      let isBinary = binaryByMime;
      if (!isBinary) {
        try {
          textContent = await file.text();
        } catch {
          setAttachmentError(`Impossibile leggere ${file.name} come allegato testuale.`);
          continue;
        }
        // Detect NULL bytes nei primi 8KB — segnale chiaro che e' binario
        // anche se mime e' generico (es. application/octet-stream o vuoto).
        const sample = textContent.slice(0, 8192);
        if (sample.indexOf("\0") !== -1) {
          isBinary = true;
        }
      }
      if (isBinary) {
        try {
          const base64Content = await readFileAsBase64(file);
          next.push({
            name: file.name,
            mimeType: file.type || "application/octet-stream",
            sizeBytes: file.size,
            textContent: "",
            base64Content,
          });
        } catch {
          setAttachmentError(`Impossibile leggere ${file.name} come allegato binario.`);
        }
        continue;
      }
      try {
        if (!textContent.trim()) {
          setAttachmentError(`Il file ${file.name} e' vuoto o non leggibile come testo.`);
          continue;
        }
        next.push({
          name: file.name,
          mimeType: file.type || "text/plain",
          sizeBytes: file.size,
          textContent,
        });
      } catch {
        setAttachmentError(`Impossibile leggere ${file.name} come allegato testuale.`);
      }
    }
    if (next.length > 0) {
      setAttachments((current) => [...current, ...next]);
      setAttachmentError(null);
    }
  };

  const handlePasteImages = async (files: File[]) => {
    const next: ChatAttachment[] = [];
    for (const file of files) {
      if (file.size > MAX_ATTACHMENT_BYTES) {
        setAttachmentError(`L'immagine supera ${Math.round(MAX_ATTACHMENT_BYTES / 1_000_000)} MB.`);
        continue;
      }
      try {
        const base64Content = await readFileAsBase64(file);
        const name = file.name || `screenshot-${Date.now()}.png`;
        next.push({
          name,
          mimeType: file.type || "image/png",
          sizeBytes: file.size,
          textContent: "",
          base64Content,
        });
      } catch {
        setAttachmentError("Impossibile leggere l'immagine incollata.");
      }
    }
    if (next.length > 0) {
      setAttachments((current) => [...current, ...next]);
      setAttachmentError(null);
    }
  };

  const toggleMicrophone = () => {
    const SpeechRecognitionCtor =
      typeof window === "undefined"
        ? undefined
        : window.SpeechRecognition || window.webkitSpeechRecognition;
    if (!SpeechRecognitionCtor) {
      setMicSupported(false);
      return;
    }

    if (speechRef.current && isListening) {
      speechRef.current.stop();
      return;
    }

    const recognition = new SpeechRecognitionCtor();
    recognition.lang = "it-IT";
    recognition.continuous = true;
    recognition.interimResults = false;
    recognition.onresult = (event) => {
      const transcript = Array.from(event.results)
        .map((result) => result[0]?.transcript ?? "")
        .join(" ")
        .trim();
      if (!transcript) return;
      setInput((current) => (current.trim() ? `${current.trim()} ${transcript}` : transcript));
    };
    recognition.onerror = () => {
      setIsListening(false);
    };
    recognition.onend = () => {
      setIsListening(false);
      speechRef.current = null;
    };
    speechRef.current = recognition;
    setIsListening(true);
    recognition.start();
  };

  const handleResend = useCallback(
    (messageId: string) => {
      const originalMessage = messages.find((message) => message.id === messageId);
      const previewContent = originalMessage?.content?.trim() ?? "";
      setResendPreview({
        messageId,
        content: previewContent || "[richiesta senza contenuto testuale]",
      });
      scrollToBottom();
      setTimeout(scrollToBottom, 40);
      setTimeout(scrollToBottom, 140);
      void (async () => {
        await resend(messageId, { profileId, activeFiles });
        setResendPreview((current) => (current?.messageId === messageId ? null : current));
        setTimeout(scrollToBottom, 40);
      })();
    },
    [messages, resend, profileId, activeFiles, scrollToBottom],
  );

  // Riattivazione di una chat sospesa dal riavvio del backend: invia un messaggio
  // SINTETICO (nascosto nella UI) con resume=true. Il backend continua l'ultimo run
  // `interrupted` dallo stato salvato (messages_json), NON riparte da zero. Nessun
  // auto-riavvio: parte solo dal click sul pulsante "Riattiva" del banner.
  const handleResume = useCallback(() => {
    void (async () => {
      await send("Riprendi l'elaborazione interrotta", {
        profileId,
        activeFiles,
        synthetic: true,
        resume: true,
      });
      setTimeout(scrollToBottom, 40);
    })();
  }, [send, profileId, activeFiles, scrollToBottom]);

  const handleDelete = useCallback(
    (messageId: string) => {
      void (async () => {
        const confirmed = await confirmDialog(
          "Vuoi cancellare questo messaggio?",
          "Conferma cancellazione",
        );
        if (confirmed) {
          await remove(messageId);
        }
      })();
    },
    [confirmDialog, remove],
  );

  const handleFeedback = useCallback(
    (messageId: string, content: string) => {
      setFeedbackDialog({ messageId, content });
    },
    [],
  );

  const handleFeedbackConfirm = useCallback(
    async (description: string) => {
      if (!feedbackDialog) return;
      const { messageId } = feedbackDialog;
      setFeedbackDialog(null);
      await feedbackError(messageId, description);
    },
    [feedbackDialog, feedbackError],
  );

  const handleConfirmAgent = useCallback(
    (runId: string, approved: boolean) => {
      void confirmAgent(runId, approved);
    },
    [confirmAgent],
  );

  const handleRemoveAttachment = useCallback(
    (name: string, sizeBytes: number) => {
      setAttachments((current) =>
        current.filter((item) => !(item.name === name && item.sizeBytes === sizeBytes)),
      );
    },
    [],
  );

  /* ---- Responsive: measure panel width ---- */
  const panelRef = useRef<HTMLDivElement>(null);
  const [panelWidth, setPanelWidth] = useState(400);
  useEffect(() => {
    if (!panelRef.current) return;
    const ro = new ResizeObserver(([entry]) => {
      if (entry) setPanelWidth(entry.contentRect.width);
    });
    ro.observe(panelRef.current);
    return () => ro.disconnect();
  }, []);
  const isCompactPanel = panelWidth < 340;

  // ADR 0037: centro notifiche del run (campanella). Definito QUI, e non dentro
  // il blocco della barra contesto, perche' ha due possibili ospiti e nessuno dei
  // due deve costargli una riga: mentre il run gira viaggia in coda alla barra di
  // stato (che e' gia' a schermo); a run concluso torna accanto alla barra
  // contesto, che a quel punto ha i suoi dati e occupa comunque la sua riga.
  const runNotifications =
    activityStreamEnabled && agentRun?.runId && liveActivityStream ? (
      <RunNotifications
        stream={liveActivityStream}
        runStatus={agentRun.status}
        runId={agentRun.runId}
        pendingActions={agentRun.pendingActions}
        onConfirm={handleConfirmAgent}
        isConfirming={confirmingRunId === agentRun.runId}
        tc={tc}
      />
    ) : null;

  /* ---- Render ---- */

  return (
    <>
    <div
      ref={panelRef}
      style={{
        flex: 1,
        minHeight: 0,
        minWidth: 0,
        width: "100%",
        maxWidth: "100%",
        display: "flex",
        flexDirection: "column",
        alignItems: "stretch",
        height: "100%",
        overflow: "hidden",
      }}
    >
      {showMemory && projectId && projectId !== "default" && (
        <MemoryPanel projectId={projectId} onClose={() => setShowMemory(false)} />
      )}

      {/* Banner "Nessun provider AI disponibile": appare quando il routing
          Rust ha rilevato che tutti i provider configurati sono in cooldown
          (quota/credito esaurito). Lo step e' emesso da
          `chat_messages.rs::spawn_agent_run` con status `provider_unavailable`.
          La UI deve fermarsi e dare istruzioni — NON deve far ripartire il
          run da sola. L'utente clicca "Configurazione provider" per andare
          all'admin oppure aspetta il reset cooldown. */}
      {providerUnavailableStep && (
        <ProviderUnavailableBanner
          step={providerUnavailableStep}
          providersInCooldown={providersInCooldown}
          tc={tc}
        />
      )}

      <div
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "stretch",
          flex: 1,
          minHeight: 0,
          minWidth: 0,
          width: "100%",
        }}
      >
        {activeFiles.length > 0 && (
          <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 8 }}>
            {activeFiles.length} file attivi
          </div>
        )}

        {/* Parallel agents indicator: conta SOLO i run non terminali. agentRuns
            tiene i run conclusi per ~30s (grace per i pannelli step), quindi
            size includerebbe anche i terminati e sui run accodati apparirebbe
            un transitorio "2 agenti in esecuzione parallela" fasullo. */}
        {activeParallelRuns > 1 && (
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 6,
                padding: "4px 10px",
                borderRadius: 8,
                background: `${tc.accent}18`,
                border: `1px solid ${tc.accent}40`,
                fontSize: 11,
                color: tc.accent,
                fontWeight: 600,
                flex: 1,
              }}
            >
              <span>⚡</span>
              <span>{activeParallelRuns} agenti in esecuzione parallela</span>
              <span
                style={{
                  width: 7,
                  height: 7,
                  borderRadius: "50%",
                  background: "#22c55e",
                  marginLeft: 2,
                  boxShadow: "0 0 0 2px #22c55e40",
                  animation: "pulse 1.2s ease-in-out infinite",
                }}
              />
            </div>
          </div>
        )}

        {/* Badge stato DB progetto */}
        {dbStatus?.configured && (
          <a
            href={`/admin/project-database?projectId=${projectId}`}
            style={{
              display: "flex", alignItems: "center", gap: 6,
              padding: "3px 10px", borderRadius: 8, marginBottom: 4,
              background: dbStatus.pending_count && dbStatus.pending_count > 0
                ? "#f59e0b18" : `${tc.accent}10`,
              border: `1px solid ${dbStatus.pending_count && dbStatus.pending_count > 0 ? "#f59e0b40" : tc.accent + "30"}`,
              fontSize: 11, color: dbStatus.pending_count && dbStatus.pending_count > 0 ? "#f59e0b" : tc.textMuted,
              textDecoration: "none", cursor: "pointer", flexShrink: 0,
            }}
          >
            <span>DB</span>
            <span style={{ fontWeight: 600 }}>{dbStatus.engine ?? "postgres"}</span>
            {dbStatus.pending_count && dbStatus.pending_count > 0 ? (
              <span>— {dbStatus.pending_count} migration pending</span>
            ) : (
              <span>— aggiornato</span>
            )}
          </a>
        )}

      {/* Messages area */}
      <div
        className="relative flex-1 flex-col"
        style={{
          minHeight: 0,
          width: "100%",
          overflow: "hidden",
        }}
      >
        <div
          ref={scrollRef}
          onScroll={handleScroll}
          className={`flex-1 flex-col-gap-6 text-base overflow-auto no-scrollbar${activityStreamEnabled ? " nx-chat-container" : ""}`}
          style={{
            minHeight: 0,
            scrollbarWidth: "thin",
            padding: "4px 2px",
          }}
        >
          {!hasProject && (
            <div
              className="text-xs"
              style={{
                color: tc.warning,
                padding: "6px 10px",
                borderRadius: 8,
                border: `1px solid ${tc.warning}`,
                background: `${tc.warning}18`,
              }}
            >
              Seleziona o registra un progetto per usare la chat AI.
            </div>
          )}

          {hasProject && !isReady && (
            <div className="text-xs" style={{ color: tc.textMuted }}>Caricamento sessione chat...</div>
          )}

          {messages.length === 0 && hasProject && isReady && (
            <div style={{ color: tc.textMuted }}>{t("chat.empty")}</div>
          )}
          <ConnectionStatusBanner
            isReconnecting={isReconnecting}
            reconnectSuccess={reconnectSuccess}
            tc={tc}
          />

          {messages.length > 0 && sessionId ? (
            <SessionWorklogPanel sessionId={sessionId} />
          ) : null}

          <MessageList
            messages={messages}
            busyByMessage={busyByMessage}
            tc={tc}
            t={t as (key: string) => string}
            onCopy={copyMessage}
            onResend={handleResend}
            onResume={handleResume}
            onDelete={handleDelete}
            onFeedback={handleFeedback}
            onFeedbackPositive={feedbackPositive}
            positiveFeedback={positiveFeedback}
            lastUserRef={lastUserRef}
            projectId={projectId}
            metaStepsMap={metaStepsMap}
            agentStepsMap={agentStepsMap}
            traces={traces}
            activityStreamEnabled={activityStreamEnabled}
            foldThreshold={foldThreshold}
            nextActions={(!agentRun && metaStepsMap.size > 0) ? (() => {
              // Scelte di proseguimento (next_actions) dell'ultimo run concluso:
              // passate a MessageList per essere rese DENTRO la bolla del
              // messaggio assistant, a fine proposta (vicino al testo). Durante il
              // run attivo restano gestite da AgentStepsPanel (qui undefined).
              const lastAssistantWithRun = [...messages]
                .reverse()
                .find((m) => m.role === "assistant" && m.runId);
              const targetRunId = lastAssistantWithRun?.runId
                ?? Array.from(metaStepsMap.keys()).pop();
              const targetMetaSteps = targetRunId ? metaStepsMap.get(targetRunId) : undefined;
              const choices = targetMetaSteps ? extractLatestNextActions(targetMetaSteps) : [];
              return choices.length ? { runId: targetRunId, choices } : undefined;
            })() : undefined}
          />

          {resendPreview && (
            <div
              style={{
                padding: "8px 10px",
                borderRadius: 10,
                border: `1px solid ${tc.accent + "55"}`,
                background: tc.accentBg,
                alignSelf: "flex-end",
                maxWidth: "96%",
                // Il 96% non comprende padding e bordo: senza questo il preview
                // sfonda la lista come faceva il bubble dei messaggi.
                boxSizing: "border-box",
                minWidth: "30%",
              }}
            >
              <div
                style={{
                  color: tc.accent,
                  fontWeight: 700,
                  fontSize: 11,
                  marginBottom: 4,
                  whiteSpace: "nowrap",
                }}
              >
                Tu (reinvio in corso)
              </div>
              <p style={{ margin: 0, whiteSpace: "pre-wrap", color: tc.text }}>
                {resendPreview.content}
              </p>
            </div>
          )}

          {/* P1: Bolla "preparando esecuzione" — visibile tra invio e primo step */}
          {isLoading && isAgentRunning && agentSteps.length === 0 && !streamingToken && (
            <AgentPreparingBubble tc={tc} />
          )}

          {/* P3: Indicatore progresso inline — visibile durante esecuzione con step */}
          {isAgentRunning && agentSteps.length > 0 && (
            <AgentProgressInline tc={tc} steps={agentSteps} />
          )}

          {/* Blocco thinking Nexus: ragionamento interno (router, executor,
              tool decisions, reasoning provider). Visibile sia durante
              run agent sia durante chat semplice; mostrato in append,
              scroll automatico. Si svuota su nuovo invio o fine run. */}
          {thinkingText && (
            <ThinkingBlock text={thinkingText} tc={tc} />
          )}

          {agentRun && isAgentRunLiveOrWaiting(agentRun.status) && (
            <AgentStepsPanel
              agentRun={agentRun}
              agentSteps={agentSteps}
              agentRuns={agentRuns}
              agentStepsMap={agentStepsMap}
              metaSteps={metaStepsMap.get(agentRun.runId) ?? []}
              tc={tc}
              t={t as (key: string) => string}
              onConfirm={handleConfirmAgent}
              isConfirming={confirmingRunId === agentRun.runId}
              streamingToken={agentRun.status === "running" ? streamingToken : undefined}
              narrationWarnAfterMs={narrationWarnAfterMs}
              narrationWarnAfterChars={narrationWarnAfterChars}
              traces={traces}
              activityStreamEnabled={activityStreamEnabled}
              foldThreshold={foldThreshold}
              mainRunStream={liveActivityStream ?? undefined}
            />
          )}

          {/* FIX D6: il blocco unico "Decisioni del turno" (per il solo ultimo
              run) e' stato rimosso. Le decisioni meta_step sono ora rese come
              card per-messaggio dentro MessageList (MessageMetaSteps), sotto
              OGNI messaggio assistant con runId: cosi' restano leggibili sui
              turni passati e convergono live/refresh (regola L). */}

          {/* Trace AI: col flag activity_stream ON il costo-per-provider e' gia'
              nel footer del nastro, quindi nascondo questo pannello per non
              duplicare. Con OFF resta il rendering odierno. */}
          {!activityStreamEnabled && traces.length > 0 && <InlineTracePanel traces={traces} />}

          {isLoading && !agentRun && streamingToken && (
            <div
              style={{
                padding: "8px 10px",
                borderRadius: 10,
                background: tc.bgCard,
                border: `1px solid ${tc.border}`,
                alignSelf: "flex-start",
                color: tc.text,
                fontSize: 13,
                whiteSpace: "pre-wrap",
                maxWidth: "100%",
                wordBreak: "break-word",
              }}
            >
              {streamingToken}
              <span
                style={{
                  display: "inline-block",
                  width: 2,
                  height: "1em",
                  background: tc.text,
                  verticalAlign: "text-bottom",
                  marginLeft: 1,
                  animation: "nexus-blink 1s step-end infinite",
                }}
              />
            </div>
          )}

          {isLoading && !agentRun && !streamingToken && (
            <div
              style={{
                padding: "8px 10px",
                borderRadius: 10,
                background: tc.bgCard,
                border: `1px solid ${tc.border}`,
                alignSelf: "flex-start",
                color: tc.textMuted,
                fontStyle: "italic",
                fontSize: 12,
              }}
            >
              {t("chat.thinking")}
            </div>
          )}

          {!isLoading && error && (
            <div
              style={{
                padding: "6px 10px",
                borderRadius: 8,
                background: `${tc.error}18`,
                border: `1px solid ${tc.error}`,
                color: tc.error,
                fontSize: 12,
              }}
            >
              {error}
            </div>
          )}
        </div>

        {/* Scroll-to-bottom button */}
        {messages.length > 0 && showScrollBtn && (
          <IconButton
            onClick={scrollToBottom}
            label="Scorri all'ultimo messaggio"
            size={28}
            style={{
              position: "absolute",
              bottom: 8,
              left: "50%",
              transform: "translateX(-50%)",
              fontSize: 14,
              boxShadow: "0 2px 8px rgba(0,0,0,0.2)",
              zIndex: 5,
              borderRadius: 999,
            }}
          >
            <span aria-hidden="true">&#8595;</span>
          </IconButton>
        )}
      </div>

      {isChatBusy && (
        <AgentActivityBar
          tc={tc}
          trailing={runNotifications}
          isAgentStuck={isAgentStuck}
          runElapsedSeconds={runElapsedSeconds}
          secondsSinceLastStep={secondsSinceLastStep}
          busyLabel={busyLabel}
          isAgentRunning={isAgentRunning}
          runningAgentStep={runningAgentStep}
          lastMetaStep={lastMetaStep}
          agentRun={agentRun ?? null}
          onCancelRun={(runId) => void cancelRun(runId)}
          agentStatusExpanded={agentStatusExpanded}
          onToggleExpanded={() => setAgentStatusExpanded((v) => !v)}
          completedSteps={completedSteps}
          runningSteps={runningSteps}
          failedSteps={failedSteps}
          runningCommand={runningCommand}
          latestOutputSnippet={latestOutputSnippet}
          latestStepWithOutputResult={latestStepWithOutput?.toolResult}
          timelineSteps={timelineSteps}
        />
      )}

      {/* Token usage bar */}
      {(() => {
        // Riempimento contesto dal punto unico computeContextFill (regola L):
        // numeratore = prompt dell'ULTIMA chiamata LLM (lastPromptTokens), mai
        // il promptTokens cumulativo di billing del run (causa del bug
        // "5046% ctx" post-compact).
        const fill = computeContextFill(
          messages,
          agentRun ?? null,
          selectedModel,
          modelCatalog,
        );
        const usageBar = (
          <TokenUsageBar
            totalTokens={tokenUsage.totalTokens}
            totalCostUsd={tokenUsage.totalCostUsd}
            contextWindow={fill.ctxWindow}
            lastInputTokens={fill.lastInputTokens}
            modelLabel={fill.activeModel}
          />
        );
        // Mentre il run gira la campanella viaggia gia' in coda alla barra di
        // stato (prop `trailing`): qui NON va aggiunta, altrimenti nasce una riga
        // che esiste solo per lei -- a inizio run la barra contesto e' vuota,
        // quindi si vedeva una fascia alta 28px col solo badge (difetto
        // segnalato). A run concluso invece la barra contesto ha i suoi dati e
        // occupa comunque la riga: li' affiancarla non costa nulla.
        return runNotifications && !isChatBusy ? (
          <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
            <div style={{ flex: 1, minWidth: 0 }}>{usageBar}</div>
            {runNotifications}
          </div>
        ) : (
          usageBar
        );
      })()}

      {/* Precheck widget */}
      <PrecheckSuggestion
        precheckPending={precheckPending}
        precheckResult={precheckResult}
        onClose={() => setPrecheckResult(null)}
        onSend={(text) => doSend(text)}
        tc={tc}
      />

      {similarHits.length > 0 && (
        <SimilarRequestBanner
          hits={similarHits}
          onProceed={() => {
            // "Rifai comunque": esegue l'invio rimasto in sospeso (se la richiesta
            // era gia' completata). Per gli hit solo "simili" non c'e' invio in
            // sospeso e il banner era puramente informativo.
            const proceed = pendingProceedSendRef.current;
            pendingProceedSendRef.current = null;
            setSimilarHits([]);
            proceed?.();
          }}
          onOpenNote={(noteId) => {
            // Apre la nota e annulla l'invio in sospeso (l'utente sta consultando).
            pendingProceedSendRef.current = null;
            setSimilarHits([]);
            window.dispatchEvent(new CustomEvent("nexus:note:open", { detail: { noteId } }));
          }}
          onDismiss={() => {
            // Chiude senza eseguire: scarta l'eventuale invio in sospeso.
            pendingProceedSendRef.current = null;
            setSimilarHits([]);
          }}
        />
      )}

      {/* Composer */}
      <Composer
        input={input}
        onInputChange={setInput}
        attachments={attachments}
        onRemoveAttachment={handleRemoveAttachment}
        attachmentError={attachmentError}
        selectedProvider={selectedProvider}
        onProviderChange={setSelectedProvider}
        forceProvider={forceProvider}
        onForceProviderChange={setForceProvider}
        selectedModel={selectedModel}
        onModelChange={setSelectedModel}
        providerModels={providerModels}
        availableProviders={availableProviders}
        runProvider={agentRun?.provider ?? null}
        runModel={agentRun?.model ?? null}
        runAutomationMode={agentRun?.automationMode ?? null}
        automationMode={automationMode}
        onAutomationModeChange={setAutomationMode}
        supervisorMode={supervisorMode}
        onSupervisorModeChange={setSupervisorMode ?? (() => {})}
        showMemory={showMemory}
        onOpenMemory={() => setShowMemory(true)}
        activeMemoryCount={activeMemoryCount}
        micSupported={micSupported}
        isListening={isListening}
        onToggleMicrophone={toggleMicrophone}
        isLoading={isLoading || precheckPending}
        isAgentRunning={isAgentRunning}
        pendingCount={pendingCount}
        onStopAgent={() => void cancelRun(agentRun?.runId)}
        hasRunningServices={hasRunningServices}
        hasProject={hasProject}
        fileInputRef={fileInputRef}
        onPickFiles={(files) => void handlePickFiles(files)}
        onPasteImages={(files) => void handlePasteImages(files)}
        onSubmit={handleSubmit}
        tc={tc}
        t={t as (key: string) => string}
        compact={isCompactPanel}
      />
      </div>
    </div>

    {/* Dialog segnala errore con textarea ridimensionabile e AI assist */}
    {feedbackDialog && (
      <FeedbackErrorDialog
        messageContent={feedbackDialog.content}
        onConfirm={handleFeedbackConfirm}
        onCancel={() => setFeedbackDialog(null)}
      />
    )}
    {/* Dialog indicizzazione KB: appare dopo l'invio di un messaggio con
        allegati salvati, chiede all'utente quali file vuole aggiungere alla
        Knowledge Base del progetto. Pre-spunta i 'text', lascia non spuntati
        immagini e binari. */}
    {attachmentIndexProposal && (
      <AttachmentIndexDialog
        proposal={attachmentIndexProposal}
        onClose={clearAttachmentIndexProposal}
        onConfirm={async (ids) => {
          try {
            const result = await indexAttachmentsToKb(
              attachmentIndexProposal.messageId,
              ids,
            );
            applyAttachmentsIndexed(
              attachmentIndexProposal.messageId,
              result.indexed,
            );
            clearAttachmentIndexProposal();
          } catch (e) {
            const msg = e instanceof Error ? e.message : "Errore sconosciuto";
            setAttachmentError(`Indicizzazione KB fallita: ${msg}`);
          }
        }}
        tc={tc}
      />
    )}
    </>
  );
}
