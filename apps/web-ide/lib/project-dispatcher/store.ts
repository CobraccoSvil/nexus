// Zustand store centrale per i dati operativi del progetto attivo.
// Tutti i pannelli (Playwright, Ports, Problems, Services, Files, Git, Database,
// Flags, Monitor) leggono da qui. L'unica fonte di verita' e' il dispatcher
// backend via SSE — niente piu' polling indipendente per pannello.

import { create } from "zustand";
import type { PlaywrightArtifact } from "../api-client";
import type {
  ConnectionStatus,
  EnvelopedEvent,
  MonitorState,
  PlaywrightRunSummary,
  ToastItem,
} from "./types";

export interface PortEntry {
  port: number;
  label: string;
  pid?: number;
}

export interface ProblemItem {
  id: string;
  severity: string;
  source: string;
  message: string;
  filePath?: string | null;
  line?: number | null;
  column?: number | null;
  createdAt: string;
}

export interface ChatSessionUsage {
  totalTokens: number;
  totalCostUsd: number;
  ts: number;
}

export interface ChatMessageDelta {
  messageId: string;
  role: string;
  totalTokens?: number;
  totalCostUsd?: number;
  ts: number;
}

export interface MutationRecord {
  method: string;
  path: string;
  statusCode: number;
  sessionId?: string;
  summary?: string;
  actorUserId?: string;
  ts: number;
}

export interface EventEnrichment {
  uiHint?: import("./types").UiHint;
  semanticTags: string[];
  severityInferred?: string;
  panelTarget?: string;
  ts: number;
}

/** Snapshot iniziale ricevuto dall'endpoint /snapshot. Campi opzionali per
 *  tollerare versioni server diverse. */
export interface ProjectSnapshot {
  seq?: number;
  playwright?: { runs?: PlaywrightRunSummary[] };
  flags?: Record<string, unknown>;
  monitors?: Record<string, MonitorState>;
  fetch_topics?: string[];
  [k: string]: unknown;
}

export interface ProjectStoreState {
  projectId: string | null;
  connectionStatus: ConnectionStatus;
  lastSeq: number;
  lastEventTs: number | null;
  reconnectAttempts: number;

  // Slices per topic
  playwright: { runs: PlaywrightRunSummary[]; configChangedAt: number };
  ports: { entries: PortEntry[] };
  problems: { items: ProblemItem[]; badge: number };
  services: { byName: Record<string, { name: string; port?: number; pid?: number; status: "running" | "stopped" }> };
  files: { recentChanged: Array<{ path: string; op: string; ts: number }> };
  git: { branch: string; ahead: number; behind: number; modified: number };
  database: { recentQueries: Array<{ duration_ms: number; rows: number; kind: string; ts: number }>; configUpdatedAt: number };
  flags: Record<string, unknown>;
  monitors: Record<string, MonitorState>;
  chat: {
    lastCompactBySession: Record<string, ChatSessionUsage>;
    lastMessageBySession: Record<string, ChatMessageDelta>;
    statusBySession: Record<string, string>;
  };
  mutations: { recent: MutationRecord[] };
  enrichments: { byEventId: Record<string, EventEnrichment> };
  // Project lifecycle: timestamp dell'ultimo evento per trigger refresh lista progetti
  projectLifecycle: { lastChangeAt: number };
  // Database migrations: timestamp dell'ultimo evento per trigger refresh lista migrazioni
  migrations: { lastChangeAt: number };
  // Run configurations: timestamp dell'ultimo evento per trigger refresh lista config
  runConfigs: { lastChangeAt: number };
  // Memory: timestamp dell'ultimo aggiornamento per trigger refresh pannello memoria
  memory: { lastChangeAt: number };
  // Knowledge: timestamp dell'ultimo evento per trigger refresh pannello knowledge
  knowledge: { lastChangeAt: number };
  // Provider health: ultimo stato per provider
  providerHealth: { lastChangeAt: number };
  // Plugin: timestamp dell'ultimo evento per trigger refresh
  plugins: { lastChangeAt: number };
  // Settings: timestamp dell'ultimo evento per trigger refresh
  settings: { lastChangeAt: number };
  // Subagent runs: ultimo stato
  subagentRuns: { lastChangeAt: number };
  // Quality scan progress
  qualityScan: { scanId?: string; phase: string; percent: number; ts: number } | null;
  // Output channels
  outputChannels: { lastChangeAt: number };
  // Findings updates: ultimo evento FindingsUpdated ricevuto (con resolved_ids).
  // I componenti come optimization-panel ascoltano per applicare delta in-place
  // senza ri-scansionare.
  findingsUpdate: {
    scanId?: string;
    total: number;
    critical: number;
    warnings: number;
    resolvedIds: string[];
    ts: number;
  } | null;
  /** Timestamp ultimo evento che invalida il pannello Problemi (job, servizi, ...). */
  problemsRefreshAt: number;
  /** Refetch completo viste operative (problemi, output, porte, playwright, run-configs). */
  operationalRefreshAt: number;
  /** Refetch stato servizi progetto (Run panel). */
  servicesRefreshAt: number;
  /** Invalidazione sidebar Modifiche / cache mutations.recent. */
  mutationsRefreshAt: number;
  /** Ultime righe log servizio via SSE (Debug panel). */
  serviceLogs: { recent: Array<{ unit: string; level: string; line: string; ts: number }> };

  // UI ephemera
  toasts: ToastItem[];
  panelHighlights: Record<string, number>; // panel -> expiresAt ts

  // Actions
  setConnectionStatus: (s: ConnectionStatus) => void;
  setProject: (projectId: string | null) => void;
  applySnapshot: (snapshot: ProjectSnapshot) => void;
  applyEvent: (env: EnvelopedEvent) => void;
  dismissToast: (id: string) => void;
  /// Mostra un toast in modo programmatico (feedback di azioni utente, es.
  /// esito compattazione), oltre ai toast generati da applyEvent via ui_hint.
  pushToast: (severity: ToastItem["severity"], message: string) => void;
  /// Aggiorna i totali post-compact di una sessione dalla risposta HTTP, senza
  /// dipendere dall'evento SSE ChatSessionCompacted (che puo' perdersi). Stessa
  /// forma applicata dall'handler SSE in applyEvent.
  recordChatCompaction: (sessionId: string, totalTokens: number, totalCostUsd: number) => void;
  bumpReconnect: () => void;
  resetReconnect: () => void;
  bumpOperationalRefresh: (ts?: number) => void;
  clearPanelPorts: () => void;
  clearPanelPlaywright: () => void;
  /**
   * Sottoscrive un listener che riceve OGNI evento applicato (post-dedup).
   * Restituisce una funzione per disiscriversi. Usato da `useProjectEvents`
   * e `useEventOfKind` per consentire a qualsiasi componente di reagire ad
   * eventi specifici senza dover passare per i selettori tipizzati.
   */
  subscribeAll: (listener: (env: EnvelopedEvent) => void) => () => void;
}

// Listener set out-of-store (Zustand `set` non funziona bene su Set).
// Modulo-level: condiviso tra tutte le istanze (in pratica una sola — Next App Router).
const eventListeners = new Set<(env: EnvelopedEvent) => void>();

const TOAST_DEFAULT_TTL = 5000;

export const useProjectStore = create<ProjectStoreState>((set, get) => ({
  projectId: null,
  connectionStatus: "idle",
  lastSeq: 0,
  lastEventTs: null,
  reconnectAttempts: 0,

  playwright: { runs: [], configChangedAt: 0 },
  ports: { entries: [] },
  problems: { items: [], badge: 0 },
  services: { byName: {} },
  files: { recentChanged: [] },
  git: { branch: "", ahead: 0, behind: 0, modified: 0 },
  database: { recentQueries: [], configUpdatedAt: 0 },
  flags: {},
  monitors: {},
  chat: { lastCompactBySession: {}, lastMessageBySession: {}, statusBySession: {} },
  mutations: { recent: [] },
  enrichments: { byEventId: {} },
  projectLifecycle: { lastChangeAt: 0 },
  migrations: { lastChangeAt: 0 },
  runConfigs: { lastChangeAt: 0 },
  memory: { lastChangeAt: 0 },
  knowledge: { lastChangeAt: 0 },
  providerHealth: { lastChangeAt: 0 },
  plugins: { lastChangeAt: 0 },
  settings: { lastChangeAt: 0 },
  subagentRuns: { lastChangeAt: 0 },
  qualityScan: null,
  outputChannels: { lastChangeAt: 0 },
  findingsUpdate: null,
  problemsRefreshAt: 0,
  operationalRefreshAt: 0,
  servicesRefreshAt: 0,
  mutationsRefreshAt: 0,
  serviceLogs: { recent: [] },

  toasts: [],
  panelHighlights: {},

  setConnectionStatus: (s) => set({ connectionStatus: s }),

  setProject: (projectId) => set({
    projectId,
    lastSeq: 0,
    lastEventTs: null,
    reconnectAttempts: 0,
    connectionStatus: projectId ? "connecting" : "idle",
    // Reset stato per evitare contaminazione tra progetti diversi
    playwright: { runs: [], configChangedAt: 0 },
    ports: { entries: [] },
    problems: { items: [], badge: 0 },
    services: { byName: {} },
    files: { recentChanged: [] },
    git: { branch: "", ahead: 0, behind: 0, modified: 0 },
    database: { recentQueries: [], configUpdatedAt: 0 },
    flags: {},
    monitors: {},
    chat: { lastCompactBySession: {}, lastMessageBySession: {}, statusBySession: {} },
    mutations: { recent: [] },
    enrichments: { byEventId: {} },
    findingsUpdate: null,
    problemsRefreshAt: 0,
    operationalRefreshAt: 0,
    servicesRefreshAt: 0,
    mutationsRefreshAt: 0,
    serviceLogs: { recent: [] },
    toasts: [],
    panelHighlights: {},
  }),

  subscribeAll: (listener) => {
    eventListeners.add(listener);
    return () => { eventListeners.delete(listener); };
  },

  applySnapshot: (snapshot: ProjectSnapshot) => set((state) => {
    const next: Partial<ProjectStoreState> = { lastSeq: snapshot.seq ?? 0 };
    if (snapshot.playwright?.runs) {
      next.playwright = { runs: snapshot.playwright.runs, configChangedAt: 0 };
    }
    if (snapshot.flags) {
      next.flags = snapshot.flags;
    }
    if (snapshot.monitors) {
      next.monitors = snapshot.monitors;
    }
    const fetchTopics = snapshot.fetch_topics;
    if (Array.isArray(fetchTopics) && fetchTopics.length > 0) {
      next.operationalRefreshAt = Date.now();
    }
    return { ...state, ...next };
  }),

  applyEvent: (env) => set((state) => {
    if (env.seq <= state.lastSeq && env.seq !== 0) return state; // dedup

    const next: ProjectStoreState = {
      ...state,
      lastSeq: Math.max(state.lastSeq, env.seq),
      lastEventTs: env.ts,
    };

    const bumpProblemsRefresh = () => {
      next.problemsRefreshAt = env.ts;
      next.operationalRefreshAt = env.ts;
    };

    const bumpServicesRefresh = () => {
      next.servicesRefreshAt = env.ts;
      next.operationalRefreshAt = env.ts;
    };

    // Apply payload
    const p = env.payload;
    switch (p.kind) {
      case "JobCreated": {
        bumpProblemsRefresh();
        if (p.job_kind === "playwright_test") {
          const newRun: PlaywrightRunSummary = {
            id: p.id,
            label: p.label,
            status: p.status,
            summary: p.summary,
            artifacts: Array.isArray(p.artifacts) ? (p.artifacts as PlaywrightArtifact[]) : [],
            createdAt: new Date(env.ts).toISOString(),
          };
          const without = next.playwright.runs.filter((r) => r.id !== p.id);
          next.playwright = { ...next.playwright, runs: [newRun, ...without].slice(0, 50) };
        }
        break;
      }
      case "JobUpdated": {
        bumpProblemsRefresh();
        next.playwright = {
          ...next.playwright,
          runs: next.playwright.runs.map((r) =>
            r.id === p.id
              ? { ...r, status: p.status, label: p.label ?? r.label, summary: p.summary ?? r.summary }
              : r,
          ),
        };
        break;
      }
      case "JobsCleared": {
        bumpProblemsRefresh();
        if (p.job_kind === "playwright_test") {
          next.playwright = { ...next.playwright, runs: [] };
        }
        break;
      }
      case "PortAllocated": {
        const without = next.ports.entries.filter((e) => e.port !== p.port);
        next.ports = { entries: [...without, { port: p.port, label: p.label, pid: p.pid }] };
        next.operationalRefreshAt = env.ts;
        break;
      }
      case "PortReleased": {
        next.ports = { entries: next.ports.entries.filter((e) => e.port !== p.port) };
        next.operationalRefreshAt = env.ts;
        break;
      }
      case "FindingsUpdated": {
        bumpProblemsRefresh();
        next.problems = { ...next.problems, badge: p.total };
        // Esponi resolved_ids in slot dedicato: optimization-panel ascolta
        // questo per marcare i findings in-place senza scan extra.
        next.findingsUpdate = {
          scanId: p.scan_id,
          total: p.total,
          critical: p.critical,
          warnings: p.warnings,
          resolvedIds: p.resolved_ids ?? [],
          ts: env.ts,
        };
        break;
      }
      case "ServiceStarted": {
        bumpServicesRefresh();
        next.services = {
          byName: {
            ...next.services.byName,
            [p.name]: { name: p.name, port: p.port, pid: p.pid, status: "running" },
          },
        };
        break;
      }
      case "ServiceStopped": {
        bumpServicesRefresh();
        const existingStopped = next.services.byName[p.name];
        next.services = {
          byName: {
            ...next.services.byName,
            [p.name]: existingStopped ? { ...existingStopped, status: "stopped" } : { name: p.name, status: "stopped" },
          },
        };
        break;
      }
      case "ServiceRestarted": {
        bumpServicesRefresh();
        const existingRestart = next.services.byName[p.name];
        next.services = {
          byName: {
            ...next.services.byName,
            [p.name]: existingRestart ? { ...existingRestart, status: "running" } : { name: p.name, status: "running" },
          },
        };
        break;
      }
      case "ServiceStatusChanged": {
        bumpServicesRefresh();
        const existingStatus = next.services.byName[p.name];
        const mappedStatus = p.status === "active" || p.status === "running" ? "running" : "stopped";
        next.services = {
          byName: {
            ...next.services.byName,
            [p.name]: {
              name: p.name,
              port: p.port ?? existingStatus?.port,
              pid: p.pid ?? existingStatus?.pid,
              status: mappedStatus,
            },
          },
        };
        break;
      }
      case "ServiceLogLine": {
        bumpServicesRefresh();
        next.serviceLogs = {
          recent: [
            { unit: p.unit, level: p.level, line: p.line, ts: env.ts },
            ...next.serviceLogs.recent,
          ].slice(0, 200),
        };
        break;
      }
      case "ServiceMetrics": {
        bumpServicesRefresh();
        const monitorId = `service:${p.unit}`;
        next.monitors = {
          ...next.monitors,
          [monitorId]: {
            value: {
              cpu_pct: p.cpu_pct,
              rss_bytes: p.rss_bytes,
              io_read_bytes: p.io_read_bytes,
              io_write_bytes: p.io_write_bytes,
              latency_ms: p.latency_ms,
              pid: p.pid,
            },
            label: p.unit,
            updated_at: new Date(env.ts).toISOString(),
          },
        };
        break;
      }
      case "TodoUpdated":
      case "PlanUpdated":
        break;
      case "FileChanged": {
        next.files = {
          recentChanged: [
            { path: p.path, op: p.op, ts: env.ts },
            ...next.files.recentChanged.filter((f) => f.path !== p.path),
          ].slice(0, 50),
        };
        next.mutationsRefreshAt = env.ts;
        next.operationalRefreshAt = env.ts;
        // Rileva creazione/modifica playwright.config.* per aggiornare pannello Playwright
        if (/playwright\.config\.(ts|js|mjs)$/.test(p.path)) {
          next.playwright = { ...next.playwright, configChangedAt: env.ts };
        }
        break;
      }
      case "GitStatusChanged": {
        next.git = { branch: p.branch, ahead: p.ahead, behind: p.behind, modified: p.modified_count };
        break;
      }
      case "DbQueryRun": {
        next.database = {
          ...next.database,
          recentQueries: [
            { duration_ms: p.duration_ms, rows: p.rows, kind: p.statement_kind, ts: env.ts },
            ...next.database.recentQueries,
          ].slice(0, 100),
        };
        break;
      }
      case "DbConfigUpdated": {
        next.database = { ...next.database, configUpdatedAt: env.ts };
        break;
      }
      case "FlagChanged": {
        next.flags = { ...next.flags, [p.key]: p.value };
        break;
      }
      case "MonitorUpdated": {
        next.monitors = {
          ...next.monitors,
          [p.monitor_id]: {
            value: p.value,
            label: p.label,
            updated_at: new Date(env.ts).toISOString(),
          },
        };
        break;
      }
      case "ChatSessionCompacted": {
        next.chat = {
          ...next.chat,
          lastCompactBySession: {
            ...next.chat.lastCompactBySession,
            [p.session_id]: {
              totalTokens: p.total_tokens,
              totalCostUsd: p.total_cost_usd,
              ts: env.ts,
            },
          },
          statusBySession: { ...next.chat.statusBySession, [p.session_id]: "compacted" },
        };
        break;
      }
      case "ChatMessageAdded": {
        next.chat = {
          ...next.chat,
          lastMessageBySession: {
            ...next.chat.lastMessageBySession,
            [p.session_id]: {
              messageId: p.message_id,
              role: p.role,
              totalTokens: p.total_tokens,
              totalCostUsd: p.total_cost_usd,
              ts: env.ts,
            },
          },
        };
        break;
      }
      case "ChatSessionStatusChanged": {
        next.chat = {
          ...next.chat,
          statusBySession: { ...next.chat.statusBySession, [p.session_id]: p.status },
        };
        break;
      }
      case "MutationRecorded": {
        next.mutationsRefreshAt = env.ts;
        next.mutations = {
          recent: [
            {
              method: p.method,
              path: p.path,
              statusCode: p.status_code,
              sessionId: p.session_id,
              summary: p.summary,
              actorUserId: p.actor_user_id,
              ts: env.ts,
            },
            ...next.mutations.recent,
          ].slice(0, 200),
        };
        break;
      }
      case "EventEnriched": {
        next.enrichments = {
          byEventId: {
            ...next.enrichments.byEventId,
            [p.event_id]: {
              uiHint: p.ui_hint,
              semanticTags: p.semantic_tags ?? [],
              severityInferred: p.severity_inferred,
              panelTarget: p.panel_target,
              ts: env.ts,
            },
          },
        };
        // Cap LRU a 500 per evitare crescita illimitata in sessioni lunghe
        const entries = Object.entries(next.enrichments.byEventId);
        if (entries.length > 500) {
          const sorted = entries.sort((a, b) => b[1].ts - a[1].ts).slice(0, 500);
          next.enrichments = { byEventId: Object.fromEntries(sorted) };
        }
        // Se l'enrichment include ui_hint, applicalo come se fosse arrivato
        // con l'evento originale (toast + highlight). Idempotente: il toast
        // ha id = event_id, dedupliato dallo store toast manager.
        if (p.ui_hint) {
          // Riusa la stessa logica del blocco "Apply UI hint" sotto:
          // facciamo merge inline per evitare ricorsione
          const h = p.ui_hint;
          if (h.toast_msg && h.toast_severity) {
            const toastId = `enriched_${p.event_id}`;
            if (!next.toasts.find((t) => t.id === toastId)) {
              const toast: ToastItem = {
                id: toastId,
                severity: h.toast_severity,
                message: h.toast_msg,
                ttl_ms: TOAST_DEFAULT_TTL,
                panel: h.highlight_panel,
                createdAt: env.ts,
              };
              next.toasts = [...next.toasts, toast].slice(-20);
              if (typeof window !== "undefined") {
                window.setTimeout(() => get().dismissToast(toast.id), TOAST_DEFAULT_TTL);
              }
            }
          }
          if (h.highlight_panel && h.flash_duration_ms) {
            next.panelHighlights = {
              ...next.panelHighlights,
              [h.highlight_panel]: Date.now() + h.flash_duration_ms,
            };
          }
        }
        break;
      }
      // ── Project lifecycle ──────────────────────────────────────────
      case "ProjectCreated":
      case "ProjectDeleted": {
        next.projectLifecycle = { lastChangeAt: env.ts };
        break;
      }
      // ── Database migrations ─────────────────────────────────────────
      case "MigrationApplied":
      case "MigrationRolledBack": {
        next.migrations = { lastChangeAt: env.ts };
        // Aggiorna anche il timestamp DB config per triggerare refresh pannello DB
        next.database = { ...next.database, configUpdatedAt: env.ts };
        break;
      }
      // ── Run configurations ──────────────────────────────────────────
      case "RunConfigChanged": {
        next.runConfigs = { lastChangeAt: env.ts };
        next.operationalRefreshAt = env.ts;
        break;
      }
      // ── Memory ──────────────────────────────────────────────────────
      case "MemoryUpdated": {
        next.memory = { lastChangeAt: env.ts };
        break;
      }
      // ── Provider health ───────────────────────────────────────────
      case "ProviderHealthChanged": {
        next.providerHealth = { lastChangeAt: env.ts };
        break;
      }
      // ── Plugin lifecycle ──────────────────────────────────────────
      case "PluginChanged": {
        next.plugins = { lastChangeAt: env.ts };
        break;
      }
      // ── Settings ──────────────────────────────────────────────────
      case "SettingChanged": {
        next.settings = { lastChangeAt: env.ts };
        break;
      }
      // ── Subagent runs ─────────────────────────────────────────────
      case "SubagentRunChanged": {
        next.subagentRuns = { lastChangeAt: env.ts };
        break;
      }
      // ── Quality scan progress ─────────────────────────────────────
      case "QualityScanProgress": {
        next.qualityScan = {
          scanId: p.scan_id,
          phase: p.phase,
          percent: p.percent ?? 0,
          ts: env.ts,
        };
        break;
      }
      // ── Output channels ───────────────────────────────────────────
      case "OutputChannelCreated": {
        next.outputChannels = { lastChangeAt: env.ts };
        next.operationalRefreshAt = env.ts;
        break;
      }
      // ── Knowledge ─────────────────────────────────────────────────
      case "KnowledgeNoteCreated":
      case "KnowledgeNoteUpdated":
      case "KnowledgeLinkCreated": {
        next.knowledge = { lastChangeAt: env.ts };
        break;
      }
      // DocumentGenerated: il refresh del pannello DOCUMENTI e' gestito in
      // connection.ts via window event "nexus:documents:refresh" (il pannello
      // ricarica dalla REST); qui nessuna mutazione di stato dispatcher.
      case "ServiceCrashDetected":
      case "ServiceBuildErrors":
      case "ServiceDiagnosisStarted":
      case "ServiceAnomaly": {
        bumpProblemsRefresh();
        bumpServicesRefresh();
        break;
      }
      case "DocumentGenerated":
      case "Notification":
      case "HighlightPanel":
      case "AgentToolUsed":
      case "Custom":
      case "SnapshotRequired":
        break;
    }

    // Apply UI hint (toast + highlight)
    const hint = env.ui_hint;
    if (hint) {
      if (hint.toast_msg && hint.toast_severity) {
        const toast: ToastItem = {
          id: env.event_id,
          severity: hint.toast_severity,
          message: hint.toast_msg,
          ttl_ms: TOAST_DEFAULT_TTL,
          panel: hint.highlight_panel,
          createdAt: env.ts,
        };
        next.toasts = [...next.toasts, toast].slice(-20);
        // Auto-dismiss
        if (typeof window !== "undefined") {
          window.setTimeout(() => get().dismissToast(toast.id), TOAST_DEFAULT_TTL);
        }
      }
      if (hint.highlight_panel && hint.flash_duration_ms) {
        next.panelHighlights = {
          ...next.panelHighlights,
          [hint.highlight_panel]: Date.now() + hint.flash_duration_ms,
        };
      }
      // badge_increment per "problems" RIMOSSO (regola H): sommava +inc per ogni
      // evento problema senza riconciliarsi col DB -> contatore gonfiato (es. 1128
      // vs ~30 reali) durante sessioni con molti run. Il conteggio problemi e' ora
      // derivato dalla lista reale (get_project_problems) lato UI; il badge da
      // FindingsUpdated (p.total) resta per il pannello Ottimizzazione (quality).
    }

    // Notifica tutti i listener `subscribeAll` (hook universali useProjectEvents).
    // Fatto qui AL FONDO dopo che lo state e' aggiornato, ma usando una
    // microtask per non bloccare il set di Zustand (i listener possono fare
    // setState a loro volta).
    if (eventListeners.size > 0) {
      Promise.resolve().then(() => {
        for (const listener of eventListeners) {
          try {
            listener(env);
          } catch (e) {
            console.error("[dispatcher] listener error:", e);
          }
        }
      });
    }

    return next;
  }),

  dismissToast: (id) => set((state) => ({
    toasts: state.toasts.filter((t) => t.id !== id),
  })),

  pushToast: (severity, message) => set((state) => {
    const id = (typeof crypto !== "undefined" && "randomUUID" in crypto)
      ? crypto.randomUUID()
      : `toast-${Date.now()}-${state.toasts.length}`;
    const toast: ToastItem = {
      id,
      severity,
      message,
      ttl_ms: TOAST_DEFAULT_TTL,
      createdAt: Date.now(),
    };
    if (typeof window !== "undefined") {
      window.setTimeout(() => get().dismissToast(id), TOAST_DEFAULT_TTL);
    }
    return { toasts: [...state.toasts, toast].slice(-20) };
  }),

  recordChatCompaction: (sessionId, totalTokens, totalCostUsd) => set((state) => ({
    chat: {
      ...state.chat,
      lastCompactBySession: {
        ...state.chat.lastCompactBySession,
        [sessionId]: { totalTokens, totalCostUsd, ts: Date.now() },
      },
      statusBySession: { ...state.chat.statusBySession, [sessionId]: "compacted" },
    },
  })),

  bumpReconnect: () => set((state) => ({ reconnectAttempts: state.reconnectAttempts + 1 })),
  resetReconnect: () => set({ reconnectAttempts: 0 }),

  bumpOperationalRefresh: (ts) => set({
    operationalRefreshAt: ts ?? Date.now(),
    problemsRefreshAt: ts ?? Date.now(),
  }),

  clearPanelPorts: () => set({ ports: { entries: [] } }),

  clearPanelPlaywright: () => set({
    playwright: { runs: [], configChangedAt: Date.now() },
  }),
}));

// Selectors esportati per ergonomia
export const selectConnection = (s: ProjectStoreState) => s.connectionStatus;
export const selectPlaywrightRuns = (s: ProjectStoreState) => s.playwright.runs;
export const selectPlaywrightConfigChangedAt = (s: ProjectStoreState) => s.playwright.configChangedAt;
export const selectPorts = (s: ProjectStoreState) => s.ports.entries;
export const selectProblemsBadge = (s: ProjectStoreState) => s.problems.badge;
export const selectServicesMap = (s: ProjectStoreState) => s.services.byName;
export const selectFilesRecent = (s: ProjectStoreState) => s.files.recentChanged;
export const selectGitStatus = (s: ProjectStoreState) => s.git;
export const selectDatabaseQueries = (s: ProjectStoreState) => s.database.recentQueries;
export const selectDbConfigUpdatedAt = (s: ProjectStoreState) => s.database.configUpdatedAt;
export const selectToasts = (s: ProjectStoreState) => s.toasts;
export const selectFlags = (s: ProjectStoreState) => s.flags;
export const selectMonitors = (s: ProjectStoreState) => s.monitors;
export const selectHighlight = (panel: string) => (s: ProjectStoreState) => {
  const exp = s.panelHighlights[panel];
  return exp && exp > Date.now() ? exp : null;
};

// ── Chat / mutation / enrichment / findings ─────────────────────────────────
export const selectChatLastCompact = (sessionId: string | null) => (s: ProjectStoreState) =>
  sessionId ? s.chat.lastCompactBySession[sessionId] ?? null : null;
export const selectChatLastMessage = (sessionId: string | null) => (s: ProjectStoreState) =>
  sessionId ? s.chat.lastMessageBySession[sessionId] ?? null : null;
export const selectChatStatus = (sessionId: string | null) => (s: ProjectStoreState) =>
  sessionId ? s.chat.statusBySession[sessionId] ?? null : null;
export const selectMutationsRecent = (s: ProjectStoreState) => s.mutations.recent;
export const selectEnrichmentByEventId = (eventId: string) => (s: ProjectStoreState) =>
  s.enrichments.byEventId[eventId] ?? null;
export const selectFindingsUpdate = (s: ProjectStoreState) => s.findingsUpdate;
export const selectProblemsRefreshAt = (s: ProjectStoreState) => s.problemsRefreshAt;
export const selectOperationalRefreshAt = (s: ProjectStoreState) => s.operationalRefreshAt;
export const selectServicesRefreshAt = (s: ProjectStoreState) => s.servicesRefreshAt;
export const selectMutationsRefreshAt = (s: ProjectStoreState) => s.mutationsRefreshAt;
export const selectServiceLogsRecent = (s: ProjectStoreState) => s.serviceLogs.recent;
export const selectProjectLifecycleAt = (s: ProjectStoreState) => s.projectLifecycle.lastChangeAt;
export const selectMigrationsChangedAt = (s: ProjectStoreState) => s.migrations.lastChangeAt;
export const selectRunConfigsChangedAt = (s: ProjectStoreState) => s.runConfigs.lastChangeAt;
export const selectMemoryChangedAt = (s: ProjectStoreState) => s.memory.lastChangeAt;
export const selectProviderHealthChangedAt = (s: ProjectStoreState) => s.providerHealth.lastChangeAt;
export const selectPluginsChangedAt = (s: ProjectStoreState) => s.plugins.lastChangeAt;
export const selectSettingsChangedAt = (s: ProjectStoreState) => s.settings.lastChangeAt;
export const selectSubagentRunsChangedAt = (s: ProjectStoreState) => s.subagentRuns.lastChangeAt;
export const selectQualityScan = (s: ProjectStoreState) => s.qualityScan;
export const selectOutputChannelsChangedAt = (s: ProjectStoreState) => s.outputChannels.lastChangeAt;
export const selectKnowledgeChangedAt = (s: ProjectStoreState) => s.knowledge.lastChangeAt;
export const subscribeAll = (
  listener: (env: EnvelopedEvent) => void,
) => useProjectStore.getState().subscribeAll(listener);
