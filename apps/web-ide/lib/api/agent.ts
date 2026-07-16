import { API_BASE, NEURAL_BASE, adminServiceUrl, fetchJson, fetchJsonNoAuth } from "./_shared";
import type { MetaStepEntry } from "../use-chat/types";

export interface AgentStep {
  stepIndex: number;
  toolName: string;
  toolInput: Record<string, unknown>;
  toolResult?: string;
  status: "running" | "completed" | "failed" | "awaiting_confirmation" | "skipped" | "provider_unavailable";
  createdAt: string;
  // FIX D2: rimossi i campi metriche per-step (usage/costUsd/latencyMs/
  // temperature/topP): il motore Rust non li popola, la UI relativa era sempre
  // vuota. I totali di run restano su AgentRunInfo (usage/totalCostUsd/...).
}

interface AITraceToolCall {
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

interface AgentPendingAction {
  index: number;
  toolName: string;
  toolInput: Record<string, unknown>;
  description: string;
}

interface AgentRunUsage {
  totalPromptTokens?: number;
  /** Prompt token dell'ultima iterazione (riempimento contesto). Popolato SOLO
      dagli eventi live agent_usage: i run letti dal DB non lo hanno (il ratio
      ctx% ricade sul lastPromptTokens del messaggio assistant persistito).
      totalPromptTokens NON va usato per il ratio: dal DB e' il cumulativo. */
  lastPromptTokens?: number;
  totalCompletionTokens?: number;
  totalTokens?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
}

export interface AgentRunInfo {
  runId: string;
  sessionId: string;
  status: "running" | "completed" | "awaiting_confirmation" | "awaiting_subagents" | "failed" | "timed_out" | "cancelled" | "interrupted" | "loop_aborted" | "provider_unavailable" | "completed_verified" | "completed_unverified" | "failed_diagnosed" | "blocked_needs_input";
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

/** Timeline meta_step persistita (plan/routing/clarify/fallback/reflection/
 *  next_actions) per i run di una sessione, raggruppata per runId. Usato per
 *  RIPRISTINARE `metaStepsMap` dopo un reload: gli eventi SSE vivono solo in
 *  memoria e si perdono al refresh, percio' la timeline delle card sparirebbe
 *  pur restando nel DB. Risposta: { runs: { "<runId>": MetaStepEntry[] } }. */
export async function getSessionMetaSteps(
  sessionId: string,
): Promise<{ runs: Record<string, MetaStepEntry[]> }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/meta-steps`);
}

export async function getActiveRunForSession(
  sessionId: string,
): Promise<{ activeRun: AgentRunInfo | null }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/active-run`);
}

/** Tracce gateway LLM (AITraceEvent: provider/model effettivi, token, stop_reason
 *  per iterazione) persistite per i run di una sessione, raggruppate per runId.
 *  Usato per RIPRISTINARE il pannello tracce dopo un reload: gli eventi SSE
 *  `agent_trace` (e la cache sessionStorage) sono volatili/per-dispositivo, qui
 *  li rileggiamo dal DB (nexus_agent_traces, mig 0485) cosi' il pannello converge
 *  con il rendering live. Il DB e' la fonte autoritativa (regola L).
 *  Risposta backend: { runs: { "<runId>": AITraceEvent[] } } -- vedi
 *  crates/mcp-core/src/chat_agent.rs::get_session_traces e trace_store.rs. */
export async function getSessionTraces(
  sessionId: string,
): Promise<{ runs: Record<string, AITraceEvent[]> }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/traces`);
}

export async function confirmAgentRun(
  runId: string,
  approved: boolean,
): Promise<{ runId: string; status: string }> {
  // Il resume nativo gira in background: la POST risponde subito. Timeout breve
  // basta per il round-trip DB; evita attese da 30s del default fetchJson.
  return fetchJson(
    `${API_BASE}/api/chat/agent-runs/${runId}/confirm`,
    {
      method: "POST",
      body: JSON.stringify({ approved }),
    },
    15_000,
  );
}

export async function cancelAgentRun(
  runId: string,
): Promise<{ runId: string; status: string }> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}/cancel`, {
    method: "POST",
  });
}

// --- Neural Core (mcp-core, endpoint AI sotto /api/neural :4000) ---

export interface ProviderModelsResponse {
  provider: string;
  status: string;
  models: string[];
}

export async function getProviderModels(provider: string): Promise<ProviderModelsResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/providers/${provider}/models`);
}

export interface ProvidersResponse {
  status: string;
  providers: string[];
}

/** Elenco dei provider attivi dal catalog DB (regola G): controparte di
 *  getProviderModels. Alimenta il dropdown provider della chat cosi' che un
 *  provider aggiunto o rimosso dal catalog/routing matrix si rifletta senza
 *  liste hardcoded lato client. */
export async function getProviders(): Promise<ProvidersResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/providers`);
}

/** Stati terminali di un run agente (allineati al backend agent_runs.status:
 *  `AgentRunStatus::is_terminal`, punto unico regola L — l'omologo in
 *  use-chat/helpers.ts delega a questa funzione). */
export function isAgentRunTerminal(status: string): boolean {
  return (
    status === "completed" ||
    status === "failed" ||
    status === "timed_out" ||
    status === "cancelled" ||
    status === "interrupted" ||
    status === "loop_aborted" ||
    status === "provider_unavailable" ||
    // Esiti canonici macchina a stati (mig 0386): terminali.
    status === "completed_verified" ||
    // Svolto ma verifica tecnica non eseguita (mig 0531): terminale, successo onesto.
    status === "completed_unverified" ||
    status === "failed_diagnosed" ||
    // ADR 0034: blocked_needs_input e' TERMINALE — run CONCLUSO con la
    // dichiarazione "serve input umano"; il prossimo messaggio crea un nuovo
    // run (solo awaiting_confirmation / awaiting_subagents restano run sospesi
    // con resume, vedi isAgentRunSuspendedWaiting).
    status === "blocked_needs_input"
  );
}

/** Stati NON-terminali in cui il run e' SOSPESO in attesa che qualcosa accada
 *  (punto unico regola L). Il run non e' finito: lo stream resta aperto, la
 *  chat NON deve trattarlo come "run concluso" (niente reset dello stato agente,
 *  tasto invio coerente col run vivo, reattach su mount/reconnect).
 *
 *  - `awaiting_confirmation` (HITL): l'agente aspetta l'approvazione dell'utente
 *    su azioni pendenti (backend fa resume alla conferma).
 *  - `awaiting_subagents` (fan-in async): il run PADRE ha dispatchato sub-agent
 *    in background; il backend lo sospende e lo riprende quando i figli
 *    completano (flag ora OFF). La UI deve mostrarlo come "in attesa dei
 *    sub-agent", non come finito. Vedi crates lato backend (agent_runs.status).
 *
 *  Entrambi restano fuori da isAgentRunTerminal (corretto): sono stati vivi. */
export function isAgentRunSuspendedWaiting(status: string): boolean {
  return status === "awaiting_confirmation" || status === "awaiting_subagents";
}

/** true se il run tiene la chat "viva" (in esecuzione o sospeso in attesa):
 *  usato per il reattach al run attivo e per mantenere aperto lo stream/pannello
 *  step. Punto unico (regola L): sostituisce i confronti duplicati
 *  `status === "running" || status === "awaiting_confirmation"` sparsi in
 *  useChat / chat-panel. */
export function isAgentRunLiveOrWaiting(status: string): boolean {
  return status === "running" || isAgentRunSuspendedWaiting(status);
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
    if (pollTimer) {
      clearTimeout(pollTimer);
      pollTimer = null;
    }
    if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      // Cap dei tentativi consecutivi raggiunto. Consulta il DB: se il run e'
      // gia' terminato chiudi pulito; ma se e' ANCORA running NON arrenderti.
      // CAUSA RADICE del bug "la chat si ferma e riparte solo al refresh": prima
      // qui si faceva finish(true) INCONDIZIONATO, percio' dopo ~100s di stream
      // morto (es. restart di mcp-core o run lento con provider in cooldown) il
      // frontend mollava la sottoscrizione mentre il backend continuava a
      // lavorare -> chat ferma -> refresh manuale obbligato. Ora, se il run e'
      // vivo, azzeriamo il contatore e continuiamo a riconnettere: il frontend si
      // riaggancia da solo appena lo stream torna disponibile.
      getAgentRun(runId)
        .then((run) => {
          if (closed) return;
          if (isAgentRunTerminal(run.status)) {
            onStep({ runId, step: null, isFinal: true });
            finish(true);
          } else {
            reconnectAttempts = 0;
            scheduleReconnect();
          }
        })
        .catch(() => {
          // Poll fallito (backend probabilmente in riavvio): non arrenderti.
          if (!closed) {
            reconnectAttempts = 0;
            scheduleReconnect();
          }
        });
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
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
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
// Client Orchestrator admin. I CRUD sub-agent parlano con admin-service
// (:4010) tramite il rewrite Next `/api/admin/:path*` (next.config.ts).
// NON creare route handler in app/api/admin/orchestrator/**: userebbero
// proxyRequest (che punta a mcp-core :4000) e romperebbero il percorso.
// ---------------------------------------------------------------------------

export async function listOrchestratorPlans(_args?: { limit?: number; projectId?: string }): Promise<{ plans: OrchestratorPlanSummary[] }> {
  // Stub: la pagina Orchestrator (plans) non e' ancora cablata all'endpoint
  // admin-service /orchestrator/plans.
  return { plans: [] };
}

export async function getOrchestratorPlan(_runId: string): Promise<OrchestratorPlanDetail | null> {
  return null;
}

export async function listSubagentDefinitions(): Promise<{ definitions: SubagentDefinition[] }> {
  return fetchJson(adminServiceUrl("/orchestrator/subagents/definitions"));
}

export async function listSubagentRuns(args?: {
  parentRunId?: string;
  kind?: string;
  projectId?: string;
  limit?: number;
}): Promise<{ runs: OrchestratorSubagentRun[] }> {
  // Il backend applica UN solo filtro, con precedenza parent_run_id > kind > project_id.
  const params = new URLSearchParams();
  if (args?.parentRunId) params.set("parent_run_id", args.parentRunId);
  if (args?.kind) params.set("kind", args.kind);
  if (args?.projectId) params.set("project_id", args.projectId);
  if (args?.limit) params.set("limit", String(args.limit));
  const qs = params.toString();
  return fetchJson(adminServiceUrl(`/orchestrator/subagents/runs${qs ? `?${qs}` : ""}`));
}

export async function upsertSubagentDefinition(
  def: SubagentDefinitionUpsert,
): Promise<{ ok: boolean; kind: string }> {
  return fetchJson(adminServiceUrl("/orchestrator/subagents/definitions"), {
    method: "POST",
    body: JSON.stringify(def),
  });
}

/** Soft delete: il backend imposta is_enabled=false, la riga resta. */
export async function deleteSubagentDefinition(
  kind: string,
): Promise<{ ok: boolean; kind: string; soft_deleted: boolean }> {
  return fetchJson(
    adminServiceUrl(`/orchestrator/subagents/definitions/${encodeURIComponent(kind)}`),
    { method: "DELETE" },
  );
}

export async function resetProviderCooldown(_providerKey: string): Promise<{ ok: boolean }> {
  return { ok: true };
}

// ── Figure (lenti) del consiglio ─────────────────────────────────────────────
// Una figura convocabile richiede QUATTRO pezzi coerenti: definition, prompt
// subagent.<kind>.base, purpose subagent_<kind> e appartenenza alla whitelist del
// dispatcher. upsertSubagentDefinition ne crea UNO (la definition): un kind creato
// solo cosi' resta muto. L'endpoint /orchestrator/figures li crea in UNA
// transazione (tutti o nessuno).

/** Body di POST /api/admin/orchestrator/figures (snake_case, come l'upsert definitions). */
export interface CreateFigureBody {
  kind: string;
  description: string;
  /** true = figura read-only che chiude con advisory_verdict; false = sub-agente esecutivo. */
  advisory: boolean;
  /** Fascia di capacita' canonica (light|medium|high|heavy|frontier). Il purpose nasce
   *  con provider/model_id vuoti: il modello concreto lo sceglie best_model_for_tier
   *  dal catalog a ogni convocazione (regola G) — qui non si nomina mai un modello. */
  tier: string;
  prompt_content: string;
  prompt_title?: string;
  tool_whitelist: string[];
  /** Omessi = il backend applica i propri default (unica fonte del valore). */
  max_iterations?: number;
  timeout_s?: number;
}

/** Chiavi derivate SERVER-side (punto unico delle derivazioni canoniche). */
export interface CreateFigureResult {
  ok: boolean;
  kind: string;
  prompt_key: string;
  purpose: string;
  whitelisted: boolean;
}

/** In errore il backend risponde {error, code, field}: fetchJson lo trasforma in
 *  ApiError con lo status HTTP (segnale strutturato su cui decidere, regola M) e
 *  il testo di `error` nel messaggio per il display. */
export async function createFigure(body: CreateFigureBody): Promise<CreateFigureResult> {
  return fetchJson(adminServiceUrl("/orchestrator/figures"), {
    method: "POST",
    body: JSON.stringify(body),
  });
}

/** Soft-delete della definition + rimozione dalla whitelist. Prompt e purpose restano (innocui). */
export async function deleteFigure(kind: string): Promise<{ ok: boolean; kind: string }> {
  return fetchJson(adminServiceUrl(`/orchestrator/figures/${encodeURIComponent(kind)}`), {
    method: "DELETE",
  });
}

/** Riparazione mirata della whitelist del dispatcher
 *  (setting orchestrator.subagent_kinds_whitelist): un kind fuori whitelist esiste
 *  ma non e' convocabile. add/remove sempre espliciti. */
export async function mutateKindsWhitelist(args: {
  add?: string[];
  remove?: string[];
}): Promise<{ ok: boolean }> {
  return fetchJson(adminServiceUrl("/orchestrator/subagents/whitelist"), {
    method: "POST",
    body: JSON.stringify({ add: args.add ?? [], remove: args.remove ?? [] }),
  });
}

// Tipi Orchestrator. Plan* restano stub-shaped (pagina non cablata); i tipi
// subagent rispecchiano il payload camelCase di admin-service
// (crates/admin-service/src/orchestrator_panel.rs).

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
  id: string;
  kind: string;
  status: string;
  task: string;
  parentRunId: string | null;
  projectId: string | null;
  iterations: number;
  tokensPrompt: number;
  tokensCompletion: number;
  costUsd: number;
  depth: number;
  source: string;
  createdAt: string | null;
  completedAt: string | null;
}
/** Riga di nexus_subagent_definitions come emessa da admin-service (camelCase). */
export interface SubagentDefinition {
  kind: string;
  description: string | null;
  promptKey: string;
  toolWhitelist: string[];
  modelPurpose: string;
  maxIterations: number;
  timeoutS: number;
  isBackground: boolean;
  isEnabled: boolean;
  updatedAt: string | null;
}

/** Body snake_case atteso dall'upsert admin-service (SubagentDefBody). */
export interface SubagentDefinitionUpsert {
  kind: string;
  description?: string | null;
  prompt_key: string;
  tool_whitelist: string[];
  model_purpose: string;
  max_iterations?: number;
  timeout_s?: number;
  is_background?: boolean;
  is_enabled?: boolean;
}

/**
 * Punto unico camelCase -> snake_case per l'upsert. L'endpoint fa un upsert
 * FULL-BODY (ON CONFLICT aggiorna tutte le colonne): anche il toggle di un
 * singolo campo deve rispedire l'intera definizione.
 */
export function toSubagentUpsertBody(d: SubagentDefinition): SubagentDefinitionUpsert {
  return {
    kind: d.kind,
    description: d.description,
    prompt_key: d.promptKey,
    tool_whitelist: d.toolWhitelist,
    model_purpose: d.modelPurpose,
    max_iterations: d.maxIterations,
    timeout_s: d.timeoutS,
    is_background: d.isBackground,
    is_enabled: d.isEnabled,
  };
}
