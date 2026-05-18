// Connection manager: singolo EventSource per progetto attivo.
// Niente fallback polling — la connessione e' la SOLA fonte di verita'.
// Se SSE down, il ConnectionStatusBadge mostra lo stato; nessun fetch silenzioso
// con dati potenzialmente stantii.

import { useProjectStore } from "./store";
import type { EnvelopedEvent } from "./types";

const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 8000;
const RECONNECT_GIVEUP_AFTER = 6; // ~ 30s prima di marcare "disconnected"

let currentSource: EventSource | null = null;
let currentProjectId: string | null = null;
let reconnectTimer: number | null = null;
let snapshotInflight: AbortController | null = null;

async function fetchSnapshot(projectId: string): Promise<void> {
  // Abort precedente snapshot ancora in volo (cambio progetto rapido)
  if (snapshotInflight) snapshotInflight.abort();
  const ctrl = new AbortController();
  snapshotInflight = ctrl;

  try {
    const res = await fetch(`/api/projects/${projectId}/snapshot?topics=*`, {
      credentials: "include",
      signal: ctrl.signal,
    });
    if (!res.ok) {
      console.warn("[dispatcher] snapshot non-200:", res.status);
      return;
    }
    const snapshot = await res.json();
    if (currentProjectId === projectId) {
      useProjectStore.getState().applySnapshot(snapshot);
    }
  } catch (err: any) {
    if (err?.name !== "AbortError") {
      console.warn("[dispatcher] snapshot fallita:", err);
    }
  }
}

function openStream(projectId: string): void {
  const lastSeq = useProjectStore.getState().lastSeq;
  const sinceQ = lastSeq > 0 ? `&since=${lastSeq}` : "";
  const url = `/api/projects/${projectId}/event-stream?topics=*${sinceQ}`;

  const es = new EventSource(url, { withCredentials: true });
  currentSource = es;

  useProjectStore.getState().setConnectionStatus("connecting");

  // I tipi evento corrispondono a `ProjectEvent::kind_name()` lato Rust.
  // Ascolto "message" come fallback per browser che non triggerano named events.
  const handleEvent = (raw: MessageEvent) => {
    try {
      const env: EnvelopedEvent = JSON.parse(raw.data);
      useProjectStore.getState().applyEvent(env);

      // Se l'evento e' SnapshotRequired -> ricarica snapshot
      if (env.payload.kind === "SnapshotRequired") {
        console.info("[dispatcher] SnapshotRequired:", env.payload.reason);
        void fetchSnapshot(projectId);
      }
    } catch (err) {
      console.warn("[dispatcher] event parse error", err, raw.data);
    }
  };

  const KINDS = [
    "JobCreated", "JobUpdated", "JobsCleared",
    "PortAllocated", "PortReleased",
    "FindingsUpdated",
    "ServiceStarted", "ServiceStopped", "ServiceRestarted",
    "FileChanged",
    "GitStatusChanged",
    "DbQueryRun",
    "DbConfigUpdated",
    "AgentToolUsed",
    "Notification",
    "FlagChanged",
    "MonitorUpdated",
    "HighlightPanel",
    "Custom",
    "SnapshotRequired",
    // ── Chat session lifecycle (handler nello store, prima non sottoscritti) ──
    "ChatSessionCompacted",
    "ChatMessageAdded",
    "ChatSessionStatusChanged",
    // ── Catch-all HTTP mutations + meta enrichment ─────────────────────────
    "MutationRecorded",
    "EventEnriched",
  ];
  KINDS.forEach((k) => es.addEventListener(k, handleEvent));
  es.addEventListener("message", handleEvent);

  es.onopen = () => {
    useProjectStore.getState().setConnectionStatus("open");
    useProjectStore.getState().resetReconnect();
  };

  es.onerror = () => {
    es.close();
    if (currentSource === es) currentSource = null;
    scheduleReconnect(projectId);
  };
}

function scheduleReconnect(projectId: string): void {
  if (currentProjectId !== projectId) return;

  const attempts = useProjectStore.getState().reconnectAttempts;
  useProjectStore.getState().bumpReconnect();

  if (attempts >= RECONNECT_GIVEUP_AFTER) {
    useProjectStore.getState().setConnectionStatus("disconnected");
  } else {
    useProjectStore.getState().setConnectionStatus("reconnecting");
  }

  const delay = Math.min(RECONNECT_BASE_MS * Math.pow(2, attempts), RECONNECT_MAX_MS);
  if (reconnectTimer) window.clearTimeout(reconnectTimer);
  reconnectTimer = window.setTimeout(() => {
    if (currentProjectId === projectId) openStream(projectId);
  }, delay);
}

/**
 * Connetti (o riconnetti su nuovo progetto) il dispatcher.
 * Chiamare in `useEffect` su mount del progetto attivo.
 */
export async function connectDispatcher(projectId: string): Promise<void> {
  if (currentProjectId === projectId && currentSource) {
    return; // gia' connesso
  }
  disconnectDispatcher();

  currentProjectId = projectId;
  useProjectStore.getState().setProject(projectId);

  // 1) Bootstrap snapshot REST (stato corrente)
  await fetchSnapshot(projectId);
  // 2) Apri SSE per delta (filtra eventi <= lastSeq via dedup nel store)
  openStream(projectId);
}

export function disconnectDispatcher(): void {
  if (reconnectTimer) {
    window.clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (currentSource) {
    currentSource.close();
    currentSource = null;
  }
  if (snapshotInflight) {
    snapshotInflight.abort();
    snapshotInflight = null;
  }
  currentProjectId = null;
}

/** Forza un re-fetch dello snapshot (es. su pulsante "Riconnetti"). */
export function refreshDispatcher(): void {
  if (!currentProjectId) return;
  const pid = currentProjectId;
  void fetchSnapshot(pid).then(() => {
    // Reset reconnect e riapri stream
    if (currentSource) {
      currentSource.close();
      currentSource = null;
    }
    useProjectStore.getState().resetReconnect();
    openStream(pid);
  });
}
