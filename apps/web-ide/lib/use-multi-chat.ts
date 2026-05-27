"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  getChatSessions,
  createChatSession,
  renameChatSession,
  deleteChatSession,
  compactChatSession,
  type ChatSessionSummary,
} from "./api-client";

const TABS_KEY = (pid: string) => `ideai:openTabs:${pid}`;
const ACTIVE_KEY = (pid: string) => `ideai:activeTab:${pid}`;

function loadPersisted(projectId: string): { tabs: string[]; active: string | null } {
  try {
    const raw = typeof window !== "undefined" ? localStorage.getItem(TABS_KEY(projectId)) : null;
    const active = typeof window !== "undefined" ? localStorage.getItem(ACTIVE_KEY(projectId)) : null;
    return { tabs: raw ? (JSON.parse(raw) as string[]) : [], active };
  } catch {
    return { tabs: [], active: null };
  }
}

function savePersisted(projectId: string, tabs: string[], active: string | null) {
  try {
    if (typeof window === "undefined") return;
    localStorage.setItem(TABS_KEY(projectId), JSON.stringify(tabs));
    if (active) localStorage.setItem(ACTIVE_KEY(projectId), active);
    else localStorage.removeItem(ACTIVE_KEY(projectId));
  } catch {
    // ignore
  }
}

export type AgentActivityMap = Map<string, boolean>;
/** Mappa sessionId → ratio (0..1+) del context_window usato nell'ultimo turno.
 *  Aggiornata da ChatPanel via onCtxRatioChange; letta da ide-shell per
 *  mostrare la % sul bottone "Compatta chat" (icona di compattazione).
 *  Valore > 1.0 ammesso (es. 1.34 = 134% ctx) per indicare overflow. */
export type CtxRatioMap = Map<string, number>;

export interface UseMultiChatReturn {
  allSessions: ChatSessionSummary[];
  openTabs: string[];
  activeTabId: string | null;
  agentActivity: AgentActivityMap;
  ctxRatio: CtxRatioMap;
  isLoading: boolean;
  error: string | null;
  openTab: (id: string) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  newSession: () => Promise<void>;
  renameSession: (id: string, title: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  compactSession: (id: string) => Promise<{ summary: string }>;
  setAgentActive: (sessionId: string, active: boolean) => void;
  setCtxRatio: (sessionId: string, ratio: number | null) => void;
  refreshSessions: () => Promise<void>;
}

export function useMultiChat(projectId: string): UseMultiChatReturn {
  const [allSessions, setAllSessions] = useState<ChatSessionSummary[]>([]);
  const [openTabs, setOpenTabs] = useState<string[]>([]);
  const [activeTabId, setActiveTabIdState] = useState<string | null>(null);
  const [agentActivity, setAgentActivity] = useState<AgentActivityMap>(new Map());
  const [ctxRatio, setCtxRatioState] = useState<CtxRatioMap>(new Map());
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const openTabsRef = useRef<string[]>([]);
  openTabsRef.current = openTabs;

  const persist = useCallback(
    (tabs: string[], active: string | null) => savePersisted(projectId, tabs, active),
    [projectId],
  );

  // ── Bootstrap ───────────────────────────────────────────────────────────────
  useEffect(() => {
    if (!projectId || projectId === "default") return;
    let cancelled = false;
    setIsLoading(true);

    (async () => {
      try {
        const data = await getChatSessions(projectId);
        if (cancelled) return;
        let sessions = data.sessions;

        // If no sessions exist, create the first one
        if (sessions.length === 0) {
          const created = await createChatSession(projectId, "Chat 1");
          if (cancelled) return;
          const s = created.session;
          sessions = [{
            id: s.id,
            projectId,
            title: s.title,
            status: s.status,
            messageCount: 0,
            createdAt: new Date().toISOString(),
            updatedAt: new Date().toISOString(),
          }];
        }

        setAllSessions(sessions);
        const sessionIds = new Set(sessions.map((s) => s.id));
        const persisted = loadPersisted(projectId);

        // Reconcile: remove stale tab IDs
        let tabs = persisted.tabs.filter((id) => sessionIds.has(id));
        let active = persisted.active && sessionIds.has(persisted.active)
          ? persisted.active : null;

        // Ensure at least one tab is open
        if (tabs.length === 0) tabs = [sessions[0].id];
        if (!active) active = tabs[tabs.length - 1];

        setOpenTabs(tabs);
        setActiveTabIdState(active);
        persist(tabs, active);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Errore caricamento sessioni");
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();

    return () => { cancelled = true; };
  }, [projectId, persist]);

  // ── Refresh ─────────────────────────────────────────────────────────────────
  const refreshSessions = useCallback(async () => {
    if (!projectId || projectId === "default") return;
    const data = await getChatSessions(projectId);
    setAllSessions(data.sessions);
  }, [projectId]);

  // ── Tab management ──────────────────────────────────────────────────────────
  const openTab = useCallback((id: string) => {
    setOpenTabs((prev) => {
      const next = prev.includes(id) ? prev : [...prev, id];
      persist(next, id);
      return next;
    });
    setActiveTabIdState(id);
  }, [persist]);

  const closeTab = useCallback((id: string) => {
    setOpenTabs((prev) => {
      const next = prev.filter((t) => t !== id);
      setActiveTabIdState((active) => {
        const newActive = active === id ? (next[next.length - 1] ?? null) : active;
        persist(next, newActive);
        return newActive;
      });
      return next;
    });
  }, [persist]);

  const setActiveTab = useCallback((id: string) => {
    setOpenTabs((prev) => {
      const next = prev.includes(id) ? prev : [...prev, id];
      persist(next, id);
      return next;
    });
    setActiveTabIdState(id);
  }, [persist]);

  // ── CRUD ────────────────────────────────────────────────────────────────────
  const newSession = useCallback(async () => {
    const count = allSessions.length + 1;
    const created = await createChatSession(projectId, `Chat ${count}`);
    const s = created.session;
    const summary: ChatSessionSummary = {
      id: s.id,
      projectId,
      title: s.title,
      status: s.status,
      messageCount: 0,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    setAllSessions((prev) => [summary, ...prev]);
    setOpenTabs((prev) => {
      const next = [...prev, s.id];
      persist(next, s.id);
      return next;
    });
    setActiveTabIdState(s.id);
  }, [allSessions.length, projectId, persist]);

  const renameSession = useCallback(async (id: string, title: string) => {
    await renameChatSession(id, title);
    setAllSessions((prev) => prev.map((s) => (s.id === id ? { ...s, title } : s)));
  }, []);

  const deleteSession = useCallback(async (id: string) => {
    await deleteChatSession(id);
    setAllSessions((prev) => prev.filter((s) => s.id !== id));
    closeTab(id);
  }, [closeTab]);

  const compactSession = useCallback(async (id: string) => {
    const result = await compactChatSession(id);
    setAllSessions((prev) =>
      prev.map((s) => (s.id === id ? { ...s, status: "compacted" } : s)),
    );
    return { summary: result.summary };
  }, []);

  // ── Agent activity tracking ──────────────────────────────────────────────────
  const setAgentActive = useCallback((sessionId: string, active: boolean) => {
    setAgentActivity((prev) => {
      const next = new Map(prev);
      if (active) next.set(sessionId, true);
      else next.delete(sessionId);
      return next;
    });
  }, []);

  // ── Ctx ratio tracking (per badge sul bottone Compatta) ──────────────────────
  // Audit 27/05/2026: short-circuit se il valore non e' cambiato per evitare
  // re-render e potenziali loop "Maximum update depth exceeded" se il caller
  // chiama setCtxRatio in un useEffect con dep instabili.
  const setCtxRatio = useCallback((sessionId: string, ratio: number | null) => {
    setCtxRatioState((prev) => {
      const currentRatio = prev.get(sessionId) ?? null;
      // Confronto con tolleranza per evitare update se la differenza e' marginale.
      if (ratio == null && currentRatio == null) return prev;
      if (ratio != null && currentRatio != null && Math.abs(ratio - currentRatio) < 0.001) return prev;
      const next = new Map(prev);
      if (ratio == null) next.delete(sessionId);
      else next.set(sessionId, ratio);
      return next;
    });
  }, []);

  return {
    allSessions,
    openTabs,
    activeTabId,
    agentActivity,
    ctxRatio,
    isLoading,
    error,
    openTab,
    closeTab,
    setActiveTab,
    newSession,
    renameSession,
    deleteSession,
    compactSession,
    setAgentActive,
    setCtxRatio,
    refreshSessions,
  };
}
