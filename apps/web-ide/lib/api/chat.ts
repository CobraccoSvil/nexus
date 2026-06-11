import { API_BASE, fetchJson } from "./_shared";

export interface ChatSessionSummary {
  id: string;
  projectId: string;
  title: string;
  status: string;
  messageCount: number;
  lastMessageAt?: string;
  lastMessagePreview?: string;
  createdAt: string;
  updatedAt: string;
}

export interface ChatMessage {
  id: string;
  sessionId: string;
  projectId: string;
  role: "user" | "assistant";
  content: string;
  requestMessageId?: string;
  deletedAt?: string;
  createdAt: string;
  provider?: string;
  model?: string;
  intent?: string;
  runId?: string;
  promptTokens?: number;
  completionTokens?: number;
  totalTokens?: number;
  totalCost?: number;
  currency?: string;
  automationMode?: "study" | "confirm" | "automatic" | "agent";
  resendOfMessageId?: string;
  /** True quando il messaggio e' stato generato automaticamente dal sistema
      (es. auto-continuazione in modalita' "automatic"). La UI lo nasconde
      per non confondere l'utente: il backend lo persiste comunque per
      coerenza del run. Vedere chat_messages.rs::synthetic. */
  synthetic?: boolean;
  /** Allegati persistiti su filesystem + chat_message_attachments associati
      al messaggio. Popolato dal backend tanto in send_chat_message (subito
      dopo l'INSERT del messaggio user) quanto in list_chat_messages (su
      refresh sessione). Quando vuoto/undefined il messaggio non ha allegati. */
  attachments?: SavedChatAttachment[];
}

/** Allegato pronto per essere inviato al backend (input dell'utente). */
export interface ChatAttachment {
  name: string;
  mimeType: string;
  sizeBytes: number;
  textContent: string;
  base64Content?: string;
}

/** Allegato persistito dal backend (output dopo invio o refresh sessione).
 *  Vedi crates/mcp-core/src/chat_attachments.rs::SavedAttachment. */
export interface SavedChatAttachment {
  id: string;
  messageId: string;
  projectId: string;
  fileName: string;
  filePath: string;
  mimeType: string;
  sizeBytes: number;
  /** Categoria derivata dal mime: governa la pipeline KB
   *  ('text' indicizzabile, 'image' metadata-only, 'binary' non indicizzabile). */
  kind: "text" | "image" | "binary";
  /** Id della nota KB associata, se l'allegato e' stato indicizzato. */
  kbNoteId?: string | null;
  /** Timestamp di indicizzazione KB. Null se ancora non indicizzato. */
  indexedAt?: string | null;
  createdAt: string;
}

export interface SendChatMessageOptions {
  profileId?: string;
  activeFiles?: string[];
  providerOverride?: string;
  modelOverride?: string;
  automationMode?: "study" | "confirm" | "automatic";
  supervisorMode?: "none" | "anomaly" | "interleaved" | "continuous";
  attachments?: ChatAttachment[];
  // BP13 piano riduzione token: limita la finestra di messaggi inviati al
  // backend. Il backend ricostruisce il contesto piu' vecchio dal DB e/o
  // dal summarizer (BP4). Default: 30 messaggi -- copre i 6 protetti dal
  // summarizer + 24 messaggi recenti.
  messageWindowSize?: number;
  /** Se true, marca il messaggio come auto-generato dal sistema (es. auto-continuazione).
      Il backend lo persiste in metadata.synthetic; la UI lo nasconde. */
  synthetic?: boolean;
  /** Hint strutturale sul tipo di agente (es. "debugger" dai pannelli error-fix).
      Il backend lo mappa su agent_type_hint -> nexus_agent_type_hint, attiva
      agent_type_forced e SALTA la disambiguazione d'intent (A/B). Non dedotto dal
      testo: e' un parametro esplicito del call site. */
  agentTypeHint?: string;
}

export interface FeedbackErrorResponse {
  ok: boolean;
  feedbackId: string;
  correctionId: string;
  deduplicatedCount: number;
  learning: Record<string, unknown>;
}

export interface FeedbackPositiveResponse {
  ok: boolean;
  feedbackId: string;
  alreadyRecorded: boolean;
  newQValue: number | null;
}

export interface SendChatMessageResponse {
  sessionId: string;
  userMessage: ChatMessage;
  assistantMessage?: ChatMessage;
  agentRun?: { runId: string; status: string; provider: string; model: string };
  /** Lista di allegati salvati su filesystem + DB. Popolato anche se la
   *  modalita' selezionata produce solo un agentRun (no assistantMessage). */
  savedAttachments?: SavedChatAttachment[];
}

export interface CompactSessionResponse {
  ok: boolean;
  summary: string;
  pointId: string;
  // Totali post-compact: la UI aggiorna la barra token in modo sincrono dalla
  // risposta HTTP, senza dipendere dall'evento SSE ChatSessionCompacted (che
  // puo' perdersi se nessun client e' sottoscritto al topic in quell'istante).
  totalTokens: number;
  totalCostUsd: number;
}

export interface ProjectMemory {
  id: string;
  sessionId?: string;
  sessionTitle: string;
  summary: string;
  active: boolean;
  createdAt: string;
}

export interface IndexAttachmentsToKbResponse {
  indexed: Array<{ attachmentId: string; kbNoteId: string }>;
  skipped: Array<{ attachmentId: string; reason: string }>;
}

export interface PrecheckResult {
  ok: boolean;
  correctedText: string | null;
  contextSuggestion: string | null;
  issues: string[];
  reason: string | null;
}

export async function getChatSessions(projectId: string): Promise<{ sessions: ChatSessionSummary[] }> {
  const url = new URL(`${API_BASE}/api/chat/sessions`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("projectId", projectId);
  return fetchJson(url.toString());
}

export async function createChatSession(
  projectId: string,
  title?: string,
): Promise<{ session: { id: string; projectId: string; title: string; status: string } }> {
  return fetchJson(`${API_BASE}/api/chat/sessions`, {
    method: "POST",
    body: JSON.stringify({ projectId, title }),
  });
}

export async function renameChatSession(
  sessionId: string,
  title: string,
): Promise<{ ok: boolean; title: string }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}`, {
    method: "PATCH",
    body: JSON.stringify({ title }),
  });
}

export async function deleteChatSession(
  sessionId: string,
): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}`, {
    method: "DELETE",
  });
}

export async function compactChatSession(
  sessionId: string,
): Promise<CompactSessionResponse> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/compact`, {
    method: "POST",
  });
}

export async function listProjectMemories(
  projectId: string,
): Promise<{ memories: ProjectMemory[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/memories`);
}

export async function toggleProjectMemory(
  memoryId: string,
): Promise<{ ok: boolean; active: boolean }> {
  return fetchJson(`${API_BASE}/api/memories/${memoryId}/toggle`, {
    method: "PATCH",
  });
}

export async function getChatMessages(
  sessionId: string,
): Promise<{ sessionId: string; projectId: string; messages: ChatMessage[] }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/messages`);
}

export async function sendChatMessage(
  sessionId: string,
  content: string,
  options: SendChatMessageOptions = {},
): Promise<SendChatMessageResponse> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/messages`, {
    method: "POST",
    body: JSON.stringify({
      content,
      profileId: options.profileId ?? "default",
      activeFiles: options.activeFiles ?? [],
      providerOverride: options.providerOverride,
      modelOverride: options.modelOverride,
      // Niente default mascherante: se il valore manca si invia undefined (omesso
      // dal JSON) e il backend usa la modalita' persistita della sessione
      // (mig 0371), invece di forzare 'confirm' e ignorare la scelta dell'utente.
      automationMode: options.automationMode,
      supervisorMode: options.supervisorMode ?? "none",
      attachments: options.attachments ?? [],
      // BP13: dichiara la finestra di messaggi che il client e' disposto a
      // inviare. Il backend usa questo hint per pruning lato suo.
      messageWindowSize: options.messageWindowSize ?? 30,
      synthetic: options.synthetic ?? false,
      // Hint strutturale (es. "debugger" dai pannelli error-fix): se assente si
      // invia undefined (omesso dal JSON) e il backend classifica l'intent come
      // di consueto. Vedi SendChatMessageOptions.agentTypeHint.
      agentTypeHint: options.agentTypeHint,
    }),
  }, 120000);
}

/** Indicizza nella Knowledge Base gli allegati selezionati di un messaggio.
 *  Crea una nota in project_knowledge_notes con embedding in Qdrant
 *  (collezione `knowledge_notes`), e popola kb_note_id+indexed_at su
 *  chat_message_attachments. Vedi crates/mcp-core/src/chat_attachments.rs. */
export async function indexAttachmentsToKb(
  messageId: string,
  attachmentIds: string[],
): Promise<IndexAttachmentsToKbResponse> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}/attachments/index`, {
    method: "POST",
    body: JSON.stringify({ attachmentIds }),
  });
}

/** URL da usare in `<img src>` o link di download per scaricare i bytes
 *  raw di un allegato. Il backend valida l'accesso via project_id. */
export function getAttachmentRawUrl(attachmentId: string): string {
  return `${API_BASE}/api/chat/attachments/${encodeURIComponent(attachmentId)}/raw`;
}

export async function resendChatMessage(
  messageId: string,
  options: SendChatMessageOptions = {},
): Promise<{ sessionId: string; userMessage?: ChatMessage; assistantMessage?: ChatMessage; agentRun?: { runId: string; status: string; provider: string; model: string }; savedAttachments?: SavedChatAttachment[] }> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}/resend`, {
    method: "POST",
    body: JSON.stringify({
      content: "",
      profileId: options.profileId ?? "default",
      activeFiles: options.activeFiles ?? [],
      providerOverride: options.providerOverride,
      modelOverride: options.modelOverride,
      automationMode: options.automationMode,
      attachments: options.attachments ?? [],
    }),
  }, 120000);
}

export async function deleteChatMessage(
  messageId: string,
): Promise<{ ok: boolean; messageId: string }> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}`, {
    method: "DELETE",
  });
}

export async function feedbackChatMessageError(
  messageId: string,
  comment: string,
): Promise<FeedbackErrorResponse> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}/feedback-error`, {
    method: "POST",
    body: JSON.stringify({ comment }),
  });
}

export async function feedbackChatMessagePositive(
  messageId: string,
  comment?: string,
): Promise<FeedbackPositiveResponse> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}/feedback-positive`, {
    method: "POST",
    body: JSON.stringify(comment ? { comment } : {}),
  });
}

// --- Feedback Assist ---
export async function feedbackAssist(
  messageContent: string,
  description: string,
): Promise<{ suggestion: string }> {
  return fetchJson(`${API_BASE}/api/chat/feedback-assist`, {
    method: "POST",
    body: JSON.stringify({ message_content: messageContent, description }),
  });
}

// ── Esecuzione comandi dalla chat ─────────────────────────────────────

export interface CommandExecResult {
  exit_code: number;
  stdout: string;
  stderr: string;
  blocked: boolean;
  blocked_reason?: string;
  duration_ms: number;
}

/** Esegue un comando shell nel contesto del progetto.
 *  Il comando viene validato da safety check prima dell'esecuzione.
 *  Usa URL relativo per transitare dal proxy Next.js (cookie HttpOnly). */
export async function executeProjectCommand(
  projectId: string,
  command: string,
  timeoutSecs?: number,
): Promise<CommandExecResult> {
  // Timeout backend max 120s — il timeout frontend deve coprirlo con margine.
  const backendTimeout = timeoutSecs ?? 60;
  const clientTimeoutMs = (backendTimeout + 10) * 1000;
  return fetchJson(`/api/projects/${projectId}/execute-command`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ command, timeout_secs: timeoutSecs }),
  }, clientTimeoutMs);
}
