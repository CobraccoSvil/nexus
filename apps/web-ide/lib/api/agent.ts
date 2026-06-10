import { API_BASE, NEURAL_BASE, fetchJson, fetchJsonNoAuth } from "./_shared";

export interface AgentStepUsage {
  promptTokens?: number;
  completionTokens?: number;
  totalTokens?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
}

export interface AgentStep {
  stepIndex: number;
  toolName: string;
  toolInput: Record<string, unknown>;
  toolResult?: string;
  status: "running" | "completed" | "failed" | "awaiting_confirmation" | "skipped" | "provider_unavailable";
  createdAt: string;
  // Metriche estese
  usage?: AgentStepUsage;
  costUsd?: number;
  latencyMs?: number;
  temperature?: number;
  topP?: number;
}

export interface AITraceToolCall {
  name: string;
  input: Record<string, unknown>;
}

export interface AITraceEvent {
  runId: string;
  iteration: number;
  provider: string;
  model: string;
  messagesSent: number;
  toolsCount: number;
  responseText: string;
  toolCalls: AITraceToolCall[];
  stopReason: string;
  timestamp: string;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
}

export interface AgentPendingAction {
  index: number;
  toolName: string;
  toolInput: Record<string, unknown>;
  description: string;
}

export interface AgentRunUsage {
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalTokens?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
}

export interface AgentRunInfo {
  runId: string;
  sessionId: string;
  status: "running" | "completed" | "awaiting_confirmation" | "failed" | "timed_out" | "cancelled" | "interrupted" | "loop_aborted" | "provider_unavailable" | "completed_verified" | "failed_diagnosed" | "blocked_needs_input";
  automationMode: string;
  provider: string;
  model: string;
  iterationCount: number;
  finalAnswer?: string;
  /** Avviso privacy per provider non-EU/non-locali (DeepSeek, OpenAI, Google, Anthropic). */
  providerPrivacyNotice?: string;
  pendingActions: AgentPendingAction[];
  steps: AgentStep[];
  createdAt: string;
  completedAt?: string;
  // Metriche estese del run complessivo
  usage?: AgentRunUsage;
  totalCostUsd?: number;
  totalLatencyMs?: number;
  cacheHitRate?: number;
  temperature?: number;
  topP?: number;
}

export interface AgentMetaStepPayload {
  runId: string;
  metaStep: {
    kind: "plan" | "routing" | "clarify" | "fallback" | "reflection" | string;
    title: string;
    payload: Record<string, unknown>;
    correlationId?: string | null;
    createdAt: string;
  };
}

export async function generateSystemPrompt(
  profileName: string,
  description?: string,
  provider?: string,
  model?: string,
): Promise<{ text: string }> {
  return fetchJson(`${API_BASE}/api/ai/generate-prompt`, {
    method: "POST",
    body: JSON.stringify({ profile_name: profileName, description, provider, model }),
  });
}

export async function getAgentRun(runId: string): Promise<AgentRunInfo> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}`);
}

export interface RunNextActionChoice {
  label?: string;
  prompt?: string;
}

/** Scelte di proseguimento (next_actions) persistite per un run. Usato per
 *  RIPRISTINARE i pulsanti delle scelte dopo un reload o sui turni passati: i
 *  meta_step live arrivano via SSE e si perdono al refresh, qui li rileggiamo dal
 *  DB. Ritorna sempre {choices: [...]}, eventualmente vuoto. */
export async function getAgentRunNextActions(
  runId: string,
): Promise<{ choices: RunNextActionChoice[] }> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}/next-actions`);
}

export async function getActiveRunForSession(
  sessionId: string,
): Promise<{ activeRun: AgentRunInfo | null }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/active-run`);
}

export async function confirmAgentRun(
  runId: string,
  approved: boolean,
): Promise<{ runId: string; status: string }> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}/confirm`, {
    method: "POST",
    body: JSON.stringify({ approved }),
  });
}

export async function cancelAgentRun(
  runId: string,
): Promise<{ runId: string; status: string }> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}/cancel`, {
    method: "POST",
  });
}

// --- Neural Core (Python :8001) ---

export interface IntentResponse {
  intent: string;
  confidence: string;
}

export interface RouteResponse {
  intent: string;
  provider: string;
  model: string;
  rationale: string;
  confidence: string;
}

export interface ProviderModelsResponse {
  provider: string;
  status: string;
  models: string[];
}

export async function classifyIntent(
  projectId: string,
  profileId: string,
  message: string,
): Promise<IntentResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/classify-intent`, {
    method: "POST",
    body: JSON.stringify({ project_id: projectId, profile_id: profileId, message }),
  });
}

export async function routeModel(
  projectId: string,
  profileId: string,
  message: string,
): Promise<RouteResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/route-model`, {
    method: "POST",
    body: JSON.stringify({ project_id: projectId, profile_id: profileId, message }),
  });
}

export async function getProviderModels(provider: string): Promise<ProviderModelsResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/providers/${provider}/models`);
}

export async function getProviderHealth(provider: string): Promise<Record<string, unknown>> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/providers/${provider}/health`);
}

export async function getNeuralHealth(): Promise<Record<string, string>> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/health`);
}

/** Stati terminali di un run agente (allineati al backend agent_runs.status). */
function isAgentRunTerminal(status: string): boolean {
  return (
    status === "completed" ||
    status === "failed" ||
    status === "timed_out" ||
    status === "cancelled" ||
    status === "interrupted" ||
    status === "loop_aborted" ||
    status === "provider_unavailable" ||
    // Esiti canonici macchina a stati (mig 0386): terminali. blocked_needs_input
    // NO: e' in attesa di input (come awaiting_confirmation), non terminale.
    status === "completed_verified" ||
    status === "failed_diagnosed"
  );
}

/**
 * Sottoscrive lo stream SSE di un run agente con AUTO-RECONNECT.
 *
 * Meccanismo: EventSource nativo verso `GET /agent-stream?run_id=X`. L'endpoint
 * backend (crates/mcp-core/src/chat_agent.rs::agent_stream) ad OGNI nuova
 * connessione fa REPLAY di tutti gli `agent_step` gia' persistiti + un
 * `agent_final` se il run e' gia' terminato, POI aggancia il broadcast live.
 *
 * Quando il backend si riavvia o c'e' un blip di rete, l'EventSource va in
 * `onerror`. NON usiamo il reconnect nativo del browser (non controllabile,
 * niente backoff, niente stop su fine-run): chiudiamo la connessione corrente
 * e riapriamo NOI con backoff esponenziale (1s, 2s, 4s, ..., cap 10s, max 10
 * tentativi). Il RESYNC e' garantito dal replay backend + dall'upsert per
 * `stepIndex` lato client (in use-chat), quindi la riconnessione NON duplica
 * step ne' ri-esegue il run. I token streaming non sono replayati (effimeri),
 * ma gli step persistiti e l'evento finale si': lo stato live torna coerente.
 *
 * Lo stop dei tentativi avviene quando: arriva `agent_final`, oppure un poll
 * di `getAgentRun` indica stato terminale, oppure si supera il cap tentativi,
 * oppure il chiamante invoca la funzione di cleanup ritornata (unmount / Stop).
 */
export function subscribeAgentStream(
  sessionId: string,
  runId: string,
  onStep: (event: { runId: string; step: AgentStep | null; isFinal: boolean }) => void,
  onDone?: () => void,
  onTrace?: (trace: AITraceEvent) => void,
  onReconnecting?: (isReconnecting: boolean) => void,
  onToken?: (delta: string) => void,
  onMetaStep?: (meta: AgentMetaStepPayload) => void,
  onThinking?: (text: string) => void,
  /** Snapshot token cumulativi live (evento `agent_usage`), emesso a ogni
   *  iterazione executor. Permette di aggiornare la barra context senza polling. */
  onUsage?: (usage: {
    totalTokens?: number;
    promptTokens?: number;
    completionTokens?: number;
    lastPromptTokens?: number;
    totalCostUsd?: number;
  }) => void,
): () => void {
  const url = `${API_BASE}/api/chat/sessions/${sessionId}/agent-stream?run_id=${runId}`;

  const MAX_RECONNECT_ATTEMPTS = 10;
  const BASE_DELAY_MS = 1000;
  const MAX_DELAY_MS = 10_000;

  // Stato condiviso tra le successive (ri)connessioni.
  let es: EventSource | null = null;
  let receivedFinal = false; // l'evento finale e' arrivato → run concluso
  let closed = false; // cleanup esplicito chiamato (unmount / Stop)
  let reconnectAttempts = 0; // tentativi di riconnessione consumati
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let signalledReconnecting = false; // evita toggle ridondanti di onReconnecting

  /** Chiude l'EventSource corrente senza terminare il loop di reconnect. */
  function closeCurrent(): void {
    if (es) {
      es.onerror = null;
      es.close();
      es = null;
    }
  }

  /** Termina definitivamente: niente piu' reconnect, niente piu' poll. */
  function finish(invokeDone: boolean): void {
    if (closed) return;
    closed = true;
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (pollTimer) {
      clearTimeout(pollTimer);
      pollTimer = null;
    }
    closeCurrent();
    if (signalledReconnecting) {
      signalledReconnecting = false;
      onReconnecting?.(false);
    }
    if (invokeDone) onDone?.();
  }

  /**
   * In caso di disconnessione transitoria: poll del run per capire se nel
   * frattempo e' terminato (es. l'agente ha finito proprio mentre cadeva la
   * rete). Se terminale → sintetizza l'evento finale e chiude. Altrimenti
   * pianifica una nuova connessione con backoff. Se il poll fallisce (server
   * ancora giu'), riprova comunque a riconnettere: il replay backend
   * ristabilira' lo stato.
   */
  function handleDisconnect(): void {
    if (closed || receivedFinal) {
      finish(receivedFinal);
      return;
    }
    if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      // Cap raggiunto: sblocca la UI consultando un'ultima volta il DB.
      getAgentRun(runId)
        .then((run) => {
          if (isAgentRunTerminal(run.status)) {
            onStep({ runId, step: null, isFinal: true });
          }
        })
        .catch(() => { /* ignora: chiudiamo comunque */ })
        .finally(() => finish(true));
      return;
    }

    if (!signalledReconnecting) {
      signalledReconnecting = true;
      onReconnecting?.(true);
    }

    // Poll del run prima di riconnettere: se gia' terminato evitiamo una
    // riconnessione inutile e sblocchiamo subito la UI.
    pollTimer = setTimeout(() => {
      if (closed) return;
      getAgentRun(runId)
        .then((run) => {
          if (closed) return;
          if (isAgentRunTerminal(run.status)) {
            onStep({ runId, step: null, isFinal: true });
            finish(true);
          } else {
            scheduleReconnect();
          }
        })
        .catch(() => {
          // Il backend e' probabilmente ancora in riavvio: riconnetti.
          if (!closed) scheduleReconnect();
        });
    }, 300);
  }

  /** Pianifica la riapertura dell'EventSource con backoff esponenziale. */
  function scheduleReconnect(): void {
    if (closed) return;
    const delay = Math.min(BASE_DELAY_MS * 2 ** reconnectAttempts, MAX_DELAY_MS);
    reconnectAttempts += 1;
    reconnectTimer = setTimeout(() => {
      if (closed) return;
      connect();
    }, delay);
  }

  /** Apre (o riapre) l'EventSource e registra gli handler. */
  function connect(): void {
    if (closed) return;
    closeCurrent();
    es = new EventSource(url, { withCredentials: true });

    es.addEventListener("open", () => {
      // Connessione (ri)stabilita: azzera backoff e segnala fine reconnect.
      reconnectAttempts = 0;
      if (signalledReconnecting) {
        signalledReconnecting = false;
        onReconnecting?.(false);
      }
    });

    es.addEventListener("agent_step", (e) => {
      try {
        const data = JSON.parse((e as MessageEvent).data);
        onStep(data);
      } catch {}
    });

    es.addEventListener("agent_trace", (e) => {
      try {
        const data = JSON.parse((e as MessageEvent).data);
        const trace = (data.trace ?? data) as AITraceEvent;
        onTrace?.(trace);
      } catch {}
    });

    es.addEventListener("agent_token", (e) => {
      try {
        const data = JSON.parse((e as MessageEvent).data);
        onToken?.(data.delta as string);
      } catch {}
    });

    es.addEventListener("agent_meta_step", (e) => {
      try {
        const data = JSON.parse((e as MessageEvent).data) as AgentMetaStepPayload;
        if (data?.metaStep?.kind) {
          onMetaStep?.(data);
        }
      } catch {}
    });

    es.addEventListener("agent_thinking", (e) => {
      try {
        const data = JSON.parse((e as MessageEvent).data);
        onThinking?.(data.text as string);
      } catch {}
    });

    es.addEventListener("agent_usage", (e) => {
      try {
        const data = JSON.parse((e as MessageEvent).data);
        onUsage?.(data);
      } catch {}
    });

    es.addEventListener("agent_final", (e) => {
      receivedFinal = true;
      try {
        const data = JSON.parse((e as MessageEvent).data);
        onStep(data);
      } catch {}
      finish(true);
    });

    es.onerror = () => {
      // EventSource error: il browser tenterebbe da solo, ma lo gestiamo noi
      // con backoff controllato e stop su fine-run. Chiudiamo la connessione
      // corrente e valutiamo se riconnettere.
      closeCurrent();
      handleDisconnect();
    };
  }

  connect();

  // Cleanup: invocato su unmount, Stop utente o cambio sessione. NON chiama
  // onDone (il run non e' "concluso", la UI semplicemente smette di ascoltare).
  return () => finish(false);
}

// ---------------------------------------------------------------------------
// Stubs Orchestrator (PR-4 admin panel) — endpoint non ancora esposti.
// Mantenuti come placeholder per consentire il build delle pagine admin
// (OrchestratorPanel, SubagentDefinitionsEditor) finche' i client reali non
// vengono cablati. Ritornano risposte vuote / mock.
// ---------------------------------------------------------------------------

export async function listOrchestratorPlans(_args?: { limit?: number; projectId?: string }): Promise<{ plans: OrchestratorPlanSummary[] }> {
  return { plans: [] };
}

export async function getOrchestratorPlan(_runId: string): Promise<OrchestratorPlanDetail | null> {
  return null;
}

export async function listSubagentDefinitions(): Promise<{ definitions: SubagentDefinition[] }> {
  return { definitions: [] };
}

export async function listSubagentRuns(_args?: { parentRunId?: string; kind?: string; projectId?: string; limit?: number }): Promise<{ runs: OrchestratorSubagentRun[] }> {
  return { runs: [] };
}

export async function upsertSubagentDefinition(_def: unknown): Promise<{ ok: boolean }> {
  return { ok: true };
}

export async function deleteSubagentDefinition(_kind: string): Promise<{ ok: boolean }> {
  return { ok: true };
}

export async function resetProviderCooldown(_providerKey: string): Promise<{ ok: boolean }> {
  return { ok: true };
}

// Tipi stub Orchestrator (corrispondono ai placeholder API sopra).

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type _Any = any;
export interface OrchestratorPlanSummary {
  runId: string; projectId: string; createdAt: string;
  status?: string; intent?: string; provider?: string; model?: string;
  plannerModel?: string; costUsd?: number;
  todosTotal?: number; todosDone?: number;
  verifierRuns?: number; subagentRuns?: number;
  approvedAt?: string | null; approvedBy?: string | null; score?: number | null;
}
export interface OrchestratorPlanDetail {
  runId: string;
  todos: _Any[]; verifierRuns: _Any[]; subagentRuns: _Any[];
  plan?: _Any;
  [k: string]: _Any;
}
export interface OrchestratorSubagentRun {
  id: string; kind: string; status: string; createdAt: string;
  costUsd?: number; tokensPrompt?: number; tokensCompletion?: number;
  iterations?: number; parentRunId?: string;
  [k: string]: _Any;
}
export interface SubagentDefinition {
  kind: string; description: string; promptKey: string;
  toolWhitelist: string[]; modelPurpose: string;
  maxIterations: number; timeoutS: number;
  isBackground: boolean; isEnabled: boolean;
  [k: string]: _Any;
}
