"use client";

import {
  useEffect,
  useRef,
  useState,
  useCallback,
  type FormEvent,
} from "react";
import { useChat } from "../lib/use-chat";
import { listProjectMemories, getProjectDbConfig, listAdminSettings, type AITraceEvent, type ChatAttachment, type PrecheckResult, type ProjectDbConfig } from "../lib/api-client";
import { useThemeColors } from "../lib/theme";
import { useI18n } from "../lib/i18n";
import { useGlobalDialog } from "./global-dialog-provider";
import { FeedbackErrorDialog } from "./feedback-error-dialog";
import { IconButton } from "./icon-button";
import { MessageList } from "./chat/message-list";
import { AgentStepsPanel } from "./chat/agent-steps-panel";
import { InlineTracePanel } from "./chat/inline-trace-panel";
import { Composer } from "./chat/composer";
import { MemoryPanel } from "./chat/memory-panel";
import { TokenUsageBar } from "./chat/token-usage-bar";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const MAX_ATTACHMENT_BYTES = 2_000_000;

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
/* AgentPreparingBubble  (P1)                                          */
/* ------------------------------------------------------------------ */

function AgentPreparingBubble({ tc }: { tc: Record<string, string> }) {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => clearInterval(id);
  }, []);
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "10px 14px",
        borderRadius: 10,
        background: tc.bgCard,
        border: `1px solid ${tc.border}`,
        alignSelf: "flex-start",
        maxWidth: "80%",
      }}
    >
      <span
        style={{
          width: 10,
          height: 10,
          borderRadius: "50%",
          background: "#22c55e",
          animation: "pulse 1.4s ease-in-out infinite",
          flexShrink: 0,
        }}
      />
      <span style={{ color: tc.textMuted, fontSize: 13, fontStyle: "italic" }}>
        Nexus sta preparando l&apos;esecuzione&hellip;
      </span>
      <span style={{ color: tc.textMuted, fontSize: 11, opacity: 0.7 }}>
        {seconds}s
      </span>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* AgentProgressInline  (P3)                                           */
/* ------------------------------------------------------------------ */

function AgentProgressInline({
  tc,
  steps,
}: {
  tc: Record<string, string>;
  steps: import("../lib/api-client").AgentStep[];
}) {
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setElapsed((s) => s + 1), 1000);
    return () => clearInterval(id);
  }, []);

  // Reset timer quando arriva un nuovo step
  const stepCount = steps.length;
  useEffect(() => {
    setElapsed(0);
  }, [stepCount]);

  const currentStep = steps[steps.length - 1];
  const recentDone = steps.filter((s) => s.status === "completed" || s.status === "failed").slice(-3);

  const toolLabel = (name: string) => {
    const labels: Record<string, string> = {
      write_file: "Scrittura file",
      edit_file: "Modifica file",
      create_file: "Creazione file",
      patch_file: "Patch file",
      read_file: "Lettura file",
      run_in_terminal: "Comando terminale",
      run_command: "Comando terminale",
      search_in_files: "Ricerca nel codice",
      search_files: "Ricerca file",
      supervisor_check: "Verifica supervisore",
    };
    return labels[name] || name.replace(/_/g, " ");
  };

  const statusIcon = (status: string) => {
    if (status === "completed") return "✓";
    if (status === "failed") return "✗";
    return "•";
  };

  const statusColor = (status: string) => {
    if (status === "completed") return "#22c55e";
    if (status === "failed") return tc.error || "#ef4444";
    return tc.textMuted;
  };

  // Badge avviso per step lenti
  let slowBadge: React.ReactNode = null;
  if (currentStep?.status === "running" && elapsed > 120) {
    slowBadge = (
      <span style={{ fontSize: 10, padding: "1px 6px", borderRadius: 4, background: "#ef444430", color: "#ef4444", fontWeight: 600 }}>
        &gt;2min
      </span>
    );
  } else if (currentStep?.status === "running" && elapsed > 30) {
    slowBadge = (
      <span style={{ fontSize: 10, padding: "1px 6px", borderRadius: 4, background: "#f9731630", color: "#f97316", fontWeight: 600 }}>
        &gt;30s
      </span>
    );
  }

  return (
    <div
      style={{
        padding: "10px 14px",
        borderRadius: 8,
        background: tc.bgCard,
        border: `1px solid ${tc.border}`,
        alignSelf: "stretch",
        fontSize: 12,
      }}
    >
      {/* Intestazione */}
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: recentDone.length > 0 ? 6 : 0 }}>
        <span
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: "#22c55e",
            animation: "pulse 1.4s ease-in-out infinite",
            flexShrink: 0,
          }}
        />
        <span style={{ fontWeight: 600, color: tc.text }}>
          Nexus sta lavorando&hellip;
        </span>
        <span style={{ color: tc.textMuted }}>
          {toolLabel(currentStep?.toolName || "...")}
        </span>
        <span style={{ color: tc.textMuted, opacity: 0.7, fontSize: 11 }}>
          {elapsed}s
        </span>
        {slowBadge}
      </div>

      {/* Step recenti completati */}
      {recentDone.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 2, marginLeft: 16 }}>
          {recentDone.map((s) => (
            <div key={s.stepIndex} style={{ display: "flex", alignItems: "center", gap: 6, color: tc.textMuted }}>
              <span style={{ color: statusColor(s.status), fontSize: 11, fontWeight: 700 }}>
                {statusIcon(s.status)}
              </span>
              <span>{s.stepIndex + 1}. {toolLabel(s.toolName)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
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
  selectedProvider,
  setSelectedProvider,
  selectedModel,
  setSelectedModel,
  providerModels,
  automationMode,
  setAutomationMode,
  supervisorMode = "none",
  setSupervisorMode,
  showMemory,
  setShowMemory,
  externalInput,
  externalAutoSend,
  externalProviderHint,
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
  selectedProvider: string;
  setSelectedProvider: (v: string) => void;
  selectedModel: string;
  setSelectedModel: (v: string) => void;
  providerModels: string[];
  automationMode: "study" | "confirm" | "automatic";
  setAutomationMode: (v: "study" | "confirm" | "automatic") => void;
  supervisorMode?: "none" | "anomaly" | "interleaved" | "continuous";
  setSupervisorMode?: (v: "none" | "anomaly" | "interleaved" | "continuous") => void;
  showMemory: boolean;
  setShowMemory: (v: boolean) => void;
  externalInput?: string;
  externalAutoSend?: boolean;
  externalProviderHint?: { provider?: string; model?: string };
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
  const [feedbackDialog, setFeedbackDialog] = useState<{ messageId: string; content: string } | null>(null);
  const hasProject = UUID_RE.test(projectId);
  const [activeMemoryCount, setActiveMemoryCount] = useState(0);
  const [dbStatus, setDbStatus] = useState<ProjectDbConfig | null>(null);
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
  const { messages, isLoading, isReady, isReconnecting, error, busyByMessage, agentRun, agentSteps, agentRuns, agentStepsMap, tokenUsage, traces, streamingToken, send, resend, remove, feedbackError, confirmAgent, cancelRun } =
    useChat(projectId, profileId, { sessionId });
  const prevAgentActiveRef = useRef(false);
  useEffect(() => {
    const isActive = agentRun?.status === "running";
    if (isActive !== prevAgentActiveRef.current) {
      prevAgentActiveRef.current = isActive;
      onAgentActivityChange?.(isActive);
    }
  }, [agentRun?.status, onAgentActivityChange]);
  useEffect(() => {
    onTracesChange?.(traces);
  }, [traces, onTracesChange]);
  const prevRunStatusRef = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (!agentRun) return;
    const isTerminal = ["completed", "failed", "timed_out", "cancelled", "interrupted"].includes(agentRun.status);
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
  const automationOnceRef = useRef<"study" | "confirm" | "automatic" | null>(null);
  useEffect(() => {
    if (externalInput) {
      if (externalAutoSend) {
        autoSendPendingRef.current = externalInput;
        pendingProviderHintRef.current = externalProviderHint;
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
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [precheckPending, setPrecheckPending] = useState(false);
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
  const isAgentRunning = agentRun?.status === "running";

  const [nowTick, setNowTick] = useState(() => Date.now());
  useEffect(() => {
    if (!isAgentRunning) return;
    const id = setInterval(() => setNowTick(Date.now()), 1000);
    return () => clearInterval(id);
  }, [isAgentRunning]);
  // M66: se ancora nessun step è arrivato via SSE, usa il timestamp di avvio
  // del run come riferimento. Altrimenti il contatore "Agente AI in esecuzione 0s"
  // resta congelato a 0 anche se l'agente sta "pensando" da minuti, perche'
  // lastStepAt = Date.now() lo fa coincidere con nowTick.
  const runStartedAt = agentRun?.createdAt ? new Date(agentRun.createdAt).getTime() : Date.now();
  const lastStepAt = agentSteps.length > 0
    ? Math.max(...agentSteps.map((s) => new Date(s.createdAt ?? 0).getTime()))
    : runStartedAt;
  const secondsSinceLastStep = Math.max(0, Math.floor((nowTick - lastStepAt) / 1000));
  const isAgentStuck = isAgentRunning && secondsSinceLastStep > 60;

  // Auto-abort: se nessun nuovo step parte entro 3 minuti, ferma automaticamente
  const autoAbortRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (!isAgentRunning) {
      if (autoAbortRef.current) clearTimeout(autoAbortRef.current);
      return;
    }
    if (autoAbortRef.current) clearTimeout(autoAbortRef.current);
    autoAbortRef.current = setTimeout(() => {
      if (agentRun?.runId) {
        console.warn("[auto-abort] Nessun progresso da 3 minuti — interrompo agente", agentRun.runId);
        void cancelRun(agentRun.runId);
      }
    }, 3 * 60 * 1000);
    return () => {
      if (autoAbortRef.current) clearTimeout(autoAbortRef.current);
    };
  // Reset timer ogni volta che arriva un nuovo step
  }, [isAgentRunning, agentSteps.length, agentRun?.runId, cancelRun]);

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
  const busyLabel = isAgentRunning
    ? "Agente AI in esecuzione"
    : hasBusyMessageAction
      ? "Operazione sui messaggi in corso"
      : "Elaborazione richiesta in corso";

  /* ---- Scroll management ---- */

  const scrollKey = sessionId ? `nexus:scroll:${sessionId}` : null;
  const scrollRestoredRef = useRef(false);
  const justRestoredRef = useRef(false); // blocca auto-scroll subito dopo restore

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    setShowScrollBtn(el.scrollHeight - el.scrollTop - el.clientHeight > 80);
    if (scrollRestoredRef.current && scrollKey) {
      try { sessionStorage.setItem(scrollKey, String(el.scrollTop)); } catch {}
    }
  }, [scrollKey]);

  const scrollToBottom = useCallback(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
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

  useEffect(() => {
    if (!scrollRestoredRef.current || justRestoredRef.current || isLoading) return;
    const el = scrollRef.current;
    if (!el) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 200) {
      setTimeout(scrollToBottom, 30);
    }
  }, [isLoading, scrollToBottom]);

  // Scroll to bottom when agent enters awaiting_confirmation so the Approva button is visible
  useEffect(() => {
    if (agentRun?.status === "awaiting_confirmation") {
      setTimeout(scrollToBottom, 60);
    }
  }, [agentRun?.status, scrollToBottom]);

  // Auto-scroll when new agent steps arrive (P4)
  useEffect(() => {
    if (!isAgentRunning || agentSteps.length === 0) return;
    if (!scrollRestoredRef.current || justRestoredRef.current) return;
    const el = scrollRef.current;
    if (!el) return;
    const isNearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 200;
    if (isNearBottom) setTimeout(scrollToBottom, 40);
  }, [agentSteps.length, isAgentRunning, scrollToBottom]);

  /* ---- Handlers ---- */

  const doSend = (text: string, providerHintOverride?: { provider?: string; model?: string }) => {
    // Se il provider e' "auto" e c'e' un hint esterno (es. generazione documenti),
    // usa il hint per forzare un provider/modello capace.
    const hint = providerHintOverride || externalProviderHint;
    const shouldForce = forceProvider && selectedProvider !== "auto";
    const effectiveProvider = shouldForce
      ? selectedProvider
      : selectedProvider === "auto"
        ? hint?.provider
        : undefined;
    const effectiveModel = shouldForce
      ? (selectedModel !== "auto" ? selectedModel : undefined)
      : selectedProvider === "auto"
        ? hint?.model
        : undefined;
    const modeForSend = automationOnceRef.current ?? automationMode;
    automationOnceRef.current = null;
    void send(text, {
      profileId,
      activeFiles,
      providerOverride: effectiveProvider,
      modelOverride: effectiveModel,
      automationMode: modeForSend,
      supervisorMode: supervisorMode !== "none" ? supervisorMode : undefined,
      attachments,
    });
    setInput("");
    setAttachments([]);
    setAttachmentError(null);
    setPrecheckResult(null);
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  // Auto-send: quando l'input esterno richiede invio automatico (es. Auto Fix sequenziale)
  useEffect(() => {
    // Attende che sia isAgentRunning=false CHE isLoading=false per evitare il race
    // condition dove lo status del run è "completed" ma isLoading è ancora true.
    if (autoSendPendingRef.current && input === autoSendPendingRef.current && !isAgentRunning && !isLoading) {
      const text = autoSendPendingRef.current;
      const hint = pendingProviderHintRef.current;
      autoSendPendingRef.current = null;
      pendingProviderHintRef.current = undefined;
      // Piccolo delay per assicurare che lo stato React sia stabile
      const timer = setTimeout(() => doSend(text, hint), 150);
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
        setAttachmentError(`Il file ${file.name} supera ${Math.round(MAX_ATTACHMENT_BYTES / 1024)} KB.`);
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
      try {
        const textContent = await file.text();
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
        setAttachmentError(`L'immagine supera ${Math.round(MAX_ATTACHMENT_BYTES / 1024)} KB.`);
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
        <div style={{
          background: "rgba(239,68,68,0.10)",
          border: "1px solid rgba(239,68,68,0.50)",
          borderLeft: "4px solid #ef4444",
          borderRadius: 6,
          padding: "10px 14px",
          margin: "0 0 10px 0",
          fontSize: 12,
          color: tc.text,
          display: "flex",
          flexDirection: "column",
          gap: 6,
        }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span style={{ fontSize: 16 }}>⚠</span>
            <span style={{ fontWeight: 700, color: "#ef4444" }}>
              Nessun provider AI disponibile
            </span>
          </div>
          <div style={{ lineHeight: 1.5 }}>
            {providerUnavailableStep.toolResult ?? "Tutti i provider configurati sono in cooldown."}
          </div>
          {providersInCooldown.length > 0 && (
            <div style={{ fontSize: 10, color: tc.textMuted }}>
              In cooldown: {providersInCooldown.join(", ")}
            </div>
          )}
          <div style={{ display: "flex", gap: 6, marginTop: 4 }}>
            <button
              type="button"
              onClick={() => {
                if (typeof window !== "undefined") {
                  window.open("/admin/settings/providers", "_blank", "noopener");
                }
              }}
              style={{
                background: "rgba(239,68,68,0.18)",
                border: "1px solid rgba(239,68,68,0.55)",
                borderRadius: 4,
                color: "#ef4444",
                cursor: "pointer",
                padding: "3px 10px",
                fontSize: 11,
                fontWeight: 600,
              }}
            >
              Configurazione provider
            </button>
          </div>
        </div>
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

        {/* Parallel agents indicator */}
        {agentRuns.size > 1 && (
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
              <span>{agentRuns.size} agenti in esecuzione parallela</span>
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
          className="flex-1 flex-col-gap-6 text-base overflow-auto no-scrollbar"
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
          {isReconnecting && (
            <div
              className="flex-row-gap-8 text-base"
              style={{
                position: "sticky",
                top: 0,
                zIndex: 8,
                alignSelf: "stretch",
                padding: "8px 12px",
                borderRadius: 10,
                border: "1px solid #f9731680",
                background: tc.bgCard,
                borderLeft: "3px solid #f97316",
                color: "#f97316",
              }}
            >
              <span style={{ animation: "spin 1s linear infinite", fontSize: 16 }}>↻</span>
              <strong>Connessione persa</strong>
              <span style={{ color: tc.textMuted, fontSize: 12 }}>
                — Riconnessione al server in corso, attendere…
              </span>
            </div>
          )}
          {reconnectSuccess && !isReconnecting && (
            <div
              className="flex-row-gap-8 text-base"
              style={{
                position: "sticky",
                top: 0,
                zIndex: 8,
                alignSelf: "stretch",
                padding: "8px 12px",
                borderRadius: 10,
                border: "1px solid #22c55e80",
                background: tc.bgCard,
                borderLeft: "3px solid #22c55e",
                color: "#22c55e",
              }}
            >
              <span style={{ fontSize: 16 }}>✓</span>
              <strong>Connessione ripristinata</strong>
            </div>
          )}

          <MessageList
            messages={messages}
            busyByMessage={busyByMessage}
            tc={tc}
            t={t as (key: string) => string}
            onCopy={copyMessage}
            onResend={handleResend}
            onDelete={handleDelete}
            onFeedback={handleFeedback}
            lastUserRef={lastUserRef}
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

          {agentRun && (agentRun.status === "running" || agentRun.status === "awaiting_confirmation") && (
            <AgentStepsPanel
              agentRun={agentRun}
              agentSteps={agentSteps}
              agentRuns={agentRuns}
              agentStepsMap={agentStepsMap}
              tc={tc}
              t={t as (key: string) => string}
              onConfirm={handleConfirmAgent}
              streamingToken={agentRun.status === "running" ? streamingToken : undefined}
              narrationWarnAfterMs={narrationWarnAfterMs}
              narrationWarnAfterChars={narrationWarnAfterChars}
            />
          )}

          {traces.length > 0 && <InlineTracePanel traces={traces} />}

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
        <div
          style={{
            margin: "6px 0 0",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            display: "flex",
            alignItems: "center",
            gap: 8,
            flexShrink: 0,
            boxShadow: "0 2px 8px rgba(0,0,0,0.10)",
          }}
          aria-live="polite"
        >
          {/* Riga principale */}
          <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "7px 10px" }}>
            <span
              style={{
                width: 8,
                height: 8,
                borderRadius: "50%",
                background: "#22c55e",
                boxShadow: "0 0 0 2px #22c55e33",
                animation: "pulse 1s ease-in-out infinite",
                flexShrink: 0,
              }}
            />
            <strong style={{ color: isAgentStuck ? "#f97316" : tc.text, fontSize: 12 }}>
              {secondsSinceLastStep > 120 ? "⚠ AI in elaborazione" : isAgentStuck ? "⚠ Agente in attesa" : busyLabel}
            </strong>
            {isAgentRunning && runningAgentStep && (
              <span style={{ color: tc.textMuted, fontSize: 11 }}>
                step {runningAgentStep.stepIndex + 1} • {runningAgentStep.toolName}
              </span>
            )}
            {isAgentRunning && (
              <span style={{
                fontSize: 10,
                color: secondsSinceLastStep > 120 ? "#f97316" : tc.textMuted,
                fontVariantNumeric: "tabular-nums",
                marginLeft: 4,
              }}>
                {secondsSinceLastStep < 60
                  ? `${secondsSinceLastStep}s`
                  : `${Math.floor(secondsSinceLastStep / 60)}m ${secondsSinceLastStep % 60}s`}
              </span>
            )}
            {secondsSinceLastStep > 120 && agentRun?.runId && (
              <button
                type="button"
                onClick={() => void cancelRun(agentRun.runId)}
                style={{
                  fontSize: 10, padding: "2px 8px", borderRadius: 4,
                  border: "1px solid #f9731680", background: "#f9731618",
                  color: "#f97316", cursor: "pointer", fontWeight: 600,
                }}
              >
                Forza stop
              </button>
            )}
            {isAgentRunning && (
              <button
                type="button"
                onClick={() => setAgentStatusExpanded((v) => !v)}
                title={agentStatusExpanded ? "Comprimi dettagli" : "Espandi dettagli"}
                style={{
                  marginLeft: "auto", border: `1px solid ${tc.border}`,
                  background: "transparent", color: tc.text, borderRadius: 6,
                  width: 22, height: 22, display: "inline-flex", alignItems: "center",
                  justifyContent: "center", cursor: "pointer", fontSize: 11,
                }}
              >
                {agentStatusExpanded ? "▾" : "▸"}
              </button>
            )}
          </div>
          {/* Dettagli espandibili */}
          {isAgentRunning && agentStatusExpanded && (
            <div style={{
              borderTop: `1px solid ${tc.border}`,
              padding: "6px 10px 8px",
              display: "flex", flexDirection: "column", gap: 4,
            }}>
              <div style={{ color: tc.textMuted, fontSize: 11 }}>
                Step completati: {completedSteps}
                {runningSteps > 0 ? ` • in corso: ${runningSteps}` : ""}
                {failedSteps > 0 ? ` • falliti: ${failedSteps}` : ""}
              </div>
              {runningCommand && (
                <div style={{
                  fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                  fontSize: 11, color: tc.textSecondary,
                  whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                }} title={runningCommand}>
                  cmd: {runningCommand}
                </div>
              )}
              {latestOutputSnippet && (
                <div style={{
                  fontSize: 11, color: tc.textMuted,
                  whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                }} title={latestStepWithOutput?.toolResult}>
                  output: {latestOutputSnippet}
                </div>
              )}
              {timelineSteps.length > 0 && (
                <div style={{ marginTop: 2, paddingTop: 4, borderTop: `1px dashed ${tc.border}`, display: "flex", flexDirection: "column", gap: 1 }}>
                  {timelineSteps.map((step) => (
                    <div
                      key={`tl-${step.stepIndex}`}
                      style={{
                        color: step.status === "failed" ? tc.error : tc.textSecondary,
                        fontSize: 11, fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                        whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                      }}
                    >
                      {step.stepIndex + 1}. {step.toolName} —{" "}
                      {step.status === "completed" ? "ok" : step.status === "running" ? "in corso" : step.status === "failed" ? "errore" : step.status}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Token usage bar */}
      <TokenUsageBar
        totalTokens={tokenUsage.totalTokens}
        totalCostUsd={tokenUsage.totalCostUsd}
      />

      {/* Precheck widget */}
      {precheckPending && (
        <div style={{
          margin: "0 8px 4px",
          padding: "8px 12px",
          borderRadius: 8,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          fontSize: 12,
          color: tc.textMuted,
          display: "flex",
          alignItems: "center",
          gap: 8,
        }}>
          <span style={{ animation: "spin 1s linear infinite", display: "inline-block" }}>⟳</span>
          Controllo ortografia e contesto…
        </div>
      )}
      {precheckResult && !precheckPending && (
        <div style={{
          margin: "0 8px 6px",
          borderRadius: 8,
          border: `1px solid ${tc.accent}66`,
          background: tc.bgCard,
          fontSize: 12,
          overflow: "hidden",
        }}>
          {/* Header */}
          <div style={{
            padding: "7px 12px",
            borderBottom: `1px solid ${tc.border}`,
            background: tc.bg,
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}>
            <span style={{ fontWeight: 600, color: tc.accent, fontSize: 11 }}>
              ✦ Suggerimento
            </span>
            <button
              onClick={() => setPrecheckResult(null)}
              style={{ background: "none", border: "none", color: tc.textMuted, cursor: "pointer", fontSize: 14, lineHeight: 1 }}
            >×</button>
          </div>

          <div style={{ padding: "10px 12px", display: "flex", flexDirection: "column", gap: 8 }}>
            {/* Testo corretto */}
            {precheckResult.correctedText && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4, textTransform: "uppercase", letterSpacing: "0.04em" }}>
                  Testo corretto
                </div>
                <div style={{
                  padding: "6px 8px",
                  borderRadius: 6,
                  background: tc.bg,
                  border: `1px solid ${tc.success}44`,
                  color: tc.text,
                  whiteSpace: "pre-wrap",
                }}>
                  {precheckResult.correctedText}
                </div>
              </div>
            )}

            {/* Suggerimento contesto */}
            {precheckResult.contextSuggestion && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4, textTransform: "uppercase", letterSpacing: "0.04em" }}>
                  Aggiungi contesto
                </div>
                <div style={{
                  padding: "6px 8px",
                  borderRadius: 6,
                  background: tc.bg,
                  border: `1px solid ${tc.accent}44`,
                  color: tc.textMuted,
                  fontStyle: "italic",
                }}>
                  {precheckResult.contextSuggestion}
                </div>
              </div>
            )}

            {/* Problemi */}
            {(precheckResult.issues?.length ?? 0) > 0 && (
              <div style={{ fontSize: 11, color: tc.textMuted }}>
                {(precheckResult.issues ?? []).map((issue, i) => (
                  <span key={i} style={{ marginRight: 8 }}>• {issue}</span>
                ))}
              </div>
            )}

            {/* Azioni */}
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: 2 }}>
              {precheckResult.correctedText && (
                <button
                  onClick={() => {
                    // Invia direttamente il testo corretto (non ri-triggerare il precheck)
                    doSend(precheckResult.correctedText!);
                  }}
                  style={{
                    padding: "5px 12px", borderRadius: 6, border: "none",
                    background: tc.accent, color: "#fff",
                    cursor: "pointer", fontSize: 11, fontWeight: 600,
                  }}
                >
                  Usa testo corretto
                </button>
              )}
              {precheckResult.contextSuggestion && (
                <button
                  onClick={() => {
                    // Invia direttamente il testo originale + suggerimento contesto
                    doSend(precheckResult.originalText + "\n\n" + precheckResult.contextSuggestion!);
                  }}
                  style={{
                    padding: "5px 12px", borderRadius: 6,
                    border: `1px solid ${tc.accent}`,
                    background: "none", color: tc.accent,
                    cursor: "pointer", fontSize: 11,
                  }}
                >
                  Aggiungi contesto
                </button>
              )}
              <button
                onClick={() => doSend(precheckResult.originalText)}
                style={{
                  padding: "5px 12px", borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: "none", color: tc.textMuted,
                  cursor: "pointer", fontSize: 11,
                }}
              >
                Invia comunque
              </button>
            </div>
          </div>
        </div>
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
        runProvider={agentRun?.provider ?? null}
        runModel={agentRun?.model ?? null}
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
        onStopAgent={() => { if (agentRun?.runId) void cancelRun(agentRun.runId); }}
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
    </>
  );
}
