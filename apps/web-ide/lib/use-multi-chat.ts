"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  getChatSessions,
  createChatSession,
  renameChatSession,
  deleteChatSession,
  compactChatSession,
  updateChatSessionPrefs,
  type ChatSessionSummary,
} from "./api-client";
import { useProjectStore } from "./project-dispatcher/store";
import { reconcileSessionTabs } from "./use-multi-chat/reconcile";

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
  /** Persiste il pin provider/modello della sessione ("auto" = azzera).
   *  Ottimistico su allSessions + PATCH server; toast su errore. */
  setSessionPrefs: (
    id: string,
    prefs: { preferredProvider?: string; preferredModel?: string },
  ) => void;
  deleteSession: (id: string) => Promise<void>;
  compactSession: (id: string) => Promise<{ summary: string }>;
  setAgentActive: (sessionId: string, active: boolean) => void;
  setCtxRatio: (sessionId: string, ratio: number | null) => void;
  refreshSessions: () => Promise<void>;
  /** Rilancia il bootstrap del progetto corrente (per il bottone Riprova
   *  quando il caricamento sessioni e' fallito). */
  retryBootstrap: () => void;
}

export function useMultiChat(projectId: string): UseMultiChatReturn {
  const [allSessions, setAllSessions] = useState<ChatSessionSummary[]>([]);
  const [openTabs, setOpenTabs] = useState<string[]>([]);
  const [activeTabId, setActiveTabIdState] = useState<string | null>(null);
  const [agentActivity, setAgentActivity] = useState<AgentActivityMap>(new Map());
  const [ctxRatio, setCtxRatioState] = useState<CtxRatioMap>(new Map());
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [bootstrapNonce, setBootstrapNonce] = useState(0);
  const openTabsRef = useRef<string[]>([]);
  openTabsRef.current = openTabs;

  // Reset SINCRONO al cambio progetto (pattern React "adjust state when a prop
  // changes"): senza questo, sessioni e tab del progetto precedente restavano
  // in vista finche' il bootstrap del nuovo non completava — e PER SEMPRE se
  // falliva (es. progetto appena creato col DB per-progetto in provisioning).
  // ChatPanel arrivava cosi' a una coppia incoerente (progetto nuovo, sessione
  // del vecchio) col composer muto: isReady=false in useChat -> send senza POST.
  const [bootProjectId, setBootProjectId] = useState(projectId);
  if (bootProjectId !== projectId) {
    setBootProjectId(projectId);
    setAllSessions([]);
    setOpenTabs([]);
    setActiveTabIdState(null);
    setAgentActivity(new Map());
    setCtxRatioState(new Map());
    setError(null);
    setIsLoading(Boolean(projectId && projectId !== "default"));
  }

  const persist = useCallback(
    (tabs: string[], active: string | null) => savePersisted(projectId, tabs, active),
    [projectId],
  );

  // ── Bootstrap ───────────────────────────────────────────────────────────────
  useEffect(() => {
    // Reset PRIMA del fetch: al cambio progetto il pannello non deve mai
    // mostrare (ne' rendere operabili) le chat del progetto precedente. Senza
    // questo reset, un fetch fallito sul progetto nuovo (es. DB in provisioning)
    // lasciava visibile la chat del progetto vecchio: un messaggio digitato li'
    // finiva su una sessione di un ALTRO progetto (incidente vendita-immobile
    // 20/07). Vale anche per il ramo "default": stato vuoto, non stantio.
    setAllSessions([]);
    setOpenTabs([]);
    setActiveTabIdState(null);
    setAgentActivity(new Map());
    setCtxRatioState(new Map());
    setError(null);
    if (!projectId || projectId === "default") return;
    let cancelled = false;
    setIsLoading(true);

    (async () => {
      try {
        // GET con breve retry: un progetto appena creato puo' avere il DB
        // per-progetto ancora in provisioning e la prima fetch puo' fallire.
        // Con un tentativo unico il pannello restava congelato (nessun retry,
        // errore mai renderizzato) fino al reload manuale della pagina.
        let sessions: ChatSessionSummary[] | null = null;
        let lastError: unknown = null;
        for (let attempt = 0; attempt < 3 && !cancelled; attempt += 1) {
          if (attempt > 0) {
            await new Promise((resolve) => setTimeout(resolve, 1000 * attempt));
            if (cancelled) break;
          }
          try {
            const data = await getChatSessions(projectId);
            sessions = data.sessions;
            break;
          } catch (e) {
            lastError = e;
          }
        }
        if (cancelled) return;
        if (sessions === null) {
          throw lastError instanceof Error
            ? lastError
            : new Error("Errore caricamento sessioni");
        }

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
        const persisted = loadPersisted(projectId);
        // Punto unico di riconciliazione (regola L): scarta tab stantie,
        // garantisce una tab aperta e un'attiva coerente.
        const { tabs, active } = reconcileSessionTabs(
          sessions.map((s) => s.id),
          persisted.tabs,
          persisted.active,
        );
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
  }, [projectId, persist, bootstrapNonce]);

  const retryBootstrap = useCallback(() => {
    setBootstrapNonce((n) => n + 1);
  }, []);

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
    try {
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
    } catch (e) {
      // Feedback esplicito: prima l'errore era silenzioso (void senza catch nel
      // chiamante) e la chat "sembrava non creata" senza alcuna indicazione.
      useProjectStore.getState().pushToast(
        "error",
        e instanceof Error ? `Creazione chat fallita: ${e.message}` : "Creazione chat fallita",
      );
      // Riconciliazione: se la POST e' riuscita lato server ma la risposta si
      // e' persa (rete), il refetch la fa comunque comparire nella lista invece
      // di lasciarla orfana sul DB fino al reload della pagina.
      void refreshSessions().catch(() => {});
    }
  }, [allSessions.length, projectId, persist, refreshSessions]);

  const renameSession = useCallback(async (id: string, title: string) => {
    await renameChatSession(id, title);
    setAllSessions((prev) => prev.map((s) => (s.id === id ? { ...s, title } : s)));
  }, []);

  // Pin provider/modello per-sessione (fix "pin perso al refresh"): la fonte di
  // verita' e' chat_sessions.preferred_provider/preferred_model lato server;
  // qui aggiornamento ottimistico di allSessions (cosi' la re-idratazione del
  // dropdown al cambio tab legge subito il valore nuovo) + PATCH in background.
  const setSessionPrefs = useCallback(
    (id: string, prefs: { preferredProvider?: string; preferredModel?: string }) => {
      const normalize = (v: string | undefined): string | null | undefined =>
        v === undefined ? undefined : (v.trim() === "" || v.trim().toLowerCase() === "auto" ? null : v.trim());
      const provider = normalize(prefs.preferredProvider);
      const model = normalize(prefs.preferredModel);
      setAllSessions((prev) =>
        prev.map((s) =>
          s.id === id
            ? {
                ...s,
                ...(provider !== undefined ? { preferredProvider: provider } : {}),
                ...(model !== undefined ? { preferredModel: model } : {}),
              }
            : s,
        ),
      );
      void updateChatSessionPrefs(id, prefs).catch((e) => {
        // Feedback esplicito: un PATCH fallito significa che il pin NON e'
        // persistito e al prossimo refresh tornerebbe al valore precedente.
        useProjectStore.getState().pushToast(
          "error",
          e instanceof Error
            ? `Pin provider non salvato: ${e.message}`
            : "Pin provider non salvato",
        );
      });
    },
    [],
  );

  const deleteSession = useCallback(async (id: string) => {
    await deleteChatSession(id);
    setAllSessions((prev) => prev.filter((s) => s.id !== id));
    closeTab(id);
  }, [closeTab]);

  const compactSession = useCallback(async (id: string) => {
    const store = useProjectStore.getState();
    try {
      const result = await compactChatSession(id);
      setAllSessions((prev) =>
        prev.map((s) => (s.id === id ? { ...s, status: "compacted" } : s)),
      );
      // Aggiorna la barra token SUBITO dalla risposta HTTP: non dipende
      // dall'evento SSE ChatSessionCompacted (che puo' perdersi se subscribers=0).
      store.recordChatCompaction(id, result.totalTokens, result.totalCostUsd);
      store.pushToast("success", "Chat compattata");
      return { summary: result.summary };
    } catch (e) {
      // Feedback esplicito: prima l'errore era silenzioso (void senza catch) e
      // la compattazione "sembrava fallita" anche quando il backend riusciva.
      store.pushToast(
        "error",
        e instanceof Error ? `Compattazione fallita: ${e.message}` : "Compattazione fallita",
      );
      throw e;
    }
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
    setSessionPrefs,
    deleteSession,
    compactSession,
    setAgentActive,
    setCtxRatio,
    refreshSessions,
    retryBootstrap,
  };
}
